use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use gunk_core::{ChangeStatus, Commit, CommitId, Identity, PathChange};
use thiserror::Error;
use time::OffsetDateTime;
use time::format_description::well_known::Iso8601;

/// Errors from git command execution.
#[derive(Debug, Error)]
pub enum GitError {
    #[error("git command failed with status {status}: {stderr}")]
    CommandFailed { status: i32, stderr: String },

    #[error("git command could not be started: {0}")]
    Spawn(#[from] std::io::Error),

    #[error("git produced invalid UTF-8 output")]
    InvalidUtf8,

    #[error("failed to parse git output: {0}")]
    Parse(String),

    #[error("not a git repository: {0}")]
    NotARepo(PathBuf),

    #[error("git binary not found; is git installed and on PATH?")]
    GitNotFound,
}

/// Result of a successful git invocation.
#[derive(Debug)]
pub struct GitOutput {
    pub stdout: String,
    pub stderr: String,
}

/// Information about a local branch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchInfo {
    pub name: String,
    pub oid: String,
    pub upstream: Option<String>,
}

/// Thin wrapper over the `git` binary. All IO goes through here.
#[derive(Debug, Clone)]
pub struct Git {
    /// Path to the repository root (the directory containing `.git`).
    repo_path: PathBuf,
    /// Path to the `git` binary. Defaults to "git" (resolved via PATH).
    git_binary: PathBuf,
}

impl Git {
    /// Create a new Git wrapper for the given repository path.
    pub fn open(repo_path: impl Into<PathBuf>) -> Result<Self, GitError> {
        let repo_path = repo_path.into();
        let git = Self {
            repo_path,
            git_binary: PathBuf::from("git"),
        };

        // Verify it's a git repo
        git.run(["rev-parse", "--git-dir"])?;
        Ok(git)
    }

    /// Create a Git wrapper without validating the repo (for init scenarios).
    pub fn at(repo_path: impl Into<PathBuf>) -> Self {
        Self {
            repo_path: repo_path.into(),
            git_binary: PathBuf::from("git"),
        }
    }

    /// Returns the repository root path.
    pub fn repo_path(&self) -> &Path {
        &self.repo_path
    }

    /// Run a git command with the given arguments, returning stdout/stderr.
    pub fn run<I, S>(&self, args: I) -> Result<GitOutput, GitError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let output = self.raw_run(args)?;

        if !output.status.success() {
            let stderr =
                String::from_utf8(output.stderr).unwrap_or_else(|_| "<non-utf8>".to_string());
            return Err(GitError::CommandFailed {
                status: output.status.code().unwrap_or(-1),
                stderr: stderr.trim().to_string(),
            });
        }

        let stdout = String::from_utf8(output.stdout).map_err(|_| GitError::InvalidUtf8)?;
        let stderr = String::from_utf8(output.stderr).unwrap_or_else(|_| String::new());

        Ok(GitOutput { stdout, stderr })
    }

    /// Run a git command returning raw `Output` (for cases where non-zero exit is expected).
    pub fn raw_run<I, S>(&self, args: I) -> Result<Output, GitError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        Command::new(&self.git_binary)
            .args(args)
            .current_dir(&self.repo_path)
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("LC_ALL", "C")
            .output()
            .map_err(|e| {
                if e.kind() == std::io::ErrorKind::NotFound {
                    GitError::GitNotFound
                } else {
                    GitError::Spawn(e)
                }
            })
    }

    /// Run a git command and return stdout as NUL-separated fields.
    pub fn run_nul_separated<I, S>(&self, args: I) -> Result<Vec<String>, GitError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let output = self.run(args)?;
        Ok(output
            .stdout
            .split('\0')
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .collect())
    }

    /// List all local branches.
    pub fn list_branches(&self) -> Result<Vec<BranchInfo>, GitError> {
        // Use %01 (SOH) as record separator, %00 (NUL) as field separator
        let output = self.run([
            "for-each-ref",
            "--format=%(refname:short)%00%(objectname)%00%(upstream:short)%01",
            "refs/heads",
        ])?;

        let mut branches = Vec::new();
        for record in output.stdout.split('\x01') {
            let record = record.trim();
            if record.is_empty() {
                continue;
            }
            let fields: Vec<&str> = record.split('\0').collect();
            if fields.len() < 2 {
                continue;
            }
            branches.push(BranchInfo {
                name: fields[0].to_string(),
                oid: fields[1].to_string(),
                upstream: fields
                    .get(2)
                    .filter(|s| !s.is_empty())
                    .map(|s| s.to_string()),
            });
        }
        Ok(branches)
    }

    /// Walk a branch's history and return commits (newest first).
    ///
    /// Parses all commits reachable from `branch`, including merges.
    pub fn walk_commits(&self, branch: &str) -> Result<Vec<Commit>, GitError> {
        // Fields separated by %x00, records by %x01
        // Fields: hash, parents, author_name, author_email, author_date,
        //         committer_name, committer_email, committer_date, subject, body
        let output = self.run([
            "log",
            branch,
            "--pretty=format:%H%x00%P%x00%an%x00%ae%x00%aI%x00%cn%x00%ce%x00%cI%x00%s%x00%b%x01",
        ])?;

        let mut commits = Vec::new();
        for record in output.stdout.split('\x01') {
            let record = record.trim();
            if record.is_empty() {
                continue;
            }
            let fields: Vec<&str> = record.splitn(10, '\0').collect();
            if fields.len() < 10 {
                return Err(GitError::Parse(format!(
                    "expected 10 fields, got {}: {:?}",
                    fields.len(),
                    record.chars().take(120).collect::<String>()
                )));
            }

            let parents = if fields[1].is_empty() {
                Vec::new()
            } else {
                fields[1]
                    .split(' ')
                    .map(|s| CommitId(s.to_string()))
                    .collect()
            };

            let author_time = parse_iso8601(fields[4])?;
            let committer_time = parse_iso8601(fields[7])?;

            commits.push(Commit {
                id: CommitId(fields[0].to_string()),
                parents,
                author: Identity {
                    name: fields[2].to_string(),
                    email: fields[3].to_string(),
                    time: author_time,
                },
                committer: Identity {
                    name: fields[5].to_string(),
                    email: fields[6].to_string(),
                    time: committer_time,
                },
                summary: fields[8].to_string(),
                body: fields[9].trim().to_string(),
                changed_paths: Vec::new(), // loaded lazily via changed_paths()
            });
        }
        Ok(commits)
    }

    /// Get the changed file paths for a single commit.
    ///
    /// For root commits (no parents), diffs against the empty tree.
    pub fn changed_paths(&self, oid: &str) -> Result<Vec<PathChange>, GitError> {
        let output = self.run([
            "diff-tree",
            "--no-commit-id",
            "--name-status",
            "-r",
            "--root", // handles root commits (diff vs empty tree)
            "-z",
            oid,
        ])?;

        parse_name_status_z(&output.stdout)
    }

    /// Get the diff patch for a single commit (lazy-loaded detail).
    pub fn show_diff(&self, oid: &str) -> Result<String, GitError> {
        // --format= suppresses the commit header, -p shows the patch
        // --root makes it work for the initial commit
        let output = self.run(["show", "--format=", "-p", "--root", oid])?;
        Ok(output.stdout)
    }
}

/// Parse an ISO-8601 timestamp from git output.
fn parse_iso8601(s: &str) -> Result<OffsetDateTime, GitError> {
    OffsetDateTime::parse(s, &Iso8601::DEFAULT)
        .map_err(|e| GitError::Parse(format!("invalid timestamp '{s}': {e}")))
}

/// Parse NUL-delimited name-status output from `git diff-tree -z`.
///
/// Format: `<status>\0<path>\0<status>\0<path>\0...`
fn parse_name_status_z(raw: &str) -> Result<Vec<PathChange>, GitError> {
    let parts: Vec<&str> = raw.split('\0').collect();
    let mut changes = Vec::new();
    let mut i = 0;
    while i + 1 < parts.len() {
        let status_str = parts[i].trim();
        let path = parts[i + 1];
        if status_str.is_empty() && path.is_empty() {
            i += 2;
            continue;
        }
        if status_str.is_empty() {
            i += 1;
            continue;
        }
        let status = match status_str.chars().next() {
            Some('A') => ChangeStatus::Added,
            Some('M') => ChangeStatus::Modified,
            Some('D') => ChangeStatus::Deleted,
            Some('R') => ChangeStatus::Renamed,
            Some('C') => ChangeStatus::Copied,
            Some('T') => ChangeStatus::TypeChange,
            _ => ChangeStatus::Unknown,
        };
        // For rename/copy, there's an extra path (old path) — skip it
        if matches!(status, ChangeStatus::Renamed | ChangeStatus::Copied) && i + 2 < parts.len() {
            changes.push(PathChange {
                status,
                path: parts[i + 2].to_string(),
            });
            i += 3;
        } else {
            changes.push(PathChange {
                status,
                path: path.to_string(),
            });
            i += 2;
        }
    }
    Ok(changes)
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_nonexistent_dir_fails() {
        let result = Git::open("/tmp/definitely_not_a_real_git_repo_xyz_123");
        assert!(result.is_err());
    }
}

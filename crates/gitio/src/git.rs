use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use thiserror::Error;

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

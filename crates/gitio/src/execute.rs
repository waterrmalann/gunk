//! Execution engine — applies an `ExecutionPlan` to a real repository.
//!
//! Implements the safety protocol:
//! 1. Refuse on dirty tree (offer stash).
//! 2. Create a backup ref before mutation.
//! 3. Rehearse in a throwaway worktree.
//! 4. Apply only on successful rehearsal.
//! 5. RAII cleanup of worktrees.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use gunk_core::{CommitId, RebaseTodo, RebaseTodoLine};
use thiserror::Error;

use crate::git::{Git, GitError};

// ── Error types ────────────────────────────────────────────────────

/// Errors from the execution engine.
#[derive(Debug, Error)]
pub enum ExecuteError {
    #[error("working tree is dirty; stash or commit changes first")]
    DirtyTree,

    #[error("git error: {0}")]
    Git(#[from] GitError),

    #[error("rehearsal failed: {0}")]
    RehearsalFailed(String),

    #[error("rebase conflict during rehearsal: {0}")]
    RebaseConflict(String),

    #[error("backup ref could not be created: {0}")]
    BackupFailed(String),

    #[error("worktree setup failed: {0}")]
    WorktreeFailed(String),

    #[error("unsupported plan type for execution: {0}")]
    Unsupported(String),
}

// ── Result of execution ────────────────────────────────────────────

/// Summary of a successful plan execution.
#[derive(Debug, Clone)]
pub struct ExecuteResult {
    /// The backup ref that was created before mutation.
    pub backup_ref: String,
    /// The new branch tip after execution.
    pub new_tip: String,
    /// The branch that was rewritten.
    pub branch: String,
    /// Commits that were reachable from remote-tracking refs (pushed history warning).
    pub pushed_commits: Vec<String>,
}

// ── Safety checks ──────────────────────────────────────────────────

/// Check whether the working tree is clean (no uncommitted changes).
pub fn check_clean(git: &Git) -> Result<(), ExecuteError> {
    let output = git.run(["status", "--porcelain=v2", "-z"])?;
    if !output.stdout.trim().is_empty() {
        return Err(ExecuteError::DirtyTree);
    }
    Ok(())
}

/// Stash uncommitted changes. Returns `true` if something was stashed.
pub fn stash_push(git: &Git) -> Result<bool, ExecuteError> {
    let before = git.run(["stash", "list"])?;
    git.run([
        "stash",
        "push",
        "-u",
        "-m",
        "gunk: auto-stash before rewrite",
    ])?;
    let after = git.run(["stash", "list"])?;
    // If the list grew, we stashed something.
    Ok(after.stdout.lines().count() > before.stdout.lines().count())
}

/// Pop the most recent stash.
pub fn stash_pop(git: &Git) -> Result<(), ExecuteError> {
    git.run(["stash", "pop"])?;
    Ok(())
}

// ── Backup refs ────────────────────────────────────────────────────

/// Create a backup ref for the current branch tip.
///
/// Format: `refs/gunk/backup/<branch>/<unix-timestamp>`
pub fn create_backup_ref(git: &Git, branch: &str) -> Result<String, ExecuteError> {
    let tip = git.run(["rev-parse", branch])?.stdout.trim().to_string();

    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let ref_name = format!("refs/gunk/backup/{branch}/{ts}");
    git.run(["update-ref", &ref_name, &tip])?;

    Ok(ref_name)
}

/// List backup refs for a branch, newest first.
pub fn list_backup_refs(git: &Git, branch: &str) -> Result<Vec<(String, String)>, ExecuteError> {
    let prefix = format!("refs/gunk/backup/{branch}/");
    let output = git.run([
        "for-each-ref",
        "--format=%(refname)%00%(objectname)%01",
        &format!("refs/gunk/backup/{branch}"),
    ])?;

    let mut refs: Vec<(String, String)> = Vec::new();
    for record in output.stdout.split('\x01') {
        let record = record.trim();
        if record.is_empty() {
            continue;
        }
        let fields: Vec<&str> = record.split('\0').collect();
        if fields.len() >= 2 {
            refs.push((fields[0].to_string(), fields[1].to_string()));
        }
    }

    // Sort by timestamp suffix descending (newest first).
    refs.sort_by(|a, b| {
        let ts_a = a.0.strip_prefix(&prefix).unwrap_or("0");
        let ts_b = b.0.strip_prefix(&prefix).unwrap_or("0");
        ts_b.cmp(ts_a)
    });

    Ok(refs)
}

/// Restore a branch to a backup ref.
pub fn restore_backup(git: &Git, branch: &str, backup_ref: &str) -> Result<(), ExecuteError> {
    let oid = git
        .run(["rev-parse", backup_ref])?
        .stdout
        .trim()
        .to_string();

    git.run(["update-ref", &format!("refs/heads/{branch}"), &oid])?;

    // If we're on this branch, reset working tree to match.
    let current = git
        .run(["symbolic-ref", "--short", "HEAD"])
        .ok()
        .map(|o| o.stdout.trim().to_string());

    if current.as_deref() == Some(branch) {
        git.run(["reset", "--hard", &oid])?;
    }

    Ok(())
}

// ── Worktree RAII guard ────────────────────────────────────────────

/// RAII guard that removes a worktree on drop.
pub struct WorktreeGuard<'a> {
    git: &'a Git,
    pub worktree_path: PathBuf,
    removed: bool,
}

impl<'a> WorktreeGuard<'a> {
    /// Add a detached worktree at `path` pointing to `commitish`.
    pub fn new(git: &'a Git, path: PathBuf, commitish: &str) -> Result<Self, ExecuteError> {
        git.run([
            "worktree",
            "add",
            "--detach",
            path.to_str().unwrap_or(""),
            commitish,
        ])
        .map_err(|e| ExecuteError::WorktreeFailed(e.to_string()))?;

        Ok(Self {
            git,
            worktree_path: path,
            removed: false,
        })
    }

    /// Explicitly remove the worktree (also called on drop).
    pub fn remove(&mut self) -> Result<(), ExecuteError> {
        if !self.removed {
            self.removed = true;
            let _ = self.git.run([
                "worktree",
                "remove",
                "--force",
                self.worktree_path.to_str().unwrap_or(""),
            ]);
        }
        Ok(())
    }

    /// Get a `Git` instance pointing at the worktree.
    pub fn git(&self) -> Git {
        Git::at(&self.worktree_path)
    }
}

impl<'a> Drop for WorktreeGuard<'a> {
    fn drop(&mut self) {
        let _ = self.remove();
    }
}

// ── Rebase execution ───────────────────────────────────────────────

/// Format a `RebaseTodo` into the text format git expects.
pub fn format_rebase_todo(todo: &RebaseTodo) -> String {
    let mut lines = Vec::new();
    for line in &todo.lines {
        match line {
            RebaseTodoLine::Pick(id) => lines.push(format!("pick {}", id.0)),
            RebaseTodoLine::Reword(id) => lines.push(format!("reword {}", id.0)),
            RebaseTodoLine::Squash(id) => lines.push(format!("squash {}", id.0)),
            RebaseTodoLine::Fixup(id) => lines.push(format!("fixup {}", id.0)),
            RebaseTodoLine::Drop(id) => lines.push(format!("drop {}", id.0)),
            RebaseTodoLine::Exec(cmd) => lines.push(format!("exec {cmd}")),
        }
    }
    lines.join("\n") + "\n"
}

/// Execute a rebase plan against a branch.
///
/// This is the core of the execution engine. It:
/// 1. Checks for dirty tree.
/// 2. Creates a backup ref.
/// 3. Rehearses in a worktree.
/// 4. Applies to the real branch on success.
pub fn execute_rebase(
    git: &Git,
    branch: &str,
    todo: &RebaseTodo,
) -> Result<ExecuteResult, ExecuteError> {
    // 1. Dirty tree check.
    check_clean(git)?;

    // 2. Backup ref.
    let backup_ref = create_backup_ref(git, branch)?;

    // 3. Rehearse in a worktree.
    let worktree_path = git.repo_path().join(".gunk-rehearsal");
    // Clean up any stale worktree from a previous crash.
    if worktree_path.exists() {
        let _ = git.run([
            "worktree",
            "remove",
            "--force",
            worktree_path.to_str().unwrap_or(""),
        ]);
        if worktree_path.exists() {
            let _ = std::fs::remove_dir_all(&worktree_path);
        }
    }

    let mut guard = WorktreeGuard::new(git, worktree_path, branch)?;
    let wt_git = guard.git();

    // Run the rebase in the worktree.
    let rebase_result = run_rebase_in(&wt_git, todo);

    match rebase_result {
        Ok(new_tip) => {
            // 4. Success: update the real branch ref to the rehearsed result.
            guard.remove()?;
            git.run(["update-ref", &format!("refs/heads/{branch}"), &new_tip])?;

            // If we're on this branch, reset working tree.
            let current = git
                .run(["symbolic-ref", "--short", "HEAD"])
                .ok()
                .map(|o| o.stdout.trim().to_string());
            if current.as_deref() == Some(branch) {
                git.run(["reset", "--hard", &new_tip])?;
            }

            // 5. Push warning: check if any rewritten commits were published.
            let commit_ids: Vec<&str> = todo
                .lines
                .iter()
                .filter_map(|line| match line {
                    RebaseTodoLine::Pick(id)
                    | RebaseTodoLine::Reword(id)
                    | RebaseTodoLine::Squash(id)
                    | RebaseTodoLine::Fixup(id)
                    | RebaseTodoLine::Drop(id) => Some(id.0.as_str()),
                    RebaseTodoLine::Exec(_) => None,
                })
                .collect();
            let pushed_commits = check_pushed_commits(git, &commit_ids).unwrap_or_default();

            Ok(ExecuteResult {
                backup_ref,
                new_tip,
                branch: branch.to_string(),
                pushed_commits,
            })
        }
        Err(e) => {
            // Abort any in-progress rebase in the worktree.
            let _ = wt_git.run(["rebase", "--abort"]);
            guard.remove()?;
            // Real branch is untouched.
            Err(e)
        }
    }
}

/// Build the rebase todo text with message feeding via `exec` lines.
///
/// When a `Reword` line has a corresponding message in `message_map`, it is
/// converted to a `Pick` followed by `exec git commit --amend -F <path>`.
/// This avoids needing a custom `GIT_EDITOR` for message feeding.
///
/// Returns `(todo_text, message_files)` where `message_files` contains
/// `(path, content)` pairs that must be written to disk before the rebase.
fn build_rebase_text(
    todo: &RebaseTodo,
    msg_dir: &std::path::Path,
) -> (String, Vec<(PathBuf, String)>) {
    let msg_lookup: HashMap<&CommitId, &str> = todo
        .message_map
        .iter()
        .map(|(id, msg)| (id, msg.as_str()))
        .collect();

    let mut output_lines: Vec<String> = Vec::new();
    let mut msg_files: Vec<(PathBuf, String)> = Vec::new();
    let mut pending_msg: Option<&str> = None;

    for line in &todo.lines {
        // Group-continuation lines (squash/fixup/exec) do not trigger a flush
        // of the pending message exec. Boundary lines (pick/reword/drop) do.
        let is_group_continuation = matches!(
            line,
            RebaseTodoLine::Squash(_) | RebaseTodoLine::Fixup(_) | RebaseTodoLine::Exec(_)
        );

        if !is_group_continuation {
            flush_pending_message(&mut pending_msg, &mut output_lines, &mut msg_files, msg_dir);
        }

        match line {
            RebaseTodoLine::Reword(id) => {
                if let Some(&msg) = msg_lookup.get(id) {
                    // Convert reword → pick; the message is fed via exec.
                    output_lines.push(format!("pick {}", id.0));
                    pending_msg = Some(msg);
                } else {
                    output_lines.push(format!("reword {}", id.0));
                }
            }
            RebaseTodoLine::Pick(id) => output_lines.push(format!("pick {}", id.0)),
            RebaseTodoLine::Squash(id) => output_lines.push(format!("squash {}", id.0)),
            RebaseTodoLine::Fixup(id) => output_lines.push(format!("fixup {}", id.0)),
            RebaseTodoLine::Drop(id) => output_lines.push(format!("drop {}", id.0)),
            RebaseTodoLine::Exec(cmd) => output_lines.push(format!("exec {cmd}")),
        }
    }

    // Flush any trailing pending message.
    flush_pending_message(&mut pending_msg, &mut output_lines, &mut msg_files, msg_dir);

    (output_lines.join("\n") + "\n", msg_files)
}

/// If there is a pending message, emit an `exec git commit --amend -F <path>`
/// line and record the file to be written.
///
/// Uses a relative filename (`.gunk-msg-N.txt`) so the exec line is immune to
/// special characters in the repository's absolute path.
fn flush_pending_message(
    pending: &mut Option<&str>,
    output_lines: &mut Vec<String>,
    msg_files: &mut Vec<(PathBuf, String)>,
    msg_dir: &std::path::Path,
) {
    if let Some(msg) = pending.take() {
        let idx = msg_files.len();
        // Relative filename — no special-character issues in exec lines.
        let file_name = format!(".gunk-msg-{idx}.txt");
        let file_path = msg_dir.join(&file_name);
        output_lines.push(format!("exec git commit --amend -F '{file_name}'"));
        msg_files.push((file_path, msg.to_string()));
    }
}

/// Run a non-interactive rebase inside a git worktree using GIT_SEQUENCE_EDITOR.
///
/// Cross-platform: writes a small helper script that copies our prepared todo
/// over the file git passes as $1 / %1.
fn run_rebase_in(git: &Git, todo: &RebaseTodo) -> Result<String, ExecuteError> {
    // Build the todo text with message feeding via exec lines.
    let (todo_content, msg_files) = build_rebase_text(todo, git.repo_path());

    // Write message files to disk.
    for (path, content) in &msg_files {
        std::fs::write(path, content).map_err(|e| {
            ExecuteError::RehearsalFailed(format!("failed to write message file: {e}"))
        })?;
    }

    // Write todo to a temp file in the worktree.
    let todo_path = git.repo_path().join(".gunk-rebase-todo");
    std::fs::write(&todo_path, &todo_content)
        .map_err(|e| ExecuteError::RehearsalFailed(format!("failed to write todo: {e}")))?;

    // Build the base argument for rebase.
    let base_arg = match &todo.base {
        Some(base) => base.0.clone(),
        None => "--root".to_string(),
    };

    // Create a helper script that copies our todo over git's todo file.
    // This is the cross-platform "sequence editor" approach from the plan.
    let (script_path, seq_editor) = create_seq_editor_script(git.repo_path(), &todo_path)?;

    // Message editor: `true` (no-op). Actual message changes are handled via
    // `exec git commit --amend -F <path>` lines in the todo.
    let msg_editor = "true";

    // Run the rebase.
    let result = std::process::Command::new(git.git_binary())
        .args(["rebase", "-i", &base_arg])
        .current_dir(git.repo_path())
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("LC_ALL", "C")
        .env("GIT_SEQUENCE_EDITOR", &seq_editor)
        .env("GIT_EDITOR", msg_editor)
        .output()
        .map_err(|e| ExecuteError::Git(GitError::Spawn(e)))?;

    // Clean up temp files.
    let _ = std::fs::remove_file(&todo_path);
    let _ = std::fs::remove_file(&script_path);
    for (path, _) in &msg_files {
        let _ = std::fs::remove_file(path);
    }

    if !result.status.success() {
        let stderr = String::from_utf8_lossy(&result.stderr);
        let stdout = String::from_utf8_lossy(&result.stdout);
        let combined = format!("{stdout}\n{stderr}");

        if combined.contains("CONFLICT") || combined.contains("could not apply") {
            return Err(ExecuteError::RebaseConflict(combined.trim().to_string()));
        }
        return Err(ExecuteError::RehearsalFailed(combined.trim().to_string()));
    }

    // Get the new tip.
    let new_tip = git.run(["rev-parse", "HEAD"])?.stdout.trim().to_string();

    Ok(new_tip)
}

/// Create a platform-appropriate script that copies our todo file over git's.
///
/// Git for Windows uses its internal MSYS2 bash to execute editors, so we
/// always write a shell script with forward-slash paths.
///
/// Returns `(script_path, editor_command_string)`.
fn create_seq_editor_script(
    repo_path: &std::path::Path,
    todo_path: &std::path::Path,
) -> Result<(PathBuf, String), ExecuteError> {
    let script_path = repo_path.join(".gunk-seq-editor.sh");
    // Convert Windows backslashes to forward slashes for MSYS2 compatibility.
    let todo_str = todo_path.to_str().unwrap_or("").replace('\\', "/");
    let content = format!("#!/bin/sh\ncp '{}' \"$1\"\n", todo_str);
    std::fs::write(&script_path, &content)
        .map_err(|e| ExecuteError::RehearsalFailed(format!("failed to write script: {e}")))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o755);
        std::fs::set_permissions(&script_path, perms).ok();
    }

    // For the editor command, also use forward slashes.
    let editor_cmd = script_path.to_str().unwrap_or("").replace('\\', "/");
    Ok((script_path, editor_cmd))
}

/// Check if commits are reachable from any remote-tracking ref (pushed history warning).
pub fn check_pushed_commits(git: &Git, commits: &[&str]) -> Result<Vec<String>, ExecuteError> {
    let mut pushed = Vec::new();

    // Get all remote-tracking refs.
    let output = git.run(["for-each-ref", "--format=%(refname)", "refs/remotes"])?;
    let remote_refs: Vec<&str> = output.stdout.lines().filter(|l| !l.is_empty()).collect();

    if remote_refs.is_empty() {
        return Ok(pushed);
    }

    for &commit in commits {
        for remote_ref in &remote_refs {
            let result = git.run(["merge-base", "--is-ancestor", commit, remote_ref]);
            if result.is_ok() {
                pushed.push(commit.to_string());
                break;
            }
        }
    }

    Ok(pushed)
}

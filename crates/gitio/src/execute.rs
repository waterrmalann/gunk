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

use gunk_core::{
    CommitId, FilterRepoSpec, FlattenSpec, OidMap, RebaseTodo, RebaseTodoLine, compose_oid_maps,
};
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

    #[error("git-filter-repo failed: {0}")]
    FilterRepoFailed(String),

    #[error("git-filter-repo is not installed")]
    FilterRepoNotInstalled,

    #[error("could not retarget plan onto rewritten history: {0}")]
    Remap(#[from] gunk_core::PlanError),
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
    /// Maps pre-execution commit ids to post-execution ids (`None` = dropped).
    ///
    /// Populated by history-rewriting phases (flatten, filter-repo) so a
    /// composite can retarget later phases onto the rewritten history. Rebase
    /// leaves this empty: composite ordering guarantees no phase follows it.
    pub oid_map: OidMap,
}

// ── Path / shell helpers ───────────────────────────────────────────

/// Render a path as UTF-8, returning a clear error instead of silently
/// substituting an empty string when the path is not representable.
fn path_str(p: &std::path::Path) -> Result<&str, ExecuteError> {
    p.to_str().ok_or_else(|| {
        ExecuteError::WorktreeFailed(format!("path is not valid UTF-8: {}", p.display()))
    })
}

/// Escape a string for safe embedding inside single quotes in a POSIX shell.
///
/// Closes the quote, inserts an escaped literal quote, and reopens — the
/// standard `'\''` idiom — so paths containing `'` cannot break out.
fn sh_single_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
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
/// Format: `refs/gunk/backup/<branch>/<unix-millis>`
///
/// A backup is the only recovery point for a rewrite, so it must never silently
/// overwrite an existing one. Millisecond resolution makes same-instant
/// collisions rare; the loop guarantees a unique name even when two rewrites of
/// the same branch land in the same millisecond, so no recovery point is lost.
pub fn create_backup_ref(git: &Git, branch: &str) -> Result<String, ExecuteError> {
    let tip = git.run(["rev-parse", branch])?.stdout.trim().to_string();

    let mut suffix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();

    // Bounded search for an unused ref name. The bound is a safety valve: in
    // practice the first candidate is almost always free.
    for _ in 0..1_000_000 {
        let ref_name = format!("refs/gunk/backup/{branch}/{suffix}");
        // `rev-parse --verify --quiet` exits non-zero (→ Err) when the ref does
        // not exist, which is exactly the name we want to claim.
        let exists = git
            .run(["rev-parse", "--verify", "--quiet", &ref_name])
            .is_ok();
        if !exists {
            git.run(["update-ref", &ref_name, &tip])?;
            return Ok(ref_name);
        }
        suffix += 1;
    }

    Err(ExecuteError::BackupFailed(format!(
        "could not find an unused backup ref name under refs/gunk/backup/{branch}/"
    )))
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

    // Sort by timestamp suffix descending (newest first). Parse the suffix as a
    // number so ordering stays correct across suffix widths (e.g. legacy
    // second-resolution refs alongside millisecond ones); a lexicographic
    // compare would rank a 10-digit value above a 13-digit one.
    let parse_suffix = |full: &str| {
        full.strip_prefix(&prefix)
            .and_then(|s| s.parse::<u128>().ok())
            .unwrap_or(0)
    };
    refs.sort_by(|a, b| parse_suffix(&b.0).cmp(&parse_suffix(&a.0)));

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
        git.run(["worktree", "add", "--detach", path_str(&path)?, commitish])
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
                // Rebase is always the final composite phase, so its rewrite
                // map is never consumed; leave it empty.
                oid_map: OidMap::new(),
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
    let todo_str = path_str(todo_path)?.replace('\\', "/");
    let content = format!("#!/bin/sh\ncp {} \"$1\"\n", sh_single_quote(&todo_str));
    std::fs::write(&script_path, &content)
        .map_err(|e| ExecuteError::RehearsalFailed(format!("failed to write script: {e}")))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o755);
        std::fs::set_permissions(&script_path, perms).ok();
    }

    // For the editor command, also use forward slashes.
    let editor_cmd = path_str(&script_path)?.replace('\\', "/");
    Ok((script_path, editor_cmd))
}

// ── Flatten execution ──────────────────────────────────────────────

/// Execute a flatten plan against a branch.
///
/// This replaces a merge commit with a single ordinary commit that:
/// - Has the exact same tree as the merge commit (byte-identical result).
/// - Has a single parent (the mainline parent).
///
/// Then rebases all descendants of the merge onto the new commit.
///
/// Follows the same safety protocol: dirty-tree check, backup ref,
/// worktree rehearsal, apply-on-success.
pub fn execute_flatten(
    git: &Git,
    branch: &str,
    spec: &FlattenSpec,
) -> Result<ExecuteResult, ExecuteError> {
    // 1. Dirty tree check.
    check_clean(git)?;

    // 2. Backup ref.
    let backup_ref = create_backup_ref(git, branch)?;

    // 3. Rehearse in a worktree.
    let worktree_path = git.repo_path().join(".gunk-rehearsal");
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

    let flatten_result = run_flatten_in(&wt_git, spec, branch);

    match flatten_result {
        Ok((new_tip, oid_map)) => {
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

            let pushed_commits = check_pushed_commits(git, &[&spec.merge.0]).unwrap_or_default();

            Ok(ExecuteResult {
                backup_ref,
                new_tip,
                branch: branch.to_string(),
                pushed_commits,
                oid_map,
            })
        }
        Err(e) => {
            guard.remove()?;
            Err(e)
        }
    }
}

/// Run the flatten operation inside a worktree.
///
/// 1. Get the merge commit's tree.
/// 2. Create a new ordinary commit with that tree, parented on mainline.
/// 3. Rebase all descendants onto the new commit.
///
/// Returns the new branch tip and an [`OidMap`] from pre-flatten ids to their
/// post-flatten ids (the merge and every descendant change id; ancestors are
/// unchanged and omitted).
fn run_flatten_in(
    git: &Git,
    spec: &FlattenSpec,
    _branch: &str,
) -> Result<(String, OidMap), ExecuteError> {
    // Get the merge commit's tree (T = M^{tree}).
    let tree = git
        .run(["rev-parse", &format!("{}^{{tree}}", spec.merge.0)])?
        .stdout
        .trim()
        .to_string();

    // Create a new commit reusing the merge tree, parented on mainline:
    // git commit-tree T -p P1 -m "<message>" → M'
    let new_commit = git
        .run([
            "commit-tree",
            &tree,
            "-p",
            &spec.mainline_parent.0,
            "-m",
            &spec.message,
        ])?
        .stdout
        .trim()
        .to_string();

    // Check if the merge is the branch tip — if so, just point HEAD at M'.
    let branch_tip = git.run(["rev-parse", "HEAD"])?.stdout.trim().to_string();

    if branch_tip == spec.merge.0 {
        // The merge is the tip; no descendants to rebase.
        git.run(["checkout", &new_commit])?;
        let mut map = OidMap::new();
        map.insert(spec.merge.clone(), Some(CommitId(new_commit.clone())));
        return Ok((new_commit, map));
    }

    // Rebase everything after the merge onto M'.
    // Use HEAD (detached) rather than the branch name, since the branch
    // is checked out in the original worktree.
    let result = std::process::Command::new(git.git_binary())
        .args(["rebase", "--onto", &new_commit, &spec.merge.0, "HEAD"])
        .current_dir(git.repo_path())
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("LC_ALL", "C")
        .output()
        .map_err(|e| ExecuteError::Git(GitError::Spawn(e)))?;

    if !result.status.success() {
        let stderr = String::from_utf8_lossy(&result.stderr);
        let stdout = String::from_utf8_lossy(&result.stdout);
        let combined = format!("{stdout}\n{stderr}");

        // Abort any in-progress rebase.
        let _ = std::process::Command::new(git.git_binary())
            .args(["rebase", "--abort"])
            .current_dir(git.repo_path())
            .output();

        if combined.contains("CONFLICT") || combined.contains("could not apply") {
            return Err(ExecuteError::RebaseConflict(combined.trim().to_string()));
        }
        return Err(ExecuteError::RehearsalFailed(combined.trim().to_string()));
    }

    // Get the new tip.
    let new_tip = git.run(["rev-parse", "HEAD"])?.stdout.trim().to_string();

    // Build a map from each old commit id to its new one. The merge maps to M';
    // each descendant maps to its rebased copy. We get the descendants from two
    // `rev-list` calls (old range and new range, both newest-first) and pair
    // them up by position.
    let mut map = OidMap::new();
    map.insert(spec.merge.clone(), Some(CommitId(new_commit.clone())));

    let old_desc = git.run(["rev-list", &format!("{}..{}", spec.merge.0, branch_tip)])?;
    let old_ids: Vec<&str> = old_desc.stdout.lines().filter(|l| !l.is_empty()).collect();
    let new_desc = git.run(["rev-list", &format!("{new_commit}..{new_tip}")])?;
    let new_ids: Vec<&str> = new_desc.stdout.lines().filter(|l| !l.is_empty()).collect();

    // Pairing by position only works if the two lists line up one-to-one. They
    // don't if the descendants contain another merge: rebase drops that merge
    // and shuffles the surrounding commits, so the lists differ in length and
    // order. (A descendant commit that rebases to nothing causes the same skew.)
    //
    // Guessing here would map operations onto the wrong commits. Instead, when
    // we can't pair safely, record every descendant as dropped (`None`). A plain
    // flatten never reads this map, so it still works. But a follow-up phase in
    // a composite that tries to touch one of these commits gets a clean
    // `CommitNotFound` error and rolls back — far better than a silent rewrite.
    let descendant_merges = git.run([
        "rev-list",
        "--merges",
        &format!("{}..{}", spec.merge.0, branch_tip),
    ])?;
    let pairing_reliable =
        descendant_merges.stdout.trim().is_empty() && old_ids.len() == new_ids.len();

    if pairing_reliable {
        for (old, new) in old_ids.iter().zip(new_ids.iter()) {
            map.insert(CommitId(old.to_string()), Some(CommitId(new.to_string())));
        }
    } else {
        for old in &old_ids {
            map.insert(CommitId(old.to_string()), None);
        }
    }

    Ok((new_tip, map))
}

// ── git-filter-repo detection ──────────────────────────────────────

/// Check whether `git-filter-repo` is available.
///
/// Returns `true` if `git filter-repo --version` succeeds, `false` otherwise.
pub fn has_filter_repo(git: &Git) -> bool {
    git.run(["filter-repo", "--version"]).is_ok()
}

// ── filter-repo execution ──────────────────────────────────────────

/// Execute a filter-repo plan against a branch.
///
/// Follows the same rehearse-then-apply safety protocol as `execute_rebase`:
/// 1. Check for dirty tree.
/// 2. Create a backup ref.
/// 3. Rehearse the rewrite in an isolated throwaway clone.
/// 4. Apply only on success by fetching the rewritten tip back.
///
/// Unlike rebase/flatten, the rehearsal uses a full clone rather than a linked
/// worktree: `filter-repo` rewrites the shared object store and rewrites refs by
/// name, so a worktree (which shares both) cannot isolate it. The clone gives
/// `filter-repo` a private object store to mutate; the real repo is untouched
/// until the rewritten objects are fetched back and the branch ref is updated.
pub fn execute_filter_repo(
    git: &Git,
    branch: &str,
    spec: &FilterRepoSpec,
) -> Result<ExecuteResult, ExecuteError> {
    // 1. Dirty tree check.
    check_clean(git)?;

    // 2. Backup ref.
    let backup_ref = create_backup_ref(git, branch)?;

    // 3. Rehearse in an isolated clone. Clean up any stale rehearsal dir first.
    let clone_path = git.repo_path().join(".gunk-filter-rehearsal");
    if clone_path.exists() {
        let _ = std::fs::remove_dir_all(&clone_path);
    }

    let rehearsal = rehearse_filter_repo(git, branch, spec, &clone_path);

    // Always remove the throwaway clone, regardless of outcome.
    let _ = std::fs::remove_dir_all(&clone_path);

    // On rehearsal failure the real repo is pristine — nothing to restore.
    let (new_tip, oid_map) = rehearsal?;

    // 4. Apply: point the real branch at the rewritten tip (objects already
    //    fetched into the real repo during rehearsal).
    git.run(["update-ref", &format!("refs/heads/{branch}"), &new_tip])?;

    let current = git
        .run(["symbolic-ref", "--short", "HEAD"])
        .ok()
        .map(|o| o.stdout.trim().to_string());
    if current.as_deref() == Some(branch) {
        git.run(["reset", "--hard", &new_tip])?;
    }

    // Push warning: compare against the pre-rewrite tip.
    let old_tip = git
        .run(["rev-parse", &backup_ref])
        .map(|o| o.stdout.trim().to_string())
        .unwrap_or_default();
    let pushed_commits = check_pushed_commits(git, &[&old_tip]).unwrap_or_default();

    Ok(ExecuteResult {
        backup_ref,
        new_tip,
        branch: branch.to_string(),
        pushed_commits,
        oid_map,
    })
}

/// Rehearse a filter-repo rewrite in an isolated clone and fetch the rewritten
/// tip back into the real repo. Returns the new branch tip OID and the
/// original→rewritten id map read from filter-repo's `commit-map`.
///
/// The real repo's refs are never modified here; only objects are added by the
/// final fetch. The caller is responsible for updating the branch ref.
fn rehearse_filter_repo(
    git: &Git,
    branch: &str,
    spec: &FilterRepoSpec,
    clone_path: &std::path::Path,
) -> Result<(String, OidMap), ExecuteError> {
    // Clone just the target branch with real object copies (no hardlinks, so
    // filter-repo's repack/gc in the clone can never touch the source's packs).
    git.run([
        "clone",
        "--no-hardlinks",
        "--single-branch",
        "--branch",
        branch,
        path_str(git.repo_path())?,
        path_str(clone_path)?,
    ])
    .map_err(|e| ExecuteError::FilterRepoFailed(format!("rehearsal clone failed: {e}")))?;

    let clone_git = Git::at(clone_path.to_path_buf());

    // Build filter-repo arguments. The clone has only this branch, so no --refs
    // scoping is needed; --force is required because the clone has reflogs.
    let mut args: Vec<String> = vec![
        "filter-repo".to_string(),
        "--invert-paths".to_string(),
        "--force".to_string(),
    ];
    for path in &spec.paths {
        // Use --path-glob for patterns with wildcards, --path for exact paths.
        if path.0.contains('*') || path.0.contains('?') || path.0.contains('[') {
            args.push("--path-glob".to_string());
        } else {
            args.push("--path".to_string());
        }
        args.push(path.0.clone());
    }

    clone_git
        .run(args.iter().map(|s| s.as_str()))
        .map_err(|e| ExecuteError::FilterRepoFailed(e.to_string()))?;

    // Read filter-repo's authoritative old→new map before adding anything on
    // top. Commits dropped by the filter map to an all-zero id.
    let commit_map_path = clone_path.join(".git/filter-repo/commit-map");
    let oid_map = std::fs::read_to_string(&commit_map_path)
        .map(|text| parse_commit_map(&text))
        .map_err(|e| {
            ExecuteError::FilterRepoFailed(format!("could not read filter-repo commit-map: {e}"))
        })?;

    // Optionally append to .gitignore as part of the rewritten branch. This
    // sits on top of the filtered history and is intentionally absent from the
    // commit-map (no prior operation references it).
    if spec.add_to_gitignore {
        append_to_gitignore(&clone_git, branch, &spec.paths)?;
    }

    // The rewritten tip in the clone.
    let new_tip = clone_git
        .run(["rev-parse", branch])
        .map_err(|e| ExecuteError::FilterRepoFailed(format!("rewrite produced no tip: {e}")))?
        .stdout
        .trim()
        .to_string();

    // Copy the rewritten objects into the real repo (refs untouched).
    git.run([
        "fetch",
        "--no-tags",
        path_str(clone_path)?,
        &format!("refs/heads/{branch}"),
    ])
    .map_err(|e| ExecuteError::FilterRepoFailed(format!("fetching rewrite failed: {e}")))?;

    Ok((new_tip, oid_map))
}

/// Parse filter-repo's `commit-map` file into an [`OidMap`].
///
/// The file has a header line (`old new`) followed by `<old-sha> <new-sha>`
/// pairs. A new-sha of all zeros marks a commit dropped by the filter.
fn parse_commit_map(text: &str) -> OidMap {
    let is_hex_sha = |s: &str| s.len() >= 40 && s.bytes().all(|b| b.is_ascii_hexdigit());
    let mut map = OidMap::new();
    for line in text.lines() {
        let mut fields = line.split_whitespace();
        let (Some(old), Some(new)) = (fields.next(), fields.next()) else {
            continue;
        };
        if !is_hex_sha(old) {
            continue; // header or malformed line
        }
        let value = if new.bytes().all(|b| b == b'0') {
            None // dropped
        } else {
            Some(CommitId(new.to_string()))
        };
        map.insert(CommitId(old.to_string()), value);
    }
    map
}

/// Append paths to `.gitignore` and commit the change.
fn append_to_gitignore(
    git: &Git,
    _branch: &str,
    paths: &[gunk_core::PathSpec],
) -> Result<(), ExecuteError> {
    let gitignore_path = git.repo_path().join(".gitignore");

    // Read existing content (if any).
    let mut content = std::fs::read_to_string(&gitignore_path).unwrap_or_default();

    // Ensure trailing newline before appending.
    if !content.is_empty() && !content.ends_with('\n') {
        content.push('\n');
    }

    // Append each path on its own line, escaping gitignore-special chars.
    content.push_str("\n# Removed from history by gunk\n");
    for path in paths {
        let escaped = escape_gitignore_path(&path.0);
        content.push_str(&escaped);
        content.push('\n');
    }

    std::fs::write(&gitignore_path, &content)
        .map_err(|e| ExecuteError::FilterRepoFailed(format!("failed to write .gitignore: {e}")))?;

    // Stage and commit.
    git.run(["add", ".gitignore"])?;
    git.run(["commit", "-m", "chore: add removed paths to .gitignore"])?;

    Ok(())
}

/// Escape a file path for safe inclusion in `.gitignore`.
///
/// Leading `#` and `!` have special meaning in gitignore; backslash-escape them.
/// Spaces at line boundaries also need escaping.
fn escape_gitignore_path(path: &str) -> String {
    let mut s = path.to_string();
    if s.starts_with('#') || s.starts_with('!') {
        s.insert(0, '\\');
    }
    if s.ends_with(' ') {
        // Trailing space must be escaped.
        s.truncate(s.len() - 1);
        s.push_str("\\ ");
    }
    s
}

// ── Composite plan execution ───────────────────────────────────────

/// Execute a full `ExecutionPlan`, handling all plan types including composites.
///
/// For composite plans, execution order matters:
/// 1. FilterRepo runs first (rewrites entire history).
/// 2. Rebase runs afterward (OIDs change after filter-repo, so the caller
///    should re-snapshot and re-plan if combining filter-repo with rebase).
pub fn execute_plan(
    git: &Git,
    branch: &str,
    exec_plan: &gunk_core::ExecutionPlan,
) -> Result<ExecuteResult, ExecuteError> {
    match exec_plan {
        gunk_core::ExecutionPlan::Rebase(todo) => execute_rebase(git, branch, todo),
        gunk_core::ExecutionPlan::FilterRepo(spec) => execute_filter_repo(git, branch, spec),
        gunk_core::ExecutionPlan::Flatten(spec) => execute_flatten(git, branch, spec),
        gunk_core::ExecutionPlan::Composite(plans) => execute_composite(git, branch, plans),
    }
}

/// Execute a composite plan (multiple sub-plans in order).
///
/// Creates a single backup ref for the entire composite. If any sub-plan
/// fails, restores from the initial backup.
///
/// Sub-plans are built against the *original* snapshot, but each history-
/// rewriting phase (flatten, filter-repo) changes commit ids. Before running a
/// later phase, its plan is retargeted through the accumulated rewrite map so
/// its operations land on the ids that actually exist at that point.
fn execute_composite(
    git: &Git,
    branch: &str,
    plans: &[gunk_core::ExecutionPlan],
) -> Result<ExecuteResult, ExecuteError> {
    if plans.is_empty() {
        return Err(ExecuteError::Unsupported("empty composite plan".into()));
    }

    // 1. Dirty tree check (once for the whole composite).
    check_clean(git)?;

    // 2. Create a single backup ref for the entire composite.
    let backup_ref = create_backup_ref(git, branch)?;

    // Original→current id map, accumulated across rewrite phases.
    let mut accumulated: OidMap = OidMap::new();
    let mut last_result: Option<ExecuteResult> = None;
    // Collect "you rewrote a pushed commit" warnings from every phase. The
    // final result only carries the last phase's, so gather them here or a
    // warning from an earlier phase would be lost.
    let mut pushed_commits: Vec<String> = Vec::new();

    for sub_plan in plans {
        // Retarget this phase's plan onto the history produced by prior phases.
        let remapped = match sub_plan.remap_oids(&accumulated) {
            Ok(p) => p,
            Err(e) => {
                let _ = restore_backup(git, branch, &backup_ref);
                return Err(e.into());
            }
        };

        let sub_result = match &remapped {
            gunk_core::ExecutionPlan::Rebase(todo) => execute_rebase(git, branch, todo),
            gunk_core::ExecutionPlan::FilterRepo(spec) => execute_filter_repo(git, branch, spec),
            gunk_core::ExecutionPlan::Flatten(spec) => execute_flatten(git, branch, spec),
            gunk_core::ExecutionPlan::Composite(inner) => execute_composite(git, branch, inner),
        };

        match sub_result {
            Ok(result) => {
                for c in &result.pushed_commits {
                    if !pushed_commits.contains(c) {
                        pushed_commits.push(c.clone());
                    }
                }
                // Fold this phase's rewrite into the accumulated map so the next
                // phase (still in original ids) resolves all the way through.
                accumulated = compose_oid_maps(&accumulated, &result.oid_map);
                last_result = Some(result);
            }
            Err(e) => {
                // Restore from the composite backup.
                let _ = restore_backup(git, branch, &backup_ref);
                return Err(e);
            }
        }
    }

    // Return the composite result with the original (top-level) backup ref and
    // the full original→final rewrite map.
    let final_result = last_result.unwrap();
    Ok(ExecuteResult {
        backup_ref,
        new_tip: final_result.new_tip,
        branch: final_result.branch,
        pushed_commits,
        oid_map: accumulated,
    })
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

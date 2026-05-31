# ADR-0018: Execution Engine & Safety Protocol

## Status
Accepted

## Context
Phase 4 implements the trust layer — the part that actually mutates Git history. This is the highest-risk component because a bug here could destroy a user's work. The safety protocol from the plan (§2.3) must be implemented faithfully.

## Decision

### Architecture
The execution engine lives in `gitio::execute` as a set of public functions (not methods on `Git`) to keep the module focused and testable:

- **`check_clean(git)`** — Refuses if working tree/index is dirty (`status --porcelain=v2 -z`).
- **`stash_push(git)` / `stash_pop(git)`** — Opt-in auto-stash with detection of whether anything was actually stashed.
- **`create_backup_ref(git, branch)`** — Creates `refs/gunk/backup/<branch>/<unix-ts>` pointing at the current tip.
- **`list_backup_refs(git, branch)`** — Lists existing backups for restore UI.
- **`restore_backup(git, branch, ref)`** — Resets branch to a backup ref, with working tree reset if currently checked out.
- **`execute_rebase(git, branch, todo)`** — Orchestrates the full safety protocol for rebase-class plans.
- **`WorktreeGuard`** — RAII struct that adds/removes a detached worktree, ensuring cleanup on drop.

### Safety Protocol (implemented)
1. **Dirty tree refusal** — `check_clean` is the first call in `execute_rebase`. If dirty, returns `ExecuteError::DirtyTree` immediately.
2. **Backup ref** — Created before any mutation. Format: `refs/gunk/backup/<branch>/<unix-ts>`. The backup ref survives indefinitely (not cleaned by `gc`).
3. **Worktree rehearsal** — A detached worktree (`.gunk-rehearsal`) is created at the branch tip. The rebase runs there first. On conflict, the real branch is never touched.
4. **Apply on success** — Only after clean rehearsal does the real branch ref get updated via `update-ref`.
5. **RAII cleanup** — `WorktreeGuard::drop()` always removes the worktree, even on panic or early return.

### Cross-Platform Editor Wiring
Git for Windows uses its internal MSYS2 bash to execute editors — not `cmd.exe`. Therefore:
- We always write a POSIX shell script (`.gunk-seq-editor.sh`) regardless of platform.
- All paths in the script use forward slashes.
- The `GIT_SEQUENCE_EDITOR` env var points to this script (with forward-slash path).
- `GIT_EDITOR` is set to `true` (a POSIX no-op) which works in Git for Windows' bash.

This eliminates the class of bugs where Windows path separators or cmd.exe quoting break editor invocation.

### Error Handling
- `ExecuteError` is a dedicated error enum (not reusing `GitError`) because execution failures have different semantics (rehearsal failed, conflict detected, dirty tree).
- On any failure during rehearsal, the worktree rebase is aborted (`rebase --abort`) and the guard cleans up.
- The caller receives a clear error and the guarantee that the real branch is untouched.

## Consequences
- The execution engine is integration-tested against real throwaway repos (18 tests).
- All rebase-class operations (drop, squash, fixup, reorder) go through the same safety envelope.
- Phase 5 will wire individual features end-to-end and add proper message feeding for reword operations.
- Filter-repo execution (Phase 6) and flatten execution (Phase 7) will follow the same safety protocol pattern.

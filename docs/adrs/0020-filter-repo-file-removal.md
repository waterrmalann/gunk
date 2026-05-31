# ADR 0020: git-filter-repo File Removal

## Status
Accepted

## Context
Phase 6 adds the ability to remove files from repository history. This is implemented using `git-filter-repo`, a Python tool that rewrites Git history to remove specified paths.

## Decisions

### 1. git-filter-repo as optional dependency
- Detected at repo-open time via `git filter-repo --version`.
- Feature is gated in the UI: if not installed, file removal checkboxes are hidden and an informational message is shown.
- This follows the plan's requirement to "disable the feature with a clear, actionable message rather than failing at apply time."

### 2. Scoped filter-repo via `--refs`
- `git filter-repo` is invoked with `--refs refs/heads/<branch>` to scope it to the target branch only.
- This puts filter-repo in "partial" mode, which **skips garbage collection** and **leaves other refs untouched**.
- This is critical because our backup refs at `refs/gunk/backup/<branch>/<timestamp>` must survive the rewrite — they point at the pre-rewrite commit objects, which must remain in the object database for restore.

### 3. No worktree rehearsal for filter-repo
- Unlike `execute_rebase` (which uses a throwaway worktree), `execute_filter_repo` runs directly on the repository.
- Rationale: filter-repo is atomic — it either succeeds completely or fails without partial writes. The backup ref + restore pattern provides equivalent safety.
- If filter-repo fails, the error path calls `restore_backup()` to reset the branch.

### 4. Composite plan OID invalidation
- When a user combines RemovePaths with rebase operations, the plan engine produces `Composite([FilterRepo, Rebase])`.
- Filter-repo rewrites all commit OIDs. The rebase todo, computed against the original snapshot, references stale OIDs.
- **Solution**: The app layer splits filter-repo and rebase operations, executes filter-repo first, re-snapshots (`walk_commits`), re-plans the rebase operations against the fresh snapshot, then executes the rebase. This avoids the stale OID problem entirely.

### 5. Gitignore integration
- When `add_to_gitignore` is true, removed paths are appended to `.gitignore` with a comment header, then staged and committed.
- Paths with leading `#` or `!` are backslash-escaped to avoid gitignore syntax collisions.
- Existing `.gitignore` content is preserved.

### 6. Draft reducer merges RemovePaths
- Multiple `DraftMsg::RemovePaths` messages are merged into a single `Operation::RemovePaths` in the draft state, deduplicating paths and OR-ing the `add_to_gitignore` flag.

## Consequences
- File removal from history requires `git-filter-repo` (Python) to be installed.
- The `--refs` scoping means only the target branch is rewritten; other branches retain the removed files.
- Backup refs survive filter-repo and can be used for restore.
- Mixed operations (file removal + rebase) work correctly via the re-snapshot pipeline.

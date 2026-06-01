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

### 2. Branch scoping via single-branch clone (not `--refs`)
- Branch scoping is achieved by the rehearsal clone (decision 3), not by `git filter-repo --refs`. The clone is made with `--single-branch --branch <branch>`, so it contains only the target branch; `git filter-repo` is then invoked with just `--invert-paths --force` plus the `--path`/`--path-glob` entries, with no `--refs` argument.
- Running filter-repo inside an isolated clone means the real repo's object store and refs are never touched during the rewrite. Our backup refs at `refs/gunk/backup/<branch>/<unix-millis>` live in the real repo and are therefore unaffected — only the rewritten tip is fetched back on success (decision 3).
- Empty path sets are rejected upstream by the plan engine (`PlanError::EmptyPathRemoval`) so filter-repo is never invoked with no `--path` args (which would rewrite every commit id for no benefit).

### 3. Clone-based rehearsal for filter-repo
- Unlike `execute_rebase` (which uses a throwaway worktree), `execute_filter_repo` rehearses in an **isolated `--no-hardlinks --single-branch` clone** of the repository.
- Rationale: filter-repo rewrites the shared object store and refs by name, so a worktree (which shares these) cannot provide isolation. A full clone gives a hermetic sandbox.
- On success, the rewritten tip is fetched back into the real repo; the branch ref and working tree are updated.
- On failure, the clone is discarded and the real repo is untouched.
- The clone's `.git/filter-repo/commit-map` is parsed to build the OID mapping for composite plan retargeting (see ADR-0024).

### 4. Composite plan OID retargeting
- When a user combines RemovePaths with rebase operations, the plan engine produces `Composite([FilterRepo, Rebase])`.
- Filter-repo rewrites all commit OIDs. The rebase todo, computed against the original snapshot, references stale OIDs.
- **Solution**: `execute_composite` threads an accumulated `OidMap` through phases — each sub-plan is retargeted via `ExecutionPlan::remap_oids()` before execution, and the result's OID map is composed into the accumulator (see ADR-0024). The app layer no longer re-snapshots or re-plans between phases.

### 5. Gitignore integration
- When `add_to_gitignore` is true, removed paths are appended to `.gitignore` with a comment header, then staged and committed.
- Paths with leading `#` or `!` are backslash-escaped to avoid gitignore syntax collisions.
- Existing `.gitignore` content is preserved.

### 6. Draft reducer merges RemovePaths
- Multiple `DraftMsg::RemovePaths` messages are merged into a single `Operation::RemovePaths` in the draft state, deduplicating paths and OR-ing the `add_to_gitignore` flag.

## Consequences
- File removal from history requires `git-filter-repo` (Python) to be installed.
- The single-branch clone means only the target branch is rewritten; other branches retain the removed files.
- Backup refs survive filter-repo (they live in the untouched real repo) and can be used for restore.
- Mixed operations (file removal + rebase) work correctly via the OID remap pipeline (ADR-0024).

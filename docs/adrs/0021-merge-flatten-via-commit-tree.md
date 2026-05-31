# ADR-0021: Merge Flatten via commit-tree

## Status
Accepted

## Context
Users need to flatten merge commits into single ordinary commits so that
squashing across what was previously a merge boundary becomes possible.
This is the trickiest feature in the plan because merge commits have
multiple parents and any naïve cherry-pick or patch-apply approach risks
conflicts or tree divergence.

## Decision
Flatten uses `git commit-tree` to create a new ordinary commit (`M'`) that:

1. **Reuses the merge commit's tree verbatim** — `T = M^{tree}`.
2. **Has a single parent** — the first (mainline) parent of the merge.
3. **Carries the merge's message** (or a user-supplied override).

This guarantees the resulting tree is byte-identical to the merge result,
including any conflict resolutions that were applied during the original merge.

After creating `M'`, any descendants of the original merge are rebased onto
`M'` using `git rebase --onto M' M HEAD` inside a detached worktree. This
makes the branch fully linear.

### Safety protocol
The same safety protocol as all other mutations applies:

- Dirty-tree refusal.
- Backup ref created before mutation (`refs/gunk/backup/<branch>/<ts>`).
- Operation rehearsed in a throwaway worktree (RAII guard for cleanup).
- Real branch ref updated only on successful rehearsal.
- Worktree uses `HEAD` (detached) for the rebase target to avoid the
  "branch already checked out" error from the original worktree.

### Composition with other operations
The plan engine orders flatten **before** filter-repo and rebase. Flatten
rewrites OIDs, so the app's execution pipeline re-snapshots commits after
flatten before proceeding to subsequent phases (filter-repo → rebase).

### Scope limitations (v1)
- **Octopus merges** (3+ parents) are rejected with `PlanError::OctopusMergeUnsupported`.
- Non-merge commits are rejected with `PlanError::NotAMergeCommit`.

## Consequences
- Merge commits can be flattened from the UI with a single toggle button.
- After flattening, the history is linear and all rebase-class operations
  (squash, fixup, reorder, drop) work across the former merge boundary.
- The `DraftMsg::ToggleFlatten` toggle in the draft reducer allows users
  to add/remove flatten intent without touching the real repo.
- 13 new tests (11 integration + 2 draft unit) cover: tip flatten,
  mid-history flatten with descendants, tree preservation, resolved-conflict
  preservation, dirty-tree refusal, backup creation, restore from backup,
  fast-forward merges, execute_plan dispatch, flatten-then-squash composite,
  message preservation, and draft toggle semantics.

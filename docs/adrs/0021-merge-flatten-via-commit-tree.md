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
`M'` inside a detached worktree. The replay command depends on the chosen
[descendant-merge strategy](#descendant-merge-strategy):

- **Preserve** (default): `git rebase --rebase-merges --onto M' M HEAD` — only
  the selected merge is flattened; any newer, unrelated merge is recreated.
- **Linearize** (opt-in): `git rebase --onto M' M HEAD` — the whole range is
  flattened, dropping every merge in it.

When the selected merge has no descendant merges, both strategies produce an
identical linear result.

### Safety protocol
The same safety protocol as all other mutations applies:

- Dirty-tree refusal.
- Backup ref created before mutation (`refs/gunk/backup/<branch>/<ts>`).
- Operation rehearsed in a throwaway worktree (RAII guard for cleanup).
- Real branch ref updated only on successful rehearsal.
- Worktree uses `HEAD` (detached) for the rebase target to avoid the
  "branch already checked out" error from the original worktree.

### Descendant-merge strategy
A flatten target may have *descendant merges* — merges newer than the selected
one that the user never touched. The replay must not silently destroy them.

A plain `git rebase --onto` linearizes its entire range, so it would collapse
those unrelated merges along with the selected one — a silent, surprising
rewrite. We reject that as the default. Instead the strategy is surfaced to the
end user (`FlattenStrategy` on `Operation::FlattenMerge` / `FlattenSpec`):

- **`PreserveDescendantMerges`** (default): replay with
  `git rebase --rebase-merges`, which recreates the unselected merges. If
  recreating one hits a re-merge conflict the operation fails loudly via the
  normal rehearsal-failure path rather than diverging silently.
- **`Linearize`**: the old plain-rebase behavior, now an explicit choice. The
  UI gates it behind a warning so only users who know they want a fully linear
  result reach for it.

**OID-map caveat.** When the range contains a descendant merge, the post-replay
linear `rev-list` no longer lines up position-for-position with the pre-replay
one (preserve rebuilds topology; linearize folds in second-parent commits).
`run_flatten_in` detects this and marks every descendant as dropped (`None`) in
the returned `OidMap`. A standalone flatten never reads that map, so it still
succeeds. But a later phase of a *composite* that tries to touch one of those
commits gets a clean `CommitNotFound` and the whole composite rolls back — the
safe outcome (see ADR-0024), not a mis-targeted rewrite.

### Composition with other operations
The plan engine orders flatten **before** filter-repo and rebase. Flatten
rewrites OIDs, so subsequent phases must be retargeted onto the post-flatten
history. This is now handled entirely inside `gitio::execute_composite` by
threading an accumulated `OidMap` through the phases — the app no longer
re-snapshots or re-plans between phases. See ADR-0024 for the OID-remap
mechanism (which supersedes the earlier app-side re-snapshot approach).

### Scope limitations (v1)
- **Octopus merges** (3+ parents) are rejected with `PlanError::OctopusMergeUnsupported`.
- Non-merge commits are rejected with `PlanError::NotAMergeCommit`.
- **Descendant merges** are handled via the
  [descendant-merge strategy](#descendant-merge-strategy) above rather than a
  flat refusal: the default preserves them with `--rebase-merges`, and a
  power-user opt-in linearizes. Composites that flatten a merge and then try to
  edit a commit *underneath* a descendant merge still roll back cleanly via the
  OID-remap `CommitNotFound` path (see ADR-0024), because the descendant ids
  can't be reliably remapped across the topology change.

## Consequences
- Merge commits can be flattened from the UI with a single toggle button.
- After flattening, the history is linear and all rebase-class operations
  (squash, fixup, reorder, drop) work across the former merge boundary.
- The `DraftMsg::ToggleFlatten` toggle in the draft reducer allows users
  to add/remove flatten intent without touching the real repo;
  `DraftMsg::SetFlattenStrategy` switches an already-flattened merge between
  preserve and linearize.
- Tests cover: tip flatten, mid-history flatten with descendants, tree
  preservation, resolved-conflict preservation, dirty-tree refusal, backup
  creation, restore from backup, fast-forward merges, execute_plan dispatch,
  flatten-then-squash composite, message preservation, draft toggle/strategy
  semantics, preserve-keeps-descendant-merge, linearize-drops-descendant-merge,
  and composite rollback when a descendant merge can't be remapped.

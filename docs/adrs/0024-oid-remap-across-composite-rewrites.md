# ADR-0024: OID Remapping Across Composite Rewrite Phases

## Status
Accepted

## Context
When a user combines operations from different rewrite classes (flatten, filter-repo,
rebase), the plan engine produces a `Composite([Flatten, FilterRepo, Rebase])`. Each
history-rewriting phase changes commit OIDs. The original app-side workaround —
splitting ops, executing each phase, re-snapshotting via `walk_commits`, then
re-planning subsequent phases against the fresh snapshot — was fragile, duplicated
plan-engine logic in the UI layer, and was the source of a critical bug (C1): the
re-plan used the *original* snapshot's OIDs when the re-snapshot happened to fail
or was skipped.

## Decision

### OidMap type
A new `OidMap = HashMap<CommitId, Option<CommitId>>` is introduced in `core::plan`:
- `Some(new_id)` — the commit was rewritten to a new OID.
- `None` — the commit was dropped by the rewrite.
- Absent key — identity mapping (commit was not touched).

### Composition
`compose_oid_maps(first, second)` chains two maps: given an original→intermediate
map and an intermediate→final map, it produces an original→final map. This is
associative and handles dropped commits correctly.

### Plan retargeting
`ExecutionPlan::remap_oids(&self, map: &OidMap)` produces a new `ExecutionPlan`
whose operation targets are translated through the map. If a required target was
dropped, it returns `PlanError::RemappedTargetDropped`.

### Execution engine threading
`execute_composite` in `gitio::execute`:
1. Creates a single backup ref for the entire composite.
2. Maintains an `accumulated: OidMap`, initially empty.
3. Before each sub-plan: `sub_plan.remap_oids(&accumulated)`.
4. After each sub-plan: `accumulated = compose_oid_maps(&accumulated, &result.oid_map)`.
5. On failure: restores from the single backup ref.

Each phase populates its `ExecuteResult.oid_map`:
- **Flatten**: maps the old merge OID to the new linearised OID, plus all
  descendant mappings via `rev-list`.
- **Filter-repo**: parses `.git/filter-repo/commit-map`.
- **Rebase**: returns an empty map (always the last phase by plan-engine ordering).

### App-side simplification
The app no longer splits operations or re-snapshots between phases. It calls
`plan()` once, then dispatches to `gitio::execute_plan()` which handles composites
with correct OID retargeting internally.

## Consequences
- Composite plans are safe across arbitrary phase combinations.
- The UI layer contains no execution-order or OID-translation logic.
- A `RemappedTargetDropped` error surfaces clearly when a phase drops a commit that
  a later phase needs.
- The single-backup-ref design means rollback is always atomic.

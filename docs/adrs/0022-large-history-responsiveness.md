# ADR-0022: Large-history responsiveness in the app shell

## Status
Accepted

## Context

Phase 8 requires the UI to remain responsive on repositories with very large
histories (8,000+ commits observed). Even after lazy commit details (ADR-0012),
opening a repo or switching branches could still block the egui frame while
`git log` was running and while all rows were rendered each frame.

## Decision

We adopt a layered responsiveness strategy in `app` and `gitio`:

1. **Paged history reads in `gitio`**
   - Add `Git::walk_commits_page(branch, skip, max_count) -> CommitPage`.
   - Use over-fetch (`max_count + 1`) to compute `has_more` without a separate
     count command.
   - Keep `walk_commits` for full snapshots used by rewrite flows.

2. **Incremental loading in `app`**
   - Load an initial window (`COMMIT_PAGE_SIZE = 500`) for open/switch.
   - Expose explicit "Load 500 more" continuation while `has_more` is true.

3. **Background loading for open/switch operations**
   - Run repo-open and branch-switch reads on worker threads.
   - Return results through `std::sync::mpsc` channel to the UI loop.
   - Track request IDs and request kind to discard mismatched/stale responses.

4. **Virtualized commit list rendering**
   - Render commit rows via `egui::ScrollArea::show_rows` rather than rendering
     every loaded row each frame.

5. **Accurate per-commit path status for detail panes**
   - Enable rename/copy detection in `changed_paths` via
     `git diff-tree --name-status -r -M -C --find-copies-harder --root -z`.

## Consequences

- Large histories are usable immediately: the app can render and interact while
  branch/repo history is still loading.
- Frame cost scales with visible rows, not total loaded rows.
- The app keeps correctness by treating each background load response as typed
  and request-scoped.
- Trade-off: open/switch now use additional app-side state (`pending_load`,
  channel receiver, request IDs), increasing UI orchestration complexity.
- Trade-off: search still runs on currently loaded commits only; full-history
  search remains a later Phase 8 hardening step.

## Notes

- This ADR complements ADR-0012 (lazy detail loading) by addressing history
  list loading/rendering latency, not per-commit detail latency.
- Safety semantics for rewrite operations are unchanged and remain governed by
  ADR-0018.

# ADR-0017: Draft reducer and preview projection in core

**Status:** Accepted  
**Date:** 2026-05-31

## Context

ADR-0005 establishes draft mode (edits accumulate as a pending `Vec<Operation>`, nothing mutates the repo until Confirm) and ADR-0009 establishes the testable reducer pattern (`State × Msg → State`). Phase 3 needs to actually wire those into the egui app: accumulate draft edits, recompute the projected history on every change, and render it as a diffed preview.

Two questions arose. First, where does the draft reducer live — in `app` (per the letter of ADR-0009) or in `core`? Second, the projected preview needs per-commit status (reworded, dropped, absorbed, moved, …) for display, which is presentation-shaped data the plan engine does not produce — the engine emits an `ExecutionPlan` (a rebase todo), not a row-per-commit view.

## Decision

### Draft reducer in core

`DraftState`/`DraftMsg` and their `reduce` live in `core::draft`, not `app`. ADR-0009 allows "`app` (or testable module)"; core is the testable module. This keeps the reducer free of any egui dependency and unit-tested alongside the rest of the domain. The reducer encodes upsert/replace/toggle/merge semantics: reword upserts by target; set-message/set-author/set-co-authors replace by exact target set; squash/fixup replace by keep; drop toggles, and drop-many adds idempotently in bulk; reorder replaces; toggle-flatten adds/removes flatten intent; remove-paths merges into any existing remove-paths op (unioning paths, OR-ing the gitignore flag); and remove-op deletes a draft entry by index. `app` holds the `DraftState` and dispatches `DraftMsg`s collected from the render loop.

### Preview projection as a separate function

`core::preview::preview(snapshot, operations) -> Result<Vec<PreviewRow>, PlanError>` is a distinct projection layer, not part of the plan engine:

1. It **validates through the real engine first** — if there are operations, it calls `plan(snapshot, operations)?`, so an invalid draft surfaces the exact same `PlanError` the engine would produce. There is one source of truth for validity.
2. On success it projects one `PreviewRow` per commit in **display order** (reorder-aware, newest-first), each tagged with a `RowStatus` (Unchanged, Reworded, Reauthored, RewordedAndReauthored, SquashKeep, Absorbed, Flattened, Dropped), the projected summary, and a `moved` flag. Co-author edits (`SetCoAuthors`) modify the commit body and therefore surface as `Reworded`; there is no distinct co-author status. (Movement is carried by the `moved` flag, not a `RowStatus` variant.)

The app renders the list in projected order by iterating `preview_rows` when a draft exists (mapping each row back to its original index for selection/search), falling back to original order otherwise.

## Consequences

- **One validity source** — preview cannot disagree with the plan engine about whether a draft is valid; it literally runs the engine.
- **Presentation/engine separation** — the engine stays focused on producing an executable plan; row-status decoration for the UI lives in its own function.
- **Fully testable** — draft accumulation and preview projection are pure functions unit-tested without a window; `app` stays thin and presentational.
- **Trade-off:** preview re-runs the full plan on every edit. Fine at current scale; if it becomes hot, the plan result can be cached on the draft.

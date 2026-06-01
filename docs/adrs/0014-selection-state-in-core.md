# ADR-0014: Selection state machine in core, not app

**Status:** Accepted  
**Date:** 2026-05-31

## Context

ADR-0009 describes the reducer pattern for UI state. When implementing multi-select (Click, Ctrl+Click, Shift+Click), we needed to decide where the selection reducer lives — in `app` alongside egui, or in `core` as a pure domain module.

Selection semantics (anchor tracking for shift-range, toggle for ctrl, single-replace for plain click) are non-trivial logic that must be correct across all features: commit list interaction, search-result bulk selection, and future draft-mode operations. Placing this in `app` would couple it to the GUI crate and make it harder to test or reuse.

## Decision

Place `SelectionState` and `SelectionMsg` in `core::selection` as a pure, deterministic reducer with no IO or GUI dependency.

- `SelectionState` tracks a `BTreeSet<usize>` of selected indices, an optional anchor for shift-range, and bounds.
- `SelectionMsg` covers: `Click`, `CtrlClick`, `ShiftClick`, `Clear`, `SelectAll`, `SelectSet`.
- `reduce(&self, msg) -> Self` is the only transition function — immutable, returns new state.
- `app` reads modifier keys from egui input and maps them to the appropriate `SelectionMsg`. The egui layer holds zero selection logic.

## Consequences

- **23 unit tests** cover all selection behaviors without any GUI dependency.
- Selection semantics are reusable if the UI framework changes.
- `SelectSet` bridges search results directly into selection, enabling "Select all results" with no glue logic.
- `app` only does `ui.input(|inp| inp.modifiers)` → `SelectionMsg` mapping — trivially correct.

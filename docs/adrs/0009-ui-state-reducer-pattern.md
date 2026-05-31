# ADR-0009: UI state as a testable reducer

**Status:** Accepted  
**Date:** 2026-05-31

## Context

egui is an immediate-mode GUI library. Business logic placed directly in rendering callbacks is untestable without a window. We need UI state transitions (selection, search, draft operations) to be testable without running the GUI.

## Decision

Model UI state transitions as a pure reducer function: `State × Msg → State`.

- `State` holds the current selection, search query, draft operations, view mode, etc.
- `Msg` represents user actions (click commit, ctrl+click, search, add operation, confirm, discard).
- The reducer is a pure function in `app` (or testable module) — no IO, no egui dependency.
- egui rendering reads `State` and emits `Msg`s. The render loop calls the reducer after collecting messages.

## Consequences

- **Testable** — selection logic, multi-select, search filtering, and draft management are all unit-tested as plain functions.
- **No logic in callbacks** — egui code is purely presentational.
- **Predictable** — state transitions are explicit and traceable.
- **Trade-off:** slightly more boilerplate than inline mutation. Worth it for testability.

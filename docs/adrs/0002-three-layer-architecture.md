# ADR-0002: Three-layer architecture separating pure logic from side effects

**Status:** Accepted  
**Date:** 2026-05-31

## Context

The app must rewrite Git history — a destructive operation. To make this safe and testable, we need a clear separation between domain logic (what to do) and IO (actually doing it). Without this, business logic leaks into UI callbacks and git-invocation code, making it untestable and error-prone.

## Decision

Structure the project as a Cargo workspace with three functional layers (four crates):

```
crates/
├─ core/      # PURE. No IO. Domain model + plan engine. Unit-tested exhaustively.
├─ gitio/     # IO. Thin typed wrapper over the git binary. Integration-tested vs fixtures.
├─ testkit/   # Test-only: RepoFixture builder + assertions. Dev-dependency.
└─ app/       # eframe/egui binary. Thin. Holds NO domain logic.
```

- **`core`** knows nothing about `git` or the filesystem. It takes an immutable history snapshot (`Vec<Commit>`) plus a set of user `Operation`s, validates them, and produces a deterministic `ExecutionPlan`. This is pure and snapshot-testable.
- **`gitio`** reads snapshots from the git binary and executes `ExecutionPlan`s. Side-effecting, tested against real throwaway repos.
- **`app`** is a thin egui shell. UI state transitions are modeled as a testable reducer (`State × Msg → State`). No business logic lives in egui callbacks.

## Consequences

- **Testability** — `core` is pure/deterministic, enabling exhaustive unit tests, snapshot tests (`insta`), and property tests (`proptest`) with zero IO.
- **Safety** — mutation is isolated in `gitio`, which enforces the safety protocol (backup refs, worktree rehearsal, dirty-tree refusal).
- **Maintainability** — clear dependency direction: `app → core + gitio`, `gitio → core`, `core → nothing`. No cycles.
- **Trade-off:** more crates to manage; slightly more boilerplate for cross-crate types. Worth it for the testability guarantee.

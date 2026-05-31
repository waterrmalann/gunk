# ADR-0003: Operations modeled as data, not imperative git calls

**Status:** Accepted  
**Date:** 2026-05-31

## Context

Users accumulate edits in draft mode (reword, squash, reorder, drop, flatten, remove files, set author). These edits could be executed immediately as git commands, or captured as data and compiled into a plan.

Imperative execution makes it impossible to:
- Preview the result before applying.
- Validate the full set for conflicts (e.g., drop + reword on the same commit).
- Reorder operations for correctness (e.g., flatten before squash).
- Snapshot-test the output deterministically.

## Decision

All user intents are captured as an `enum Operation` — a pure data structure. The plan engine (`core::plan(snapshot, operations) → Result<ExecutionPlan, PlanError>`) compiles them into a concrete `ExecutionPlan` that describes exactly what git commands to run.

```rust
enum Operation {
    Reword { target, summary, body },
    SetAuthor { targets, author },
    SetMessage { targets, summary, body },
    Squash { keep, absorb },
    Fixup { keep, absorb },
    Drop { target },
    Reorder { new_order },
    RemovePaths { paths, add_to_gitignore },
    FlattenMerge { merge },
}

enum ExecutionPlan {
    Rebase(RebaseTodo),
    FilterRepo(FilterRepoSpec),
    Flatten(FlattenSpec),
    Composite(Vec<ExecutionPlan>),
}
```

The plan engine validates, detects conflicts, auto-reorders when necessary, and is idempotent and order-independent with respect to how the user entered edits.

## Consequences

- **Draft mode** is trivial — just a `Vec<Operation>`.
- **Preview** — recompute the projected history from `core` on every edit, shown as a diffed view.
- **Validation** — conflicting or impossible operations produce typed `PlanError`s before any mutation.
- **Determinism** — `ExecutionPlan` is snapshot-testable with `insta`.
- **Composition** — `Composite` allows flatten-then-rebase or filter-repo-then-rebase in a single confirm.

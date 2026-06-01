# ADR-0016: Plan engine validation pipeline

**Status:** Accepted  
**Date:** 2026-05-31

## Context

The plan engine (`core::plan`) compiles user `Operation`s into an `ExecutionPlan`. Since an invalid or contradictory set of operations could corrupt a repository if executed, the engine must reject bad input before producing output. Additionally, generated rebase `exec` lines embed user-supplied identity strings in shell commands, creating a command injection risk.

## Decision

### Validation pipeline

The plan engine runs a three-stage validation pipeline before building any plan:

1. **Commit existence** — Every `CommitId` referenced by any operation must exist in the snapshot. Fail-fast with `PlanError::CommitNotFound`.
2. **Structural invariants** — Per-operation checks: flatten targets must be 2-parent merges (not octopus), squash/fixup must have non-empty absorb lists, absorb must not contain the keep commit, remove-paths must have a non-empty path set (`PlanError::EmptyPathRemoval` — an empty set would rewrite every commit id for no benefit).
3. **Cross-operation conflict detection** — Detects contradictions across the full operation set: drop+reword, drop+set-author, drop+set-co-authors, reword+set-message on the same commit, a commit appearing as both keep and absorbed, operations on absorbed commits (reword/set-author/set-message/set-co-authors), duplicate keeps, multiple reorders.

Each failure is a typed `PlanError` variant (not a generic string) so callers can handle errors programmatically.

### Shell escaping

Author identity fields interpolated into `git commit --amend --author="…"` exec lines are escaped for double-quote contexts (`"`, `\`, `$`, `` ` ``). This prevents command injection from malicious or accidental special characters in author names/emails.

### Module structure

The plan engine is split into submodules by concern:

- `plan/mod.rs` — Public types (`ExecutionPlan`, `PlanError`, etc.) and orchestrator
- `plan/validate.rs` — Validation and conflict detection
- `plan/rebase.rs` — Rebase todo builder (ordering, adjacency, line generation)
- `plan/tests.rs` — Unit tests, snapshot tests (insta), and property tests (proptest)

## Consequences

- Invalid operation sets are rejected early with specific, actionable errors.
- The UI can display targeted messages per error variant rather than parsing strings.
- Shell injection via identity fields is mitigated.
- Each submodule can be reasoned about and tested independently.

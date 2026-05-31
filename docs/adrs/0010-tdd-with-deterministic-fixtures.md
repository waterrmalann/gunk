# ADR-0010: Test-driven development with deterministic fixtures

**Status:** Accepted  
**Date:** 2026-05-31

## Context

The app rewrites Git history. Bugs in this domain silently corrupt user data. We need a testing strategy that gives high confidence in correctness and catches regressions early.

## Decision

TDD is mandatory. Red → green → refactor, always. The testing strategy is:

- **`core`** — pure unit tests + `insta` snapshot tests of `ExecutionPlan` + `proptest` invariants. Fast, no IO. Near-total coverage.
- **`gitio`** — integration tests against `testkit::RepoFixture` (real repos in tempdirs). Assert resulting history shape (oids, messages, authors, parentage), not stdout strings. Always assert the safety net: backup ref exists, real branch untouched on failure.
- **`app`** — test the reducer (`State × Msg → State`) as plain functions. egui rendering is not unit-tested; kept logic-free so there's nothing to test.

**Determinism:** pin author/committer dates and identities in fixtures (`GIT_AUTHOR_DATE`, `GIT_COMMITTER_DATE`, env identities) so oids/snapshots are stable across machines and CI.

**`testkit::RepoFixture`** is a builder that scripts throwaway repos: `.commit()`, `.branch()`, `.merge()`, `.commit_by()`. It is the foundation of all integration tests and is itself tested.

## Consequences

- **High confidence** — the plan engine and safety protocol are heavily tested.
- **Fast feedback** — `core` tests run in milliseconds with no IO.
- **Stable snapshots** — pinned dates/identities make oids deterministic across platforms.
- **CI matrix** — tests run on Linux, macOS, and Windows.

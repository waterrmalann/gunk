# ADR-0001: Shell out to git CLI instead of using library bindings

**Status:** Accepted  
**Date:** 2026-05-31

## Context

We need to read and rewrite Git history. There are several options:

1. **`libgit2` / `git2-rs`** — C library with Rust bindings. Good read coverage, but incomplete and sometimes incorrect rebase/rewrite behavior.
2. **`gix` (gitoxide)** — Pure Rust Git implementation. Still maturing; rewrite operations are incomplete.
3. **`git` CLI** — The canonical implementation. Interactive rebase, `filter-repo`, and plumbing commands are fully and correctly implemented.

The dangerous operations (interactive rebase, history filtering) are fully and correctly implemented only in the `git` CLI and in `git-filter-repo`. Reimplementing them risks corrupting a user's history — the one thing this tool must never do.

## Decision

Shell out to the user's own `git` binary for all operations.

- **Reads** use plumbing commands with machine-readable, NUL-delimited output (`-z`, `--pretty=format:` with `%x00` separators, `for-each-ref --format`, `diff-tree -z`, `cat-file --batch`). We parse typed structs from that output.
- **Writes** are expressed as generated rebase todo files fed to `git rebase -i` (via `GIT_SEQUENCE_EDITOR`/`GIT_EDITOR` overrides), `git-filter-repo` invocations, or low-level plumbing (`commit-tree`, `update-ref`).

`gix` may be introduced later purely as a read-path performance optimization for very large repos. It is not in the v1 critical path.

## Consequences

- **Single source of truth** — the user's own `git` binary, full behavioral fidelity.
- **Fewer dependencies** — no native C compilation (`libgit2`) or large Rust dep tree (`gix`).
- **Cross-platform** — `git` is available everywhere; we control editor wiring via our own subcommands.
- **Trivially reproducible** — a user can inspect and replay the exact commands we run.
- **Trade-off:** slower than in-process bindings for read-heavy workloads. Acceptable for v1; `gix` can optimize later.
- **Requirement:** `git` must be installed and on PATH. Detected at startup.

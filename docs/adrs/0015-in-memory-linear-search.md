# ADR-0015: In-memory linear search over commits

**Status:** Accepted  
**Date:** 2026-05-31

## Context

The app needs search functionality to find commits by message, author, or changed file path. Options considered:

1. **In-memory linear scan** — iterate all loaded commits, case-insensitive substring match.
2. **Inverted index** — pre-build a token → commit-index map at load time.
3. **Shell out to git log --grep / --author** — let git do the filtering.

## Decision

Use in-memory linear scan (`core::search::search_commits`).

- Case-insensitive substring matching across: summary, body, author name, author email, and changed paths.
- Returns `Vec<SearchHit>` with the matching index and which fields matched.
- `search_hit_indices()` extracts a `BTreeSet<usize>` for direct use with `SelectionMsg::SelectSet`.
- Recomputed on every query change (no caching).

## Rationale

- **Simplicity** — ~60 lines of code, no data structures to maintain.
- **Sufficient performance** — a branch walk typically returns hundreds to low thousands of commits. Linear scan over that is sub-millisecond.
- **Pure and testable** — lives in `core` with no IO. 17 tests cover matching, multi-field hits, edge cases.
- **git log --grep** was rejected because it would require a round-trip to the git process on every keystroke and couldn't search changed paths without additional commands.

## Consequences

- Search is instant for typical repo sizes (< 10k commits).
- For very large repos (100k+ commits), we may need to debounce the search input or introduce an inverted index. This is a Phase 8 concern per the plan's large-repo hardening pass.
- Multi-field `SearchHit` allows the UI to highlight *why* a commit matched (not yet wired, but the data is there).

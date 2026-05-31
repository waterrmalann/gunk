# ADR-0012: Lazy-load commit details (changed paths + diff)

**Status:** Accepted  
**Date:** 2026-05-31

## Context

`walk_commits(branch)` returns the full linear history of a branch as `Vec<Commit>`. For each commit we need metadata (oid, parents, author, committer, summary, body) and detail (changed file paths, diff patch).

Loading everything eagerly would mean running `git diff-tree` and `git show` for every commit in the history on branch switch — potentially thousands of commits. This would make branch switching slow and waste resources on detail the user may never look at.

## Decision

Split commit data into two tiers:

1. **Metadata (eager):** `walk_commits` populates all fields of `Commit` *except* `changed_paths`, which is left as an empty `Vec`. This uses a single `git log` call with a compact format string.

2. **Detail (lazy):** `changed_paths(oid)` and `show_diff(oid)` are separate `gitio` methods, called on-demand when the user selects a commit in the UI. Each is a single plumbing call (`diff-tree -z` and `show -p` respectively).

The `Commit` struct retains the `changed_paths: Vec<PathChange>` field for downstream use (search-by-filename in Phase 2, file removal UI in Phase 6), but it is populated lazily by the caller rather than during the walk.

## Consequences

- **Fast branch switching** — loading a branch with 5,000 commits runs one `git log` call, not 5,001.
- **Responsive UI** — the commit list renders immediately; detail loads only on selection.
- **Simple API** — `changed_paths()` and `show_diff()` are independent, stateless calls. No caching layer needed in v1.
- **Trade-off:** selecting a commit incurs two git calls. For typical repos this is <50ms — imperceptible. If it becomes a bottleneck on large repos, a simple LRU cache in the app layer can be added later.
- **Trade-off:** `Commit.changed_paths` is empty after `walk_commits`. Callers must not assume it is populated. This is documented in the method's contract.

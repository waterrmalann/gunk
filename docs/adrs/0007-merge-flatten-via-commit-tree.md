# ADR-0007: Merge flatten via commit-tree plumbing

**Status:** Accepted  
**Date:** 2026-05-31

## Context

Users need to flatten merge commits into ordinary commits so that squashing across the former merge boundary becomes possible. This is the trickiest feature because a merge commit's tree is the result of combining two (or more) parent histories, potentially with conflict resolutions baked in.

Approaches considered:
1. **Cherry-pick / patch-apply the side branch** — risks conflicts when the merge originally had conflict resolutions.
2. **Rebase --onto** — same conflict risk; also changes commit ordering.
3. **`commit-tree` reusing the merge tree** — the merge commit's tree already contains the correct, fully-resolved result. Just re-parent it.

## Decision

Use `git commit-tree` to create a new ordinary commit that reuses the merge commit's tree byte-for-byte, parented only on the mainline parent:

```
M (merge) has tree T, parents P1 (mainline), P2 (side)
M' = git commit-tree T -p P1 -m "<flattened message>"
```

This guarantees the post-flatten tree is identical to the merge result — no conflicts possible. Descendants of M are then rebased onto M'.

This is modeled as `ExecutionPlan::Flatten(FlattenSpec)`. When combined with squashing, emit `ExecutionPlan::Composite([Flatten, Rebase])` so flatten happens first.

## Consequences

- **Zero conflict risk** — the tree is reused as-is.
- **Byte-identical result** — the working tree after flatten matches what it was after the merge.
- **Side branch commits are intentionally collapsed** — that's the point of "flatten into a single commit."
- **Octopus merges (3+ parents):** rejected with a clear `PlanError` in v1. May be supported later.
- **Composition** — the `Composite` plan type ensures correct ordering when flatten is followed by squash/rebase operations.

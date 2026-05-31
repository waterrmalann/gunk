# ADR-0006: git-filter-repo as optional dependency for file removal

**Status:** Accepted  
**Date:** 2026-05-31

## Context

Removing files from Git history requires rewriting every commit that touched those files. Git's built-in `filter-branch` is deprecated and slow. `git-filter-repo` is the recommended replacement — fast, correct, and well-maintained — but it is a Python script, not bundled with Git.

We could:
1. Bundle `git-filter-repo` — adds Python as a runtime dependency, licensing/distribution complexity.
2. Reimplement file removal with plumbing — risky, significant effort, likely buggy.
3. Depend on it externally and gate the feature — simple, correct, transparent.

## Decision

`git-filter-repo` is an optional external dependency. Detect it on startup (`git filter-repo --version`). If absent, disable the "Remove files from history" feature with a clear, actionable message ("Install git-filter-repo to enable this feature: https://github.com/newren/git-filter-repo#how-do-i-install-it"). Never fail at apply time due to a missing tool.

## Consequences

- **Correct file removal** — delegates to the purpose-built, well-tested tool.
- **No Python runtime requirement** for users who don't need file removal.
- **Graceful degradation** — feature is hidden/disabled, not broken.
- **Trade-off:** users must install a separate tool. The actionable message mitigates this.

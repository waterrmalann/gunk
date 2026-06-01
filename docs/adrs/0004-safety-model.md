# ADR-0004: Safety model — backup refs, worktree rehearsal, dirty-tree refusal

**Status:** Accepted  
**Date:** 2026-05-31

## Context

The tool rewrites Git history — an inherently destructive operation. The core UX promise is that users can clean up history "without fear of breaking something." A single bug or edge case that silently corrupts a branch would destroy user trust permanently.

Existing tools (command-line `git rebase -i`, `git filter-branch`) offer no guardrails beyond the reflog. Users routinely lose work or end up in broken rebase states.

## Decision

Every mutating apply follows a strict safety protocol, implemented in `gitio` and covered by integration tests:

1. **Refuse on dirty tree.** Check `git status --porcelain=v2 -z`. If dirty, refuse and offer to auto-stash (`git stash push -u`) with explicit user consent. Restore on completion/abort.

2. **Backup ref.** Before any rewrite, create `refs/gunk/backup/<branch>/<unix-millis>` pointing at the current branch tip. The suffix uses millisecond resolution and `create_backup_ref` runs a bounded uniqueness search, so two rewrites of the same branch within the same instant never collapse to one ref name and silently destroy an earlier recovery point. The UI exposes "Restore from backup" which is just `update-ref` back. The reflog is the secondary safety net.

3. **Rehearse in a throwaway worktree.** `git worktree add --detach <tmpdir> <branch-tip>`. Run the full plan there first. If it conflicts or fails, surface the error and **do not touch the real branch**. If it succeeds, force-update the real branch ref to the rehearsed result (a rewrite produces a tip that is not a descendant of the old one, so this is an overwrite, not a fast-forward).

4. **Atomic ref update.** The real branch ref is only moved once the rehearsal proves the plan applies cleanly.

5. **RAII worktree teardown.** The throwaway worktree is always cleaned up, even on panic or error.

6. **Push warning.** If the oldest rewritten commit is reachable from any remote-tracking ref, warn that this rewrites published history.

## Consequences

- **Users cannot accidentally destroy history** — backup ref always exists, restore is one click.
- **Conflicts are surfaced before real mutation** — rehearsal catches them.
- **No broken rebase states** — the real branch is never mid-rebase.
- **Trade-off:** every apply creates a throwaway worktree and replays the plan twice (rehearsal + real). This is slower but worth the safety guarantee.
- **Trade-off:** backup refs accumulate. May need periodic pruning (out of scope for v1).

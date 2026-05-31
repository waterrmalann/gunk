# ADR-0019: Message Feeding via Exec Lines in Rebase Todo

## Status
Accepted — supersedes the `GIT_EDITOR` portion of ADR-0008

## Context
ADR-0008 proposed wiring `GIT_EDITOR` to a self-subcommand (`gunk --write-msg <file>`) that copies a prepared message file onto the editor target git passes as `$1`. This works but has two problems:

1. **Single-shot limitation.** `GIT_EDITOR` is invoked once per editor prompt. During a squash, git opens the editor with a *combined* message for the user to finalize. Our subcommand would need to know *which* commit is currently being edited so it can look up the right message — but git passes no commit identifier to the editor. We'd have to maintain mutable state (a counter file) across invocations, which is fragile and hard to test.

2. **Interaction with `GIT_EDITOR=true`.** For commits that should keep their message unchanged (plain `pick`), we want the editor to be a no-op. But for `reword`/squash keeps, we need a real editor. A single `GIT_EDITOR` value can't serve both purposes without external state.

## Decision
Feed prepared messages through **`exec` lines** in the rebase todo itself, not through `GIT_EDITOR`:

1. Keep `GIT_EDITOR=true` (no-op) — this ensures squash/fixup combine messages using git's default behavior, and reword lines that lack a prepared message fall through safely.

2. When a `Reword(commit)` line has a corresponding entry in `message_map`:
   - Emit `pick <commit>` instead of `reword <commit>` (avoids editor prompt).
   - Append `exec git commit --amend -F '.gunk-msg-N.txt'` immediately after the group (after any trailing squash/fixup/exec lines).

3. Message files use **relative filenames** (`.gunk-msg-0.txt`, `.gunk-msg-1.txt`, …) in the worktree root. This avoids shell-escaping issues with absolute paths containing special characters (spaces, quotes, unicode).

4. `build_rebase_text()` handles group awareness: the `exec` for a message flush is emitted at group boundaries (before the next `pick`/`reword`/`drop` or at the end of the todo), so squash/fixup chains complete before the amend fires.

5. All message files are cleaned up after execution (success or failure).

### Sequence example

Given operations: squash c2 into c1, reword c1 to "combined":
```
pick <c1>
squash <c2>
exec git commit --amend -F '.gunk-msg-0.txt'
pick <c3>
```

Git processes: pick c1 → squash c2 (combines with default behavior) → exec amend overwrites the combined message → pick c3.

## Consequences
- **No mutable state between editor invocations** — each `exec` is self-contained.
- **No new binary subcommand needed** for message editing — simplifies cross-platform concerns.
- **`GIT_SEQUENCE_EDITOR`** (todo replacement) remains the only custom editor; `GIT_EDITOR=true` is a static no-op.
- **Testable** — `build_rebase_text()` is a pure function; its output can be snapshot-tested.
- **Limitation:** If a `Reword` line has no `message_map` entry, it falls back to `reword` with `GIT_EDITOR=true`, which silently keeps the original message. This is acceptable because the plan engine always populates `message_map` for `Reword` lines.

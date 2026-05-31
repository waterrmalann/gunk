# ADR-0008: Cross-platform editor wiring via self-subcommands

**Status:** Accepted  
**Date:** 2026-05-31

## Context

`git rebase -i` delegates to external editors via `GIT_SEQUENCE_EDITOR` (for the todo list) and `GIT_EDITOR` (for commit messages during reword/squash). On Unix, it's common to wire these to shell one-liners like `cp "$PLAN" "$1"`. This breaks on Windows where shell semantics differ.

## Decision

Ship our own subcommands for editor wiring:

- `gunk --write-todo <plan-file>` — copies our generated todo file onto the path git passes as `$1`.
- `gunk --write-msg <message-file>` — copies our prepared commit message onto the editor target.

Wire these via environment variables:
```
GIT_SEQUENCE_EDITOR="gunk --write-todo /tmp/our_plan.txt"
GIT_EDITOR="gunk --write-msg /tmp/our_msg.txt"
```

These are simple file-copy operations implemented in the `app` binary, invoked by git as subprocesses.

## Consequences

- **Cross-platform** — no reliance on `cp`, `cat`, shell builtins, or POSIX shell syntax.
- **Single binary** — the subcommands are modes of the same `gunk` executable.
- **Testable** — the subcommands are trivial (copy file A to file B) and can be integration-tested.
- **No shell injection risk** — arguments are file paths, not shell-interpreted strings.

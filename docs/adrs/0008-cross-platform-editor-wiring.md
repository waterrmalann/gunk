# ADR-0008: Cross-platform editor wiring via self-subcommands

**Status:** Superseded — by ADR-0018 (todo wiring) and ADR-0019 (message feeding)  
**Date:** 2026-05-31

> **Superseded.** The `gunk --write-todo` / `gunk --write-msg` subcommands described
> below were never implemented. The cross-platform problem this ADR identified is real,
> but it was solved differently:
> - **Todo list (`GIT_SEQUENCE_EDITOR`):** instead of a binary subcommand, the execution
>   engine writes a generated POSIX shell script (`.gunk-seq-editor.sh`) that copies our
>   todo onto git's target. See ADR-0018 ("Cross-Platform Editor Wiring").
> - **Commit messages (`GIT_EDITOR`):** `GIT_EDITOR` is set to `true` (a no-op); prepared
>   messages are fed via `exec git commit --amend -F` lines injected into the rebase todo,
>   not through an editor at all. See ADR-0019, which supersedes the `GIT_EDITOR` portion
>   of this ADR.
>
> The rest of this document is retained for historical context.

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

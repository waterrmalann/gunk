# ADR-0005: Draft mode — no mutation until explicit confirm

**Status:** Accepted  
**Date:** 2026-05-31

## Context

Users need to feel safe experimenting with history cleanup. If every action immediately mutated the repo, mistakes would be costly and the undo story complex. The tool should behave more like an editor with a "save" step than a REPL that executes immediately.

## Decision

All edits accumulate as a pending `Vec<Operation>` (draft mode). The plan engine recomputes the projected history on every edit, shown as a diffed preview (added/removed/changed/reordered rows). Nothing touches the real repository until the user explicitly clicks **Confirm**.

The confirm flow:
1. Show a summary dialog of the full plan.
2. On confirm, execute the safety protocol (ADR-0004): backup → rehearsal → apply.
3. Show result (success or error with details).
4. "Discard all drafts" resets the operation list at any time.

## Consequences

- **Safe experimentation** — users can add, remove, and tweak operations freely.
- **Preview before commit** — the projected history is always visible.
- **Single mutation point** — only the Confirm path touches refs.
- **Simpler undo** — before confirm, undo is just removing operations from the list. After confirm, restore from backup ref.

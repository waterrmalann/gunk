# ADR-0023: Co-Author Trailer Management

## Status
Accepted

## Context
Git supports multiple authors on a commit via `Co-authored-by:` trailers in the commit body. These trailers follow the format `Co-authored-by: Name <email>` and are recognized by GitHub, GitLab, and other hosting platforms. Gunk already supports editing the primary author via `SetAuthor`, but co-author trailers in the body were treated as opaque text — users could accidentally corrupt them through `Reword`/`SetMessage` operations, and had no structured way to add, remove, or edit co-authors.

## Decision
Model co-authors as structured data and manage them through trailer manipulation on the commit body.

### Data model

A `CoAuthor { name, email }` struct represents a co-author identity (no timestamp — trailers don't carry one). Parsing and serialization functions operate on the commit body string:

- `parse_co_authors(body)` — extracts all `Co-authored-by:` trailers (case-insensitive prefix match, `Name <email>` parsing).
- `strip_co_author_trailers(body)` — removes all co-author lines, trims trailing whitespace.
- `set_co_authors_in_body(body, co_authors)` — strips existing trailers, appends the new set after a blank separator line.

### Operation

`Operation::SetCoAuthors { targets, co_authors }` replaces all co-author trailers on the target commits. This is a bulk-capable operation matching the pattern of `SetAuthor` and `SetMessage`. An empty `co_authors` vec removes all co-authors.

### Plan engine integration

Co-author changes are implemented as message rewrites in the rebase todo. The plan engine:

1. Collects `SetCoAuthors` operations into a `co_author_changes` map.
2. After collecting all reword operations, merges co-author changes: if a commit also has a `Reword`/`SetMessage`, co-authors are applied on top of the rewritten text; otherwise, the original commit message is used as the base.
3. The merged message is added to `message_map`, which the execution engine feeds via `exec git commit --amend -F` lines (ADR-0019).

This means co-author changes reuse the existing message-feeding infrastructure with no new execution mechanisms.

### Conflict rules

- `SetCoAuthors` conflicts with `Drop` (cannot modify a dropped commit).
- `SetCoAuthors` conflicts with absorbed squash/fixup commits.
- `SetCoAuthors` is compatible with `Reword`, `SetMessage`, and `SetAuthor` on the same commit (co-authors are layered on top of the rewritten message, and author changes are independent `--author` exec lines).

### Preview

Co-author changes show as `Reworded` status in the preview projection, since they modify the commit body.

## Consequences

- Users can add, remove, and replace co-authors on individual or bulk-selected commits through the UI.
- Co-author trailers are always well-formed — no risk of manual typos corrupting the format.
- Existing `Reword`/`SetMessage` operations compose correctly with co-author changes.
- No new execution mechanisms needed — reuses ADR-0019 message feeding.
- The `Co-authored-by:` prefix match is case-insensitive for parsing but always emits the canonical casing on write.

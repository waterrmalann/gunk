# ADR-0013: Dual-delimiter protocol for git plumbing output

**Status:** Accepted  
**Date:** 2026-05-31

## Context

ADR-0001 established that we shell out to git plumbing commands with machine-readable, NUL-delimited output. The practical challenge is that a single NUL character (`%x00` / `\0`) is not enough when a command returns multiple *records* each containing multiple *fields* — a commit has 10 fields (hash, parents, author name, author email, author date, committer name, committer email, committer date, subject, body), and `git log` returns many commits.

If we use only NUL as a separator, we cannot reliably distinguish field boundaries from record boundaries, especially when the body field may be empty or multi-line.

## Decision

Use a **dual-delimiter protocol**:

- **`%x00` (NUL, `\0`)** — field separator within a single record.
- **`%x01` (SOH, Start of Heading)** — record separator between records.

In `--pretty=format:` strings this looks like:

```
git log --pretty=format:%H%x00%P%x00%an%x00%ae%x00%aI%x00%cn%x00%ce%x00%cI%x00%s%x00%b%x01
```

For commands that return a single record type with fixed fields (e.g., `for-each-ref`), the same scheme applies:

```
git for-each-ref --format=%(refname:short)%00%(objectname)%00%(upstream:short)%01
```

The parser splits on `\x01` first (records), then on `\x00` (fields), using `splitn` with the known field count to avoid splitting on NUL characters that may appear inside the body field.

For `diff-tree -z` output (which has its own NUL-delimited format defined by git), we use git's native `-z` output directly rather than our custom protocol.

## Consequences

- **Unambiguous parsing** — field and record boundaries are always distinguishable, even with empty or multi-line fields.
- **SOH is safe** — `\x01` does not appear in commit messages, file paths, or author names in practice. It is a control character specifically designed as a separator.
- **Consistent** — all `git log` and `for-each-ref` calls use the same delimiter scheme, making the parser reusable.
- **`splitn` for safety** — the body field (last field) may contain NUL bytes in theory; using `splitn(field_count, '\0')` ensures only the first N-1 delimiters are consumed.
- **No interaction with `-z`** — git's own `-z` flag (used by `diff-tree`, `status --porcelain`) uses NUL natively; those commands use git's format, not ours.

# PLAN.md — Git History Cleanup Tool

> **For the implementing agent:** This document is the source of truth for scope, architecture, and execution order. Build it phase by phase. **Every phase is test-first (red → green → refactor).** Do not write production logic before there is a failing test that demands it. Do not skip ahead; each phase produces a working, tested vertical slice.

---

## 1. Product summary

A cross-platform desktop app that opens a Git repository, lets the user pick a branch, renders its history as a **single linear list**, and lets them clean it up safely:

1. Multi-select commits and **squash** them.
2. **Flatten a merge commit** into a single ordinary commit (so squashing is possible even when a merge is in the way).
3. **Edit commit message / description and author info**, including across multiple commits at once.
4. **Remove files from history** (and optionally append them to `.gitignore`).
5. **Reorder commits.**

Plus:

- **Draft mode**: all edits accumulate as a pending plan. Nothing touches the real repo until the user explicitly confirms. The user reviews the resulting history before it becomes permanent.
- **Multi-select** via Ctrl+click.
- **Search** by commit message, author, or filename; search results are themselves multi-selectable and **bulk-editable**.
- UX is **strictly utilitarian**: minimal, fast, intuitive, and — above all — it must make the user feel they cannot accidentally destroy their history.

### Non-goals (explicitly out of scope for v1)
- Staging/committing new work, diff editing, conflict-resolution UI beyond "stop and tell the user clearly".
- Remote operations (push/pull/fetch). The tool operates purely on local history. (It will *warn* when rewriting already-pushed commits, but will not push.)
- Hosting-provider features (PRs, issues).
- Submodule rewriting.

---

## 2. Core architectural decisions (read before coding)

### 2.1 Git is the single source of truth — shell out for everything
We do **not** reimplement history rewriting with `libgit2`/`gix` bindings. The dangerous operations (interactive rebase, history filtering) are fully and correctly implemented only in the `git` CLI and in `git-filter-repo`; reimplementing them risks corrupting a user's history.

Therefore:
- **Reads** (branch list, commit walk, diffs, file lists) go through `git` **plumbing** commands with machine-readable, NUL-delimited output (`-z`, `--pretty=format:` with `%x00` separators, `for-each-ref --format`, `diff-tree -z`, `cat-file --batch`). We parse typed structs out of that.
- **Writes** are expressed as a generated **rebase todo** fed to `git rebase -i` (via `GIT_SEQUENCE_EDITOR`/`GIT_EDITOR` overrides), or as a **`git-filter-repo`** invocation, or as low-level **plumbing** (`commit-tree`, `update-ref`).
- This gives single source of truth (the user's own `git` binary), full behavioral fidelity, fewer dependencies, and trivially reproducible operations.

`gix` may be introduced **later** purely as a read-path performance optimization for very large repos. It is not in the v1 critical path.

**Dependency note:** `git-filter-repo` is a Python script. Detect it on startup (`git filter-repo --version`). If absent, disable the "remove files from history" feature with a clear, actionable message rather than failing at apply time.

### 2.2 Separate pure logic from side effects (this is what makes TDD work)
Three layers, in a Cargo workspace:

```
git-cleanup/
├─ Cargo.toml                 # workspace
├─ crates/
│  ├─ core/                   # PURE. No IO. Domain model + plan engine. Unit-tested exhaustively.
│  ├─ gitio/                  # IO. Thin typed wrapper over the git binary. Integration-tested vs fixtures.
│  ├─ testkit/                # Test-only: RepoFixture builder + assertions. dev-dependency.
│  └─ app/                    # eframe/egui binary. Thin. Holds NO domain logic.
└─ PLAN.md
```

- `core` knows nothing about `git` or the filesystem. It takes an immutable snapshot of history (`Vec<Commit>`) plus a set of user `Operation`s, validates them, and produces a concrete **`ExecutionPlan`** (rebase todo / filter-repo spec / flatten spec). This is **pure and deterministic** → snapshot-testable to death.
- `gitio` reads snapshots and executes `ExecutionPlan`s. Side-effecting → tested against real throwaway repos.
- `app` is a thin egui shell that holds UI state, calls `core` to (re)compute the plan on every edit, and calls `gitio` only on **Confirm**. **No business logic lives in egui callbacks.** UI state transitions are modeled as a testable reducer (`State × Msg → State`), so they're unit-testable without a window.

### 2.3 Safety model (the "without fear of breaking something" requirement)
Every mutating apply follows this protocol, implemented in `gitio` and covered by integration tests:

1. **Refuse on dirty tree.** Check `git status --porcelain=v2 -z`. If the working tree/index is dirty, refuse and offer to auto-stash (`git stash push -u`) with explicit user consent; restore on completion/abort.
2. **Backup ref.** Before any rewrite, create `refs/cleanup/backup/<branch>/<unix-ts>` pointing at the current branch tip. The UI exposes "Restore from backup" which is just `update-ref` back. The reflog is the secondary safety net.
3. **Rehearse in a throwaway worktree.** `git worktree add --detach <tmpdir> <branch-tip>`, run the full plan there first. If it conflicts or fails, surface it and **do not touch the real branch**. If it succeeds, fast-forward the real branch ref to the rehearsed result (or replay). Tear down the worktree afterward, always (RAII guard).
4. **Atomic-ish commit.** The real branch ref is only moved once the rehearsal proves the plan applies cleanly.
5. **Push warning.** If the oldest rewritten commit is reachable from any remote-tracking ref, warn that this rewrites published history.

The draft-mode preview is computed from `core` (the projected history) and optionally validated by a rehearsal; nothing in draft mode mutates real refs.

---

## 3. Domain model (target shape for `core`)

The agent should converge on something like this; exact field names may evolve, but **operations must be modeled as data**, never as imperative git calls scattered through the UI.

```rust
struct CommitId(String);            // full 40/64-char oid

struct Commit {
    id: CommitId,
    parents: Vec<CommitId>,         // len >= 2 ⇒ merge commit
    author: Identity,               // name, email, time
    committer: Identity,
    summary: String,                // subject line
    body: String,                   // remainder of message
    changed_paths: Vec<PathChange>, // for search-by-filename + file removal UI
}

struct Identity { name: String, email: String, time: OffsetDateTime }

/// One user intent captured in draft mode. The plan engine turns a set of these
/// into a concrete ExecutionPlan. Operations are commutative-checked and validated.
enum Operation {
    Reword     { target: CommitId, summary: String, body: String },
    SetAuthor  { targets: Vec<CommitId>, author: Identity },   // bulk-capable
    SetMessage { targets: Vec<CommitId>, summary: String, body: String }, // bulk reword
    Squash     { keep: CommitId, absorb: Vec<CommitId> },      // absorb messages into keep
    Fixup      { keep: CommitId, absorb: Vec<CommitId> },      // discard absorbed messages
    Drop       { target: CommitId },
    Reorder    { new_order: Vec<CommitId> },                   // permutation of the range
    RemovePaths{ paths: Vec<PathSpec>, add_to_gitignore: bool },
    FlattenMerge { merge: CommitId },                          // see §6
}

/// Pure output of the plan engine. Deterministic. Snapshot-tested.
enum ExecutionPlan {
    Rebase(RebaseTodo),        // pick/squash/fixup/reword/drop/exec lines + message & author maps
    FilterRepo(FilterRepoSpec),// path removal
    Flatten(FlattenSpec),      // plumbing-level merge flattening
    Composite(Vec<ExecutionPlan>), // ordered; e.g. flatten THEN rebase-squash
}
```

The plan engine (`core::plan(snapshot, operations) -> Result<ExecutionPlan, PlanError>`) is the heart of the app and the most heavily tested unit. It must:
- Validate that targets exist and are within the editable range.
- Auto-reorder when a squash requires adjacency, or reject with a clear `PlanError` if impossible.
- Detect contradictory operations (e.g., drop + reword same commit).
- Decide whether file removal forces a `filter-repo` pass and how it composes with rebase ops.
- Be **idempotent** and order-independent w.r.t. how the user entered edits.

---

## 4. Git mechanics reference (so the agent doesn't guess)

**Reads (gitio):**
- Branches: `git for-each-ref --format='%(refname:short)%00%(objectname)%00%(upstream)' refs/heads`
- Linear walk of a branch: `git log --first-parent? <branch> --pretty=format:'%H%x00%P%x00%an%x00%ae%x00%aI%x00%s%x00%b' -z` — note: keep merges visible (do **not** use `--first-parent` for the main list; the user needs to see merges to flatten them).
- Changed paths per commit: `git diff-tree --no-commit-id --name-status -r -z <oid>` (handle the root commit: diff against the empty tree).
- Full diff for the detail pane: `git show --format= -p <oid>` (lazy-loaded, never block the list render).

**Rebase execution (gitio):**
- Generate a todo file (`pick`/`reword`/`squash`/`fixup`/`drop`/`exec` lines) from `RebaseTodo`.
- Run `git -c core.editor=… rebase -i <base>` with:
  - `GIT_SEQUENCE_EDITOR` set to a command that **overwrites** the todo path git hands it with our generated todo. Cross-platform: do **not** rely on `cp`. Ship a tiny self-subcommand, e.g. `git-cleanup --write-todo <our_plan_file>`, that copies our file onto `$1`. Wire it in via the env var.
  - `GIT_EDITOR` similarly set to feed prepared commit messages (for `reword`/`squash`) from a message map keyed by a marker, again via a self-subcommand.
- Bulk **author** changes on selected commits: emit `exec git commit --amend --no-edit --author="Name <email>"` lines after the relevant picks (or `--reset-author` for committer). Prefer this over filter-repo when the change is scoped to selected commits.

**File removal (gitio):**
- `git filter-repo --invert-paths --path <p1> --path <p2> …` (or `--path-glob`). This rewrites the whole branch history; compose it correctly with any rebase ops (run filter-repo first, then re-derive the snapshot, then rebase).
- If `add_to_gitignore`, append the paths to `.gitignore` as a final ordinary commit (or amend), and ensure they're untracked going forward.

**Merge flatten (gitio, plumbing):** see §6.

---

## 5. Execution phases (each is test-first)

> Definition of done for **every** phase: failing tests written first; all tests green; `cargo clippy -- -D warnings` clean; `cargo fmt` clean; the phase's vertical slice is demonstrably usable.

### Phase 0 — Scaffolding & test harness
- Create the workspace and four crates per §2.2.
- Set up CI (fmt, clippy -D warnings, test) — Linux/macOS/Windows matrix.
- **Build `testkit::RepoFixture` first.** A builder that scripts a throwaway repo in a tempdir: `.commit("msg", files)`, `.branch()`, `.merge(...)`, `.commit_by(author)`, returns oids. This is the foundation of all integration tests. Write tests *for the fixture builder itself*.
- Decide and document the git-invocation wrapper (capture stdout/stderr, NUL parsing, error typing) as a stub with tests.

### Phase 1 — Read-only history (vertical slice: "open a repo, see the list")
- TDD `gitio`: open repo, list branches, walk a branch into `Vec<Commit>`, parse author/parents/summary/changed-paths from plumbing output. Test against fixtures with: root commit, normal commits, a merge, unicode messages, commits with empty bodies.
- TDD the lazy diff loader.
- `app`: minimal egui window — open-folder dialog, branch dropdown, scrollable linear commit list (sha short, summary, author, relative date; merges visually marked). Detail pane shows message + changed files + diff on selection.
- No editing yet.

### Phase 2 — Selection & search (pure state logic)
- TDD the UI **reducer** in isolation: single select, **Ctrl+click multi-select** (toggle), shift-range select, clear.
- TDD a search index over message/author/path; filtering returns commit ids; results are selectable and feed the same selection set so bulk edits apply to them.
- Wire reducer into egui. Search box + result highlighting. "Select all results" affordance.

### Phase 3 — Draft/plan engine (pure `core`, no execution)
- TDD `core::plan(...)` exhaustively with **snapshot tests** (`insta`) of the generated `ExecutionPlan` for: reword, bulk set-message, bulk set-author, squash (adjacent), squash (requiring auto-reorder), fixup, drop, reorder, conflicting ops (→ `PlanError`).
- Property tests (`proptest`) for invariants: reordering is a permutation; every non-dropped commit appears exactly once; plan generation is order-independent.
- `app`: edits mutate a pending `Vec<Operation>` and recompute the projected history shown as a **diffed preview** (added/removed/changed/reordered rows). Still nothing applied. "Discard all drafts" resets.

### Phase 4 — Execution engine & safety (the trust layer)
- TDD `gitio` apply for the **rebase-based** plans against fixtures: dirty-tree refusal + opt-in stash, backup ref creation, **worktree rehearsal**, apply-on-success, conflict surfacing, RAII worktree teardown, restore-from-backup.
- Tests must assert resulting history (oids' shape, messages, authors, parentage) and that on simulated failure the **real branch is untouched** and the backup ref exists.
- `app`: explicit **Confirm** dialog summarizing the plan; progress + result; visible "Restore backup" entry point.

### Phase 5 — Wire the rebase-class features end to end
For each, write fixture integration tests first, then connect UI:
- **Reword / bulk edit message & author** across multiple selected (incl. selected-from-search).
- **Squash / fixup** multi-select.
- **Reorder** (drag in the list; reducer already tested in P2/P3).
- **Drop**.

### Phase 6 — Remove files from history
- Detect `git-filter-repo`; gate the feature on its presence.
- TDD removal against fixtures: single path, glob, path that exists only in old commits, binary file, `add_to_gitignore` behavior.
- TDD **composition** with rebase ops (filter-repo first → re-snapshot → rebase) inside the same Confirm/rehearse/backup envelope.
- UI: from a commit's changed-files list or from search-by-filename, select files → "Remove from all history" + ".gitignore" checkbox.

### Phase 7 — Flatten merge (highest risk; most tests) — see §6
- TDD the plumbing flatten in isolation, then its composition with squashing.

### Phase 8 — UX polish & hardening
- Keyboard-first interactions; clear empty/error/conflict states; "this rewrites pushed history" warning; large-repo behavior (virtualized list, lazy diffs, bounded initial walk with "load more").
- Final pass: every destructive path has a backup ref + a tested restore.

---

## 6. Flatten-merge design (call this out — it's the trickiest feature)

**Goal:** replace a merge commit `M` (parents `P1` = mainline, `P2` = side) with a single ordinary commit `M'` that (a) has the exact resulting tree of `M`, and (b) has a single parent on the mainline. After this, the range is linear and ordinary squashing works.

**Robust low-level approach (no patch/apply, no conflicts):**
1. Take `M`'s tree as-is: `T = M^{tree}`.
2. Create a new commit reusing that tree, parented on mainline:
   `git commit-tree T -p P1 -m "<flattened message>"` → `M'`.
   This guarantees the post-flatten tree is byte-identical to the merge result.
3. Rebase everything that was after `M` onto `M'` (the rehearsal worktree handles replaying descendants).
4. The side branch's individual commits are intentionally collapsed into `M'` (that's the point of "flatten into a single commit").

Model this as `ExecutionPlan::Flatten(FlattenSpec)`, and when the user also squashes across the (now-removed) merge, emit `ExecutionPlan::Composite([Flatten, Rebase])` so the engine guarantees flatten happens first.

**Tests must cover:** merge with conflicts originally resolved (tree must still match `M`), fast-forward-only "merges", octopus merges (≥3 parents → either support or reject with a clear `PlanError`; v1 may reject), a merge that is the branch tip, and a merge in the middle followed by a squash spanning it.

---

## 7. Testing strategy (TDD is mandatory)

- **Red → green → refactor**, always. A reviewer should be able to read the git log and see the test commit precede the implementation commit.
- **`core`**: pure unit + `insta` snapshot tests of `ExecutionPlan` + `proptest` invariants. Fast, no IO. This is where coverage must be near-total.
- **`gitio`**: integration tests against `testkit::RepoFixture` real repos in tempdirs. Assert resulting history shape, not stdout strings. Always assert the **safety net**: backup ref exists, real branch untouched on failure.
- **`app`**: test the reducer (`State × Msg → State`) as plain functions. egui rendering itself is not unit-tested; keep it logic-free so there's nothing to test there.
- **Determinism**: pin author/committer dates and identities in fixtures (`GIT_AUTHOR_DATE`, `GIT_COMMITTER_DATE`, env identities) so oids/snapshots are stable across machines and CI.
- **Cross-platform**: CI on Linux, macOS, Windows. No reliance on shell builtins; the sequence/message editors are our own subcommands (§4) precisely so Windows works.
- Recommended dev-deps: `assert_fs`, `tempfile`, `assert_cmd`, `predicates`, `insta`, `proptest`.

---

## 8. Tech stack

- **Language:** Rust (stable).
- **GUI:** `eframe` / `egui` (immediate-mode; utilitarian; single binary).
- **Git:** the user's `git` binary (required) + `git-filter-repo` (optional, gates file removal). `gix` optional later for read perf only.
- **Serialization:** `serde` (persist/inspect plans; useful for debugging and snapshot tests).
- **Errors:** `thiserror` for typed errors in `core`/`gitio`; surface user-facing messages distinctly from internal ones.
- **Time:** `time` (`OffsetDateTime`) for identities.
- **Async:** not required for v1; run git on a worker thread to keep the UI responsive, communicate via channel. Keep this in `app` only.

---

## 9. Risk register

| Risk | Mitigation |
|---|---|
| Rewriting corrupts/loses history | Backup refs + reflog + rehearsal worktree; real branch moved only after clean rehearsal. |
| Merge flatten edge cases (octopus, conflicts) | Plumbing `commit-tree` reusing the merge tree; reject octopus in v1 with clear error; heavy fixture tests (§6). |
| `git-filter-repo` not installed | Detect on startup; gate feature; actionable message. |
| Cross-platform editor wiring | Use our own `--write-todo` / `--write-msg` subcommands, never shell builtins. |
| Rebase conflicts mid-apply | Rehearse first; on conflict, abort cleanly and report; never leave the user in a detached rebase state on their real branch. |
| Large repos slow/janky | Bounded initial walk + "load more", virtualized list, lazy diffs, git on worker thread; consider `gix` later. |
| Rewriting already-pushed commits | Detect reachability from remote-tracking refs; warn before apply. |
| Logic leaking into egui (untestable) | Reducer pattern; `app` holds no domain logic; enforced in review. |

---

## 10. Definition of done (project)

- All five features work end to end, each backed by passing fixture integration tests.
- Draft mode never mutates the real repo; Confirm is the only mutation point; every mutation leaves a restorable backup ref.
- Ctrl+click multi-select, shift-range, and search-result multi-select all feed one selection model used by bulk edits.
- `cargo test` green on Linux/macOS/Windows; `clippy -D warnings` and `fmt` clean.
- A new user can open a messy repo, squash across a flattened merge, bulk-fix authors, strip a secret file from history, reorder, review the preview, confirm, and — if they panic — restore from backup, all without the terminal.
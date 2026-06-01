// Hide the console window on Windows release builds (portable GUI app).
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;
use std::time::Duration;

use eframe::egui;
use egui_phosphor::regular as icon;
use gunk_core::{
    ChangeStatus, CoAuthor, Commit, CommitId, DraftMsg, DraftState, Identity, PathChange, PathSpec,
    PreviewRow, RowStatus, SearchHit, SelectionMsg, SelectionState, parse_co_authors, plan,
    preview, search_commits, search_hit_indices,
};
use gunk_gitio::{
    BranchInfo, ExecuteResult, Git, execute_plan as gitio_execute_plan, has_filter_repo,
    list_backup_refs, restore_backup,
};
use std::collections::HashMap;

const COMMIT_PAGE_SIZE: usize = 500;

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("gunk — git history cleanup")
            .with_inner_size([1100.0, 700.0]),
        ..Default::default()
    };
    eframe::run_native(
        "gunk",
        options,
        Box::new(|cc| {
            let mut fonts = egui::FontDefinitions::default();
            egui_phosphor::add_to_fonts(&mut fonts, egui_phosphor::Variant::Regular);
            cc.egui_ctx.set_fonts(fonts);
            Ok(Box::new(App::default()))
        }),
    )
}

// ── State ──────────────────────────────────────────────────────────

/// The loaded repository state.
struct RepoState {
    git: Git,
    branches: Vec<BranchInfo>,
    selected_branch: usize,
    commits: Vec<Commit>,
    /// Maps each commit's id to its index in `commits`. Cached so the
    /// reorder-aware preview render does not rebuild it O(N) every frame;
    /// rebuilt only when `commits` changes via `rebuild_commit_index`.
    commit_index: HashMap<CommitId, usize>,
    has_more_commits: bool,
    /// Total commits reachable from the branch tip (for the "loaded of total" label).
    total_commits: usize,
    /// Commit selection (multi-select via Ctrl/Shift).
    selection: SelectionState,
    /// Lazily loaded detail for the focused commit (last clicked).
    detail: Option<CommitDetail>,
    /// Current search query text.
    search_query: String,
    /// Cached search results (recomputed when query changes).
    search_hits: Vec<SearchHit>,
    /// Indices of commits matching the current search (for fast lookup).
    search_match_indices: std::collections::BTreeSet<usize>,
    /// Pending draft operations (nothing applied to the real repo).
    draft: DraftState,
    /// Projected preview rows for the current draft (display order, newest-first).
    preview_rows: Vec<PreviewRow>,
    /// Validation error from the plan engine for the current draft, if any.
    plan_error: Option<String>,
    /// Edit buffer for the reword summary (single-select detail pane).
    reword_summary: String,
    /// Edit buffer for the reword body.
    reword_body: String,
    /// Edit buffer for the author name (set-author).
    author_name: String,
    /// Edit buffer for the author email (set-author).
    author_email: String,
    /// Edit buffers for co-author management.
    co_author_name: String,
    co_author_email: String,
    /// Whether the confirmation dialog is visible.
    show_confirm_dialog: bool,
    /// Result of the last plan execution (success or error message).
    execute_result: Option<Result<ExecuteResult, String>>,
    /// Whether the restore-from-backup panel is open.
    show_restore_panel: bool,
    /// Cached backup refs for the current branch.
    backup_refs: Vec<(String, String)>,
    /// Whether git-filter-repo is available (gates file removal feature).
    filter_repo_available: bool,
    /// Files selected for removal from history (paths toggled via checkboxes).
    files_selected_for_removal: std::collections::BTreeSet<String>,
    /// Whether to add removed files to .gitignore.
    add_to_gitignore: bool,
}

impl RepoState {
    /// Rebuild the `commit_index` lookup. Call after any mutation of `commits`.
    fn rebuild_commit_index(&mut self) {
        self.commit_index = self
            .commits
            .iter()
            .enumerate()
            .map(|(i, c)| (c.id.clone(), i))
            .collect();
    }
}

struct CommitDetail {
    changed_paths: Vec<PathChange>,
    /// Diff split into lines once at load time so the diff pane can render
    /// virtualized rows (`ScrollArea::show_rows`) without re-splitting per frame.
    diff_lines: Vec<String>,
}

impl CommitDetail {
    fn from_diff(changed_paths: Vec<PathChange>, diff: String) -> Self {
        let diff_lines = diff.lines().map(str::to_string).collect();
        Self {
            changed_paths,
            diff_lines,
        }
    }
}

enum PendingLoadKind {
    OpenRepo,
    SwitchBranch { new_idx: usize },
}

struct PendingLoad {
    request_id: u64,
    kind: PendingLoadKind,
    message: String,
}

struct ExecOutcome {
    exec_result: ExecuteResult,
    commits: Vec<Commit>,
    has_more: bool,
    total_commits: usize,
}

struct ExecResponse {
    request_id: u64,
    result: Result<ExecOutcome, String>,
}

struct PendingExec {
    request_id: u64,
    message: String,
}

struct DetailOutcome {
    /// Commit index the detail was loaded for, so a stale response for a
    /// commit the user has since navigated away from can be discarded.
    idx: usize,
    changed_paths: Vec<PathChange>,
    diff: String,
}

struct DetailResponse {
    request_id: u64,
    result: Result<DetailOutcome, String>,
}

struct PendingDetail {
    request_id: u64,
}

struct RepoLoadSnapshot {
    git: Git,
    branches: Vec<BranchInfo>,
    commits: Vec<Commit>,
    has_more_commits: bool,
    total_commits: usize,
    filter_repo_available: bool,
}

enum LoadOutcome {
    OpenRepo(RepoLoadSnapshot),
    SwitchBranch {
        new_idx: usize,
        commits: Vec<Commit>,
        has_more_commits: bool,
        total_commits: usize,
    },
}

struct LoadResponse {
    request_id: u64,
    result: Result<LoadOutcome, String>,
}

/// Top-level app state.
#[derive(Default)]
struct App {
    repo: Option<RepoState>,
    error: Option<String>,
    load_rx: Option<Receiver<LoadResponse>>,
    pending_load: Option<PendingLoad>,
    exec_rx: Option<Receiver<ExecResponse>>,
    pending_exec: Option<PendingExec>,
    detail_rx: Option<Receiver<DetailResponse>>,
    pending_detail: Option<PendingDetail>,
    next_request_id: u64,
}

fn read_commit_window(
    git: &Git,
    branch: &str,
    desired_count: usize,
) -> Result<(Vec<Commit>, bool, usize), gunk_gitio::GitError> {
    let page = git.walk_commits_page(branch, 0, desired_count.max(COMMIT_PAGE_SIZE))?;
    let total = git.count_commits(branch)?;
    Ok((page.commits, page.has_more, total))
}

fn commit_count_label(loaded_count: usize, total: usize) -> String {
    if loaded_count >= total {
        format!("{loaded_count} commits")
    } else {
        format!("{loaded_count} of {total} commits loaded")
    }
}

// ── Logic (no UI) ──────────────────────────────────────────────────

impl App {
    fn open_repo(&mut self, path: PathBuf) {
        self.error = None;
        let request_id = self.next_request_id;
        self.next_request_id = self.next_request_id.saturating_add(1);
        let message = format!("Loading repository: {}", path.display());

        let (tx, rx) = mpsc::channel();
        self.load_rx = Some(rx);
        self.pending_load = Some(PendingLoad {
            request_id,
            kind: PendingLoadKind::OpenRepo,
            message,
        });

        thread::spawn(move || {
            let result = (|| -> Result<LoadOutcome, String> {
                let git = Git::open(&path).map_err(|e| format!("Failed to open repo: {e}"))?;
                let branches = git
                    .list_branches()
                    .map_err(|e| format!("Failed to list branches: {e}"))?;
                if branches.is_empty() {
                    return Err("Repository has no branches.".to_string());
                }

                let (commits, has_more_commits, total_commits) =
                    read_commit_window(&git, &branches[0].name, COMMIT_PAGE_SIZE)
                        .map_err(|e| format!("Failed to read commits: {e}"))?;

                Ok(LoadOutcome::OpenRepo(RepoLoadSnapshot {
                    filter_repo_available: has_filter_repo(&git),
                    git,
                    branches,
                    commits,
                    has_more_commits,
                    total_commits,
                }))
            })();

            let _ = tx.send(LoadResponse { request_id, result });
        });
    }

    fn start_branch_switch_load(&mut self, new_idx: usize) {
        let Some(repo) = &self.repo else { return };
        if new_idx == repo.selected_branch {
            return;
        }

        let git = repo.git.clone();
        let branch_name = repo.branches[new_idx].name.clone();
        let request_id = self.next_request_id;
        self.next_request_id = self.next_request_id.saturating_add(1);

        let (tx, rx) = mpsc::channel();
        self.load_rx = Some(rx);
        self.pending_load = Some(PendingLoad {
            request_id,
            kind: PendingLoadKind::SwitchBranch { new_idx },
            message: format!("Loading branch: {branch_name}"),
        });

        thread::spawn(move || {
            let result = read_commit_window(&git, &branch_name, COMMIT_PAGE_SIZE)
                .map(
                    |(commits, has_more_commits, total_commits)| LoadOutcome::SwitchBranch {
                        new_idx,
                        commits,
                        has_more_commits,
                        total_commits,
                    },
                )
                .map_err(|e| format!("Failed to read commits: {e}"));

            let _ = tx.send(LoadResponse { request_id, result });
        });
    }

    fn poll_background_load(&mut self) {
        let response = {
            let Some(rx) = &self.load_rx else { return };
            match rx.try_recv() {
                Ok(r) => Some(r),
                Err(TryRecvError::Empty) => None,
                Err(TryRecvError::Disconnected) => {
                    self.load_rx = None;
                    self.pending_load = None;
                    self.error = Some("Background loading failed unexpectedly.".to_string());
                    None
                }
            }
        };

        let Some(response) = response else { return };

        let Some(pending) = self.pending_load.take() else {
            return;
        };
        if pending.request_id != response.request_id {
            return;
        }
        self.load_rx = None;

        match (pending.kind, response.result) {
            (_, Err(err)) => {
                self.error = Some(err);
            }
            (PendingLoadKind::OpenRepo, Ok(LoadOutcome::OpenRepo(snapshot))) => {
                let selection = SelectionState::new(snapshot.commits.len());
                let mut state = RepoState {
                    git: snapshot.git,
                    branches: snapshot.branches,
                    selected_branch: 0,
                    commits: snapshot.commits,
                    commit_index: HashMap::new(),
                    has_more_commits: snapshot.has_more_commits,
                    total_commits: snapshot.total_commits,
                    selection,
                    detail: None,
                    search_query: String::new(),
                    search_hits: Vec::new(),
                    search_match_indices: std::collections::BTreeSet::new(),
                    draft: DraftState::new(),
                    preview_rows: Vec::new(),
                    plan_error: None,
                    reword_summary: String::new(),
                    reword_body: String::new(),
                    author_name: String::new(),
                    author_email: String::new(),
                    co_author_name: String::new(),
                    co_author_email: String::new(),
                    show_confirm_dialog: false,
                    execute_result: None,
                    show_restore_panel: false,
                    backup_refs: Vec::new(),
                    filter_repo_available: snapshot.filter_repo_available,
                    files_selected_for_removal: std::collections::BTreeSet::new(),
                    add_to_gitignore: false,
                };
                state.rebuild_commit_index();
                self.repo = Some(state);
            }
            (
                PendingLoadKind::SwitchBranch {
                    new_idx: expected_idx,
                },
                Ok(LoadOutcome::SwitchBranch {
                    new_idx,
                    commits,
                    has_more_commits,
                    total_commits,
                }),
            ) => {
                if expected_idx != new_idx {
                    self.error =
                        Some("Background branch load result was inconsistent.".to_string());
                    return;
                }
                if let Some(repo) = &mut self.repo {
                    repo.selected_branch = new_idx;
                    repo.selection = SelectionState::new(commits.len());
                    repo.commits = commits;
                    repo.rebuild_commit_index();
                    repo.has_more_commits = has_more_commits;
                    repo.total_commits = total_commits;
                    repo.detail = None;
                    repo.search_query.clear();
                    repo.search_hits.clear();
                    repo.search_match_indices.clear();
                    repo.draft = DraftState::new();
                    repo.preview_rows.clear();
                    repo.plan_error = None;
                }
            }
            _ => {
                self.error = Some("Background load result did not match request type.".to_string());
            }
        }
    }

    fn poll_background_exec(&mut self) {
        let response = {
            let Some(rx) = &self.exec_rx else { return };
            match rx.try_recv() {
                Ok(r) => Some(r),
                Err(TryRecvError::Empty) => None,
                Err(TryRecvError::Disconnected) => {
                    self.exec_rx = None;
                    self.pending_exec = None;
                    self.error = Some("Background execution failed unexpectedly.".to_string());
                    None
                }
            }
        };

        let Some(response) = response else { return };
        let Some(pending) = self.pending_exec.take() else {
            return;
        };
        if pending.request_id != response.request_id {
            return;
        }
        self.exec_rx = None;

        match response.result {
            Err(err) => {
                if let Some(repo) = &mut self.repo {
                    repo.execute_result = Some(Err(err));
                }
            }
            Ok(outcome) => {
                if let Some(repo) = &mut self.repo {
                    repo.execute_result = Some(Ok(outcome.exec_result));
                    repo.selection = SelectionState::new(outcome.commits.len());
                    repo.commits = outcome.commits;
                    repo.rebuild_commit_index();
                    repo.has_more_commits = outcome.has_more;
                    repo.total_commits = outcome.total_commits;
                    repo.draft = DraftState::new();
                    repo.preview_rows.clear();
                    repo.plan_error = None;
                    repo.detail = None;
                    repo.search_query.clear();
                    repo.search_hits.clear();
                    repo.search_match_indices.clear();
                    repo.reword_summary.clear();
                    repo.reword_body.clear();
                    repo.author_name.clear();
                    repo.author_email.clear();
                    repo.co_author_name.clear();
                    repo.co_author_email.clear();
                    repo.files_selected_for_removal.clear();
                    repo.add_to_gitignore = false;
                }
            }
        }
    }

    fn apply_selection_msg(&mut self, msg: SelectionMsg) {
        if let Some(repo) = &mut self.repo {
            repo.selection = repo.selection.reduce(msg);
            // Load detail for the last-clicked commit (if exactly one or the most recent click)
            self.load_detail_for_focus();
        }
    }

    /// Kick off loading the detail (changed paths + diff) for the focused
    /// commit on a background thread. `git show -p` can be very slow for fat
    /// commits, so it must never run on the UI thread. The latest request wins:
    /// stale responses are discarded by `request_id` in `poll_background_detail`.
    fn load_detail_for_focus(&mut self) {
        let Some(repo) = &self.repo else { return };

        // Show detail for the single selected commit only.
        let focus = if repo.selection.len() == 1 {
            repo.selection.selected.iter().next().copied()
        } else {
            None
        };

        let Some(idx) = focus else {
            // Multi-select or empty: clear detail and cancel any in-flight load.
            if let Some(repo) = &mut self.repo {
                repo.detail = None;
            }
            self.pending_detail = None;
            self.detail_rx = None;
            return;
        };

        let oid = repo.commits[idx].id.0.clone();
        let git = repo.git.clone();
        let request_id = self.next_request_id;
        self.next_request_id = self.next_request_id.saturating_add(1);

        // Clear stale detail so the pane shows a loading state, not the
        // previously-focused commit's diff.
        if let Some(repo) = &mut self.repo {
            repo.detail = None;
        }

        let (tx, rx) = mpsc::channel();
        self.detail_rx = Some(rx);
        self.pending_detail = Some(PendingDetail { request_id });

        thread::spawn(move || {
            let result = (|| -> Result<DetailOutcome, String> {
                let changed_paths = git.changed_paths(&oid).map_err(|e| format!("{e}"))?;
                let diff = git.show_diff(&oid).map_err(|e| format!("{e}"))?;
                Ok(DetailOutcome {
                    idx,
                    changed_paths,
                    diff,
                })
            })();
            let _ = tx.send(DetailResponse { request_id, result });
        });
    }

    fn poll_background_detail(&mut self) {
        let response = {
            let Some(rx) = &self.detail_rx else { return };
            match rx.try_recv() {
                Ok(r) => Some(r),
                Err(TryRecvError::Empty) => None,
                Err(TryRecvError::Disconnected) => {
                    self.detail_rx = None;
                    self.pending_detail = None;
                    None
                }
            }
        };

        let Some(response) = response else { return };
        let Some(pending) = &self.pending_detail else {
            return;
        };
        if pending.request_id != response.request_id {
            // A newer request superseded this one; ignore the stale result.
            return;
        }
        self.pending_detail = None;
        self.detail_rx = None;

        match response.result {
            Ok(outcome) => {
                if let Some(repo) = &mut self.repo {
                    // Only apply if the focused commit is still the one we loaded.
                    let still_focused = repo.selection.len() == 1
                        && repo.selection.selected.iter().next().copied() == Some(outcome.idx);
                    if still_focused {
                        repo.detail =
                            Some(CommitDetail::from_diff(outcome.changed_paths, outcome.diff));
                    }
                }
            }
            Err(err) => {
                self.error = Some(format!("Failed to load commit detail: {err}"));
            }
        }
    }

    fn apply_draft_msg(&mut self, msg: DraftMsg) {
        if let Some(repo) = &mut self.repo {
            repo.draft = repo.draft.reduce(msg);
        }
        self.recompute_preview();
    }

    /// Recompute the projected preview from the current draft.
    ///
    /// Stores the rows on success, or the plan-engine error string on failure
    /// (leaving the previous rows in place so the list does not flicker empty).
    fn recompute_preview(&mut self) {
        if let Some(repo) = &mut self.repo {
            if repo.draft.is_empty() {
                repo.preview_rows.clear();
                repo.plan_error = None;
                return;
            }
            match preview(&repo.commits, &repo.draft.ops) {
                Ok(rows) => {
                    repo.preview_rows = rows;
                    repo.plan_error = None;
                }
                Err(e) => {
                    repo.plan_error = Some(format!("{e}"));
                }
            }
        }
    }

    fn load_more_commits(&mut self) {
        let mut load_error = None;
        let mut refresh_search = false;
        let mut refresh_preview = false;

        if let Some(repo) = &mut self.repo {
            if !repo.has_more_commits {
                return;
            }

            let branch_name = repo.branches[repo.selected_branch].name.clone();
            let loaded_count = repo.commits.len();
            match repo
                .git
                .walk_commits_page(&branch_name, loaded_count, COMMIT_PAGE_SIZE)
            {
                Ok(page) => {
                    repo.commits.extend(page.commits);
                    repo.rebuild_commit_index();
                    repo.has_more_commits = page.has_more;
                    repo.selection.count = repo.commits.len();
                    refresh_search = !repo.search_query.is_empty();
                    refresh_preview = !repo.draft.is_empty();
                }
                Err(e) => load_error = Some(format!("Failed to load more commits: {e}")),
            }
        }

        if let Some(err) = load_error {
            self.error = Some(err);
        }
        if refresh_search {
            self.update_search();
        }
        if refresh_preview {
            self.recompute_preview();
        }
    }

    fn update_search(&mut self) {
        if let Some(repo) = &mut self.repo {
            if repo.search_query.is_empty() {
                repo.search_hits.clear();
                repo.search_match_indices.clear();
            } else {
                repo.search_hits = search_commits(&repo.commits, &repo.search_query);
                repo.search_match_indices = search_hit_indices(&repo.search_hits);
            }
        }
    }

    /// Execute the current draft plan against the repository.
    ///
    /// Delegates composite OID remapping entirely to `gitio::execute_plan`
    /// (which handles flatten → filter-repo → rebase ordering with correct
    /// OID retargeting between phases). The UI thread only calls `plan()` to
    /// validate; the actual git work runs on a background thread (M2).
    fn execute_plan(&mut self) {
        let Some(repo) = &mut self.repo else { return };
        if self.pending_exec.is_some() {
            return;
        }

        let branch = repo.branches[repo.selected_branch].name.clone();

        // Plan on the UI thread — pure + fast; surface errors inline.
        let exec_plan = match plan(&repo.commits, &repo.draft.ops) {
            Ok(p) => p,
            Err(e) => {
                repo.execute_result = Some(Err(format!("Plan error: {e}")));
                return;
            }
        };

        // Move owned data into the worker thread.
        let git = repo.git.clone();
        let loaded_count = repo.commits.len();
        let request_id = self.next_request_id;
        self.next_request_id = self.next_request_id.saturating_add(1);

        let (tx, rx) = mpsc::channel();
        self.exec_rx = Some(rx);
        self.pending_exec = Some(PendingExec {
            request_id,
            message: format!("Applying {n} operation(s)…", n = repo.draft.len()),
        });

        thread::spawn(move || {
            let result = (|| -> Result<ExecOutcome, String> {
                let exec_result =
                    gitio_execute_plan(&git, &branch, &exec_plan).map_err(|e| format!("{e}"))?;
                let (commits, has_more, total_commits) =
                    read_commit_window(&git, &branch, loaded_count)
                        .map_err(|e| format!("Failed to reload commits: {e}"))?;
                Ok(ExecOutcome {
                    exec_result,
                    commits,
                    has_more,
                    total_commits,
                })
            })();
            let _ = tx.send(ExecResponse { request_id, result });
        });
    }

    /// Load backup refs for the current branch.
    fn load_backup_refs(&mut self) {
        if let Some(repo) = &mut self.repo {
            let branch = &repo.branches[repo.selected_branch].name;
            match list_backup_refs(&repo.git, branch) {
                Ok(refs) => repo.backup_refs = refs,
                Err(e) => self.error = Some(format!("Failed to list backups: {e}")),
            }
        }
    }

    /// Restore the current branch from a backup ref.
    fn restore_from_backup(&mut self, backup_ref: &str) {
        let Some(repo) = &mut self.repo else { return };

        let branch = repo.branches[repo.selected_branch].name.clone();
        match restore_backup(&repo.git, &branch, backup_ref) {
            Ok(()) => {
                // Reload commits after restore.
                repo.draft = DraftState::new();
                repo.preview_rows.clear();
                repo.plan_error = None;
                repo.detail = None;
                repo.execute_result = None;
                match read_commit_window(&repo.git, &branch, repo.commits.len()) {
                    Ok((commits, has_more_commits, total_commits)) => {
                        repo.selection = SelectionState::new(commits.len());
                        repo.commits = commits;
                        repo.rebuild_commit_index();
                        repo.has_more_commits = has_more_commits;
                        repo.total_commits = total_commits;
                    }
                    Err(e) => {
                        repo.selection = SelectionState::new(0);
                        repo.commits.clear();
                        repo.rebuild_commit_index();
                        repo.has_more_commits = false;
                        repo.total_commits = 0;
                        self.error = Some(format!("Failed to reload commits: {e}"));
                    }
                }
            }
            Err(e) => self.error = Some(format!("Restore failed: {e}")),
        }
    }
}

// ── UI ─────────────────────────────────────────────────────────────

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_background_load();
        self.poll_background_exec();
        self.poll_background_detail();
        if self.pending_load.is_some()
            || self.pending_exec.is_some()
            || self.pending_detail.is_some()
        {
            ctx.request_repaint_after(Duration::from_millis(50));
        }

        let mut request_load_more = false;
        let mut request_open_repo: Option<PathBuf> = None;
        let mut request_switch_branch: Option<usize> = None;
        let is_loading = self.pending_load.is_some();
        let is_executing = self.pending_exec.is_some();
        let is_busy = is_loading || is_executing;
        let is_loading_detail = self.pending_detail.is_some();

        // Top bar
        egui::TopBottomPanel::top("toolbar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(!is_busy, egui::Button::new("📂 Open Repository"))
                    .clicked()
                    && let Some(path) = rfd::FileDialog::new().pick_folder()
                {
                    request_open_repo = Some(path);
                }

                if let Some(repo) = &mut self.repo {
                    ui.separator();
                    let current = repo.branches[repo.selected_branch].name.clone();
                    let mut new_idx = repo.selected_branch;
                    ui.add_enabled_ui(!is_busy, |ui| {
                        egui::ComboBox::from_label("Branch")
                            .selected_text(&current)
                            .show_ui(ui, |ui| {
                                for (i, b) in repo.branches.iter().enumerate() {
                                    ui.selectable_value(&mut new_idx, i, &b.name);
                                }
                            });
                    });
                    if !is_busy && new_idx != repo.selected_branch {
                        request_switch_branch = Some(new_idx);
                    }

                    ui.separator();
                    ui.label(commit_count_label(repo.commits.len(), repo.total_commits));

                    if repo.has_more_commits
                        && !is_busy
                        && ui
                            .button(format!("Load {} more", COMMIT_PAGE_SIZE))
                            .clicked()
                    {
                        request_load_more = true;
                    }

                    if !repo.selection.is_empty() {
                        ui.separator();
                        ui.label(format!("{} selected", repo.selection.len()));
                    }
                }
            });
        });

        if let Some(path) = request_open_repo {
            self.open_repo(path);
        }
        if let Some(new_idx) = request_switch_branch {
            self.start_branch_switch_load(new_idx);
            request_load_more = false;
        }

        if let Some(pending) = &self.pending_load {
            egui::TopBottomPanel::top("loading_banner").show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(&pending.message);
                });
            });
        }

        if let Some(pending) = &self.pending_exec {
            egui::TopBottomPanel::top("exec_banner").show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(&pending.message);
                });
            });
        }

        // Search bar (below toolbar)
        if self.repo.is_some() {
            let mut search_changed = false;
            let mut select_all_results = false;

            egui::TopBottomPanel::top("search_bar").show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(icon::MAGNIFYING_GLASS);
                    let repo = self.repo.as_mut().unwrap();
                    let response = ui.add(
                        egui::TextEdit::singleline(&mut repo.search_query)
                            .hint_text("Search commits (message, author, path)…")
                            .desired_width(300.0),
                    );
                    if response.changed() {
                        search_changed = true;
                    }
                    if !repo.search_query.is_empty() {
                        ui.label(format!("{} matches", repo.search_hits.len()));
                        if ui.button("Select all results").clicked() && !repo.search_hits.is_empty()
                        {
                            select_all_results = true;
                        }
                        if ui.button(format!("{} Clear", icon::X)).clicked() {
                            repo.search_query.clear();
                            search_changed = true;
                        }
                    }
                });
            });

            if search_changed {
                self.update_search();
            }
            if select_all_results && let Some(repo) = &self.repo {
                let indices = search_hit_indices(&repo.search_hits);
                self.apply_selection_msg(SelectionMsg::SelectSet(indices));
            }
        }

        // Error banner
        if let Some(err) = &self.error {
            egui::TopBottomPanel::top("error_banner").show(ctx, |ui| {
                ui.colored_label(egui::Color32::RED, format!("{} {err}", icon::WARNING));
            });
        }

        // No repo loaded — show welcome
        if self.repo.is_none() {
            egui::CentralPanel::default().show(ctx, |ui| {
                ui.vertical_centered(|ui| {
                    ui.add_space(200.0);
                    ui.heading("gunk");
                    if let Some(pending) = &self.pending_load {
                        ui.add_space(8.0);
                        ui.spinner();
                        ui.label(&pending.message);
                    } else {
                        ui.label("Open a Git repository to get started.");
                    }
                });
            });
            return;
        }

        // Detail pane (right side)
        let mut selection_msg: Option<SelectionMsg> = None;
        let mut draft_msg: Option<DraftMsg> = None;

        egui::SidePanel::right("detail_pane")
            .default_width(450.0)
            .show(ctx, |ui| {
                if let Some(repo) = &mut self.repo {
                    let sel_len = repo.selection.len();
                    if sel_len == 1 {
                        let idx = *repo.selection.selected.iter().next().unwrap();
                        let id = repo.commits[idx].id.clone();
                        let is_dropped = repo
                            .draft
                            .ops
                            .iter()
                            .any(|op| matches!(op, gunk_core::Operation::Drop { target } if *target == id));

                        // Edit controls for the single focused commit.
                        ui.horizontal(|ui| {
                            let label = if is_dropped { "↺ Undrop" } else { "🗑 Drop" };
                            if ui.button(label).clicked() {
                                draft_msg = Some(DraftMsg::ToggleDrop(id.clone()));
                            }

                            // Flatten button — only shown for merge commits.
                            if repo.commits[idx].is_merge() {
                                let flatten_strategy = repo.draft.ops.iter().find_map(|op| {
                                    match op {
                                        gunk_core::Operation::FlattenMerge { merge, strategy }
                                            if *merge == id =>
                                        {
                                            Some(*strategy)
                                        }
                                        _ => None,
                                    }
                                });
                                let flatten_label = if flatten_strategy.is_some() {
                                    "↺ Unflatten"
                                } else {
                                    "⑂ Flatten merge"
                                };
                                if ui.button(flatten_label).clicked() {
                                    draft_msg = Some(DraftMsg::ToggleFlatten(id.clone()));
                                }
                            }
                        });

                        // Descendant-merge strategy — only relevant once the
                        // merge is flattened. Default preserves any unrelated
                        // descendant merges; Linearize is the power-user opt-in
                        // that collapses them, so it carries a warning.
                        if repo.commits[idx].is_merge() {
                            if let Some(current) = repo.draft.ops.iter().find_map(|op| match op {
                                gunk_core::Operation::FlattenMerge { merge, strategy }
                                    if *merge == id =>
                                {
                                    Some(*strategy)
                                }
                                _ => None,
                            }) {
                                ui.indent("flatten_strategy", |ui| {
                                    ui.label("If a newer merge sits above this one:");
                                    let preserve = gunk_core::FlattenStrategy::PreserveDescendantMerges;
                                    let linearize = gunk_core::FlattenStrategy::Linearize;
                                    if ui
                                        .radio(current == preserve, "Preserve it (recommended)")
                                        .clicked()
                                        && current != preserve
                                    {
                                        draft_msg =
                                            Some(DraftMsg::SetFlattenStrategy(id.clone(), preserve));
                                    }
                                    if ui
                                        .radio(current == linearize, "Linearize everything")
                                        .clicked()
                                        && current != linearize
                                    {
                                        draft_msg = Some(DraftMsg::SetFlattenStrategy(
                                            id.clone(),
                                            linearize,
                                        ));
                                    }
                                    if current == linearize {
                                        ui.colored_label(
                                            egui::Color32::from_rgb(220, 160, 60),
                                            "⚠ Drops every merge above this one, not just this one.",
                                        );
                                    }
                                });
                            }
                        }
                        ui.collapsing("Reword", |ui| {
                            ui.label("Summary");
                            ui.text_edit_singleline(&mut repo.reword_summary);
                            ui.label("Body");
                            ui.text_edit_multiline(&mut repo.reword_body);
                            if ui.button("Apply reword").clicked()
                                && !repo.reword_summary.trim().is_empty()
                            {
                                draft_msg = Some(DraftMsg::Reword {
                                    target: id.clone(),
                                    summary: repo.reword_summary.clone(),
                                    body: repo.reword_body.clone(),
                                });
                            }
                        });
                        ui.collapsing("Set author", |ui| {
                            ui.label("Name");
                            ui.text_edit_singleline(&mut repo.author_name);
                            ui.label("Email");
                            ui.text_edit_singleline(&mut repo.author_email);
                            if ui.button("Apply author").clicked()
                                && !repo.author_name.trim().is_empty()
                            {
                                draft_msg = Some(DraftMsg::SetAuthor {
                                    targets: vec![id.clone()],
                                    author: Identity {
                                        name: repo.author_name.clone(),
                                        email: repo.author_email.clone(),
                                        time: time::OffsetDateTime::now_utc(),
                                    },
                                });
                            }
                        });
                        let existing_co_authors =
                            parse_co_authors(&repo.commits[idx].body);
                        ui.collapsing("Co-authors", |ui| {
                            if existing_co_authors.is_empty() {
                                ui.label("No co-authors");
                            } else {
                                for (i, ca) in
                                    existing_co_authors.iter().enumerate()
                                {
                                    ui.horizontal(|ui| {
                                        ui.label(format!(
                                            "{} <{}>",
                                            ca.name, ca.email
                                        ));
                                        if ui
                                            .small_button(icon::X)
                                            .on_hover_text(
                                                "Remove this co-author",
                                            )
                                            .clicked()
                                        {
                                            let mut updated =
                                                existing_co_authors.clone();
                                            updated.remove(i);
                                            draft_msg =
                                                Some(DraftMsg::SetCoAuthors {
                                                    targets: vec![id.clone()],
                                                    co_authors: updated,
                                                });
                                        }
                                    });
                                }
                            }
                            ui.separator();
                            ui.label("Add co-author");
                            ui.label("Name");
                            ui.text_edit_singleline(&mut repo.co_author_name);
                            ui.label("Email");
                            ui.text_edit_singleline(&mut repo.co_author_email);
                            if ui.button("Add co-author").clicked()
                                && !repo.co_author_name.trim().is_empty()
                                && !repo.co_author_email.trim().is_empty()
                            {
                                let mut updated = existing_co_authors.clone();
                                updated.push(CoAuthor {
                                    name: repo.co_author_name.clone(),
                                    email: repo.co_author_email.clone(),
                                });
                                draft_msg = Some(DraftMsg::SetCoAuthors {
                                    targets: vec![id.clone()],
                                    co_authors: updated,
                                });
                                repo.co_author_name.clear();
                                repo.co_author_email.clear();
                            }
                            if !existing_co_authors.is_empty()
                                && ui.button("Remove all co-authors").clicked()
                            {
                                draft_msg = Some(DraftMsg::SetCoAuthors {
                                    targets: vec![id.clone()],
                                    co_authors: vec![],
                                });
                            }
                        });
                        ui.separator();

                        let commit = &repo.commits[idx];
                        if let Some(detail) = &repo.detail {
                            render_detail(ui, commit, detail);

                            // File removal UI (gated on filter-repo availability).
                            if repo.filter_repo_available && !detail.changed_paths.is_empty() {
                                ui.separator();
                                ui.collapsing("🗑 Remove files from history", |ui| {
                                    ui.label("Select files to remove from all history:");
                                    for change in &detail.changed_paths {
                                        let mut selected = repo
                                            .files_selected_for_removal
                                            .contains(&change.path);
                                        if ui.checkbox(&mut selected, &change.path).changed() {
                                            if selected {
                                                repo.files_selected_for_removal
                                                    .insert(change.path.clone());
                                            } else {
                                                repo.files_selected_for_removal
                                                    .remove(&change.path);
                                            }
                                        }
                                    }
                                    ui.checkbox(
                                        &mut repo.add_to_gitignore,
                                        "Add to .gitignore",
                                    );
                                    let can_apply =
                                        !repo.files_selected_for_removal.is_empty();
                                    ui.add_enabled_ui(can_apply, |ui| {
                                        if ui
                                            .button("Remove selected from all history")
                                            .clicked()
                                        {
                                            let paths: Vec<PathSpec> = repo
                                                .files_selected_for_removal
                                                .iter()
                                                .map(|p| PathSpec(p.clone()))
                                                .collect();
                                            draft_msg = Some(DraftMsg::RemovePaths {
                                                paths,
                                                add_to_gitignore: repo.add_to_gitignore,
                                            });
                                            repo.files_selected_for_removal.clear();
                                        }
                                    });
                                });
                            } else if !repo.filter_repo_available {
                                ui.separator();
                                ui.colored_label(
                                    egui::Color32::GRAY,
                                    "ℹ Install git-filter-repo to enable file removal from history.",
                                );
                            }
                        } else if is_loading_detail {
                            ui.horizontal(|ui| {
                                ui.spinner();
                                ui.label("Loading commit detail…");
                            });
                        }
                    } else if sel_len > 1 {
                        // Bulk actions over the selected set. The oldest commit
                        // (largest index, since the list is newest-first) is the
                        // squash/fixup keep target.
                        let targets: Vec<CommitId> = repo
                            .selection
                            .selected
                            .iter()
                            .map(|&i| repo.commits[i].id.clone())
                            .collect();
                        let keep = repo.commits[*repo.selection.selected.iter().next_back().unwrap()]
                            .id
                            .clone();
                        let absorb: Vec<CommitId> =
                            targets.iter().filter(|t| **t != keep).cloned().collect();

                        ui.heading(format!("{sel_len} commits selected"));
                        ui.separator();

                        if ui
                            .button(format!("{} Drop all", icon::TRASH))
                            .clicked()
                        {
                            draft_msg = Some(DraftMsg::DropMany(targets.clone()));
                        }
                        ui.horizontal(|ui| {
                            if ui
                                .button(format!(
                                    "{} Squash into oldest",
                                    icon::ARROW_DOWN
                                ))
                                .clicked()
                            {
                                draft_msg = Some(DraftMsg::Squash {
                                    keep: keep.clone(),
                                    absorb: absorb.clone(),
                                });
                            }
                            if ui
                                .button(format!(
                                    "{} Fixup into oldest",
                                    icon::ARROW_DOWN
                                ))
                                .clicked()
                            {
                                draft_msg = Some(DraftMsg::Fixup {
                                    keep: keep.clone(),
                                    absorb: absorb.clone(),
                                });
                            }
                        });
                        ui.collapsing("Set message (all)", |ui| {
                            ui.label("Summary");
                            ui.text_edit_singleline(&mut repo.reword_summary);
                            ui.label("Body");
                            ui.text_edit_multiline(&mut repo.reword_body);
                            if ui.button("Apply message").clicked()
                                && !repo.reword_summary.trim().is_empty()
                            {
                                draft_msg = Some(DraftMsg::SetMessage {
                                    targets: targets.clone(),
                                    summary: repo.reword_summary.clone(),
                                    body: repo.reword_body.clone(),
                                });
                            }
                        });
                        ui.collapsing("Set author (all)", |ui| {
                            ui.label("Name");
                            ui.text_edit_singleline(&mut repo.author_name);
                            ui.label("Email");
                            ui.text_edit_singleline(&mut repo.author_email);
                            if ui.button("Apply author").clicked()
                                && !repo.author_name.trim().is_empty()
                            {
                                draft_msg = Some(DraftMsg::SetAuthor {
                                    targets: targets.clone(),
                                    author: Identity {
                                        name: repo.author_name.clone(),
                                        email: repo.author_email.clone(),
                                        time: time::OffsetDateTime::now_utc(),
                                    },
                                });
                            }
                        });
                        ui.collapsing("Set co-authors (all)", |ui| {
                            ui.label("Name");
                            ui.text_edit_singleline(&mut repo.co_author_name);
                            ui.label("Email");
                            ui.text_edit_singleline(&mut repo.co_author_email);
                            if ui.button("Add co-author to all").clicked()
                                && !repo.co_author_name.trim().is_empty()
                                && !repo.co_author_email.trim().is_empty()
                            {
                                draft_msg = Some(DraftMsg::SetCoAuthors {
                                    targets: targets.clone(),
                                    co_authors: vec![CoAuthor {
                                        name: repo.co_author_name.clone(),
                                        email: repo.co_author_email.clone(),
                                    }],
                                });
                                repo.co_author_name.clear();
                                repo.co_author_email.clear();
                            }
                            if ui
                                .button("Remove all co-authors from all")
                                .clicked()
                            {
                                draft_msg = Some(DraftMsg::SetCoAuthors {
                                    targets: targets.clone(),
                                    co_authors: vec![],
                                });
                            }
                        });
                    } else {
                        ui.vertical_centered(|ui| {
                            ui.add_space(200.0);
                            ui.label("Select a commit to view details.");
                        });
                    }
                }
            });

        // Bottom draft bar
        let mut clear_draft = false;
        let mut request_confirm = false;
        let mut request_restore_panel = false;

        if let Some(repo) = &self.repo
            && (!repo.draft.is_empty()
                || repo.plan_error.is_some()
                || repo.execute_result.is_some())
        {
            egui::TopBottomPanel::bottom("draft_bar").show(ctx, |ui| {
                ui.horizontal(|ui| {
                    if !repo.draft.is_empty() {
                        ui.label(format!("{} draft operation(s)", repo.draft.len()));
                        if ui.button("Discard all drafts").clicked() {
                            clear_draft = true;
                        }
                        if repo.plan_error.is_none()
                            && !is_executing
                            && ui
                                .button(
                                    egui::RichText::new("✓ Confirm & Apply")
                                        .color(egui::Color32::from_rgb(80, 200, 80)),
                                )
                                .clicked()
                        {
                            request_confirm = true;
                        }
                    }
                    if ui
                        .button(format!(
                            "{} Restore from backup",
                            icon::ARROW_COUNTER_CLOCKWISE
                        ))
                        .clicked()
                    {
                        request_restore_panel = true;
                    }
                    if let Some(err) = &repo.plan_error {
                        ui.separator();
                        ui.colored_label(egui::Color32::RED, format!("{} invalid draft: {err}", icon::WARNING));
                    }
                });

                // Show execution result inline.
                if let Some(result) = &repo.execute_result {
                    match result {
                        Ok(r) => {
                            ui.colored_label(
                                egui::Color32::from_rgb(80, 200, 80),
                                format!(
                                    "✓ Applied successfully. Backup: {}  New tip: {}",
                                    &r.backup_ref,
                                    &r.new_tip[..7.min(r.new_tip.len())]
                                ),
                            );
                            if !r.pushed_commits.is_empty() {
                                ui.colored_label(
                                    egui::Color32::from_rgb(255, 200, 50),
                                    format!(
                                        "{} {} commit(s) were reachable from remote-tracking refs (published history rewritten)",
                                        icon::WARNING,
                                        r.pushed_commits.len()
                                    ),
                                );
                            }
                        }
                        Err(e) => {
                            ui.colored_label(egui::Color32::RED, format!("✗ Execution failed: {e}"));
                        }
                    }
                }
            });
        }

        // Confirmation dialog (modal-style window)
        if let Some(repo) = &self.repo
            && repo.show_confirm_dialog
        {
            let draft_count = repo.draft.len();
            let branch = repo.branches[repo.selected_branch].name.clone();
            let mut do_execute = false;
            let mut cancel_confirm = false;

            egui::Window::new("Confirm Apply")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    ui.label(format!("{} This will rewrite the branch history. This cannot be undone automatically (but a backup ref will be created).", icon::WARNING));
                    ui.add_space(8.0);
                    ui.label(format!("Branch: {branch}"));
                    ui.label(format!("Operations: {draft_count}"));
                    ui.add_space(12.0);
                    ui.horizontal(|ui| {
                        if ui.button("Cancel").clicked() {
                            cancel_confirm = true;
                        }
                        if ui
                            .button(
                                egui::RichText::new("Apply")
                                    .color(egui::Color32::from_rgb(80, 200, 80)),
                            )
                            .clicked()
                        {
                            do_execute = true;
                        }
                    });
                });

            if cancel_confirm && let Some(repo) = &mut self.repo {
                repo.show_confirm_dialog = false;
            }
            if do_execute {
                if let Some(repo) = &mut self.repo {
                    repo.show_confirm_dialog = false;
                }
                self.execute_plan();
            }
        }

        // Restore from backup panel
        if let Some(repo) = &self.repo
            && repo.show_restore_panel
        {
            let mut close_panel = false;
            let mut restore_ref: Option<String> = None;

            egui::Window::new("Restore from Backup")
                .collapsible(false)
                .resizable(true)
                .default_width(500.0)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    let branch = &repo.branches[repo.selected_branch].name;
                    ui.label(format!("Backups for branch: {branch}"));
                    ui.separator();

                    if repo.backup_refs.is_empty() {
                        ui.label("No backup refs found for this branch.");
                    } else {
                        for (ref_name, oid) in &repo.backup_refs {
                            ui.horizontal(|ui| {
                                let short_oid = &oid[..7.min(oid.len())];
                                // Extract timestamp from ref name.
                                let ts = ref_name.rsplit('/').next().unwrap_or("?");
                                ui.label(format!("{ts}  →  {short_oid}"));
                                if ui.button("Restore").clicked() {
                                    restore_ref = Some(ref_name.clone());
                                }
                            });
                        }
                    }

                    ui.separator();
                    if ui.button("Close").clicked() {
                        close_panel = true;
                    }
                });

            if close_panel && let Some(repo) = &mut self.repo {
                repo.show_restore_panel = false;
            }
            if let Some(ref_name) = restore_ref {
                if let Some(repo) = &mut self.repo {
                    repo.show_restore_panel = false;
                }
                self.restore_from_backup(&ref_name);
            }
        }

        // Commit list (main area)
        egui::CentralPanel::default().show(ctx, |ui| {
            if let Some(repo) = &self.repo {
                let searching = !repo.search_query.is_empty();

                // Iterate in projected order when a draft has produced preview
                // rows (reorder-aware); otherwise use the original commit order.
                let order: Vec<(usize, Option<&PreviewRow>)> = if repo.preview_rows.is_empty() {
                    (0..repo.commits.len()).map(|i| (i, None)).collect()
                } else {
                    // Reorder-aware lookup via the cached commit index (rebuilt
                    // only when commits change, not every frame).
                    repo.preview_rows
                        .iter()
                        .map(|r| (repo.commit_index[&r.id], Some(r)))
                        .collect()
                };

                // Card height: meta line (small) + message line (body) +
                // inter-line spacing + the frame's vertical inner margin, plus
                // a little gap between cards.
                let card_height = ui.text_style_height(&egui::TextStyle::Small)
                    + ui.text_style_height(&egui::TextStyle::Body)
                    + 3.0  // item_spacing.y between the two lines
                    + 12.0 // inner_margin top + bottom (6 + 6)
                    + 4.0; // gap between cards

                egui::ScrollArea::vertical()
                    .auto_shrink([false; 2])
                    .show_rows(ui, card_height, order.len(), |ui, row_range| {
                        for row_index in row_range {
                            let (i, prow) = order[row_index];
                            let commit = &repo.commits[i];
                            let selected = repo.selection.is_selected(i);
                            let is_search_match =
                                searching && repo.search_match_indices.contains(&i);

                            let response =
                                render_commit_row(ui, commit, selected, is_search_match, prow);

                            if response.clicked() {
                                let modifiers = ui.input(|inp| inp.modifiers);
                                let msg = if modifiers.ctrl || modifiers.command {
                                    SelectionMsg::CtrlClick(i)
                                } else if modifiers.shift {
                                    SelectionMsg::ShiftClick(i)
                                } else {
                                    SelectionMsg::Click(i)
                                };
                                selection_msg = Some(msg);
                            }
                        }
                    });
            }
        });

        if clear_draft {
            self.apply_draft_msg(DraftMsg::Clear);
            if let Some(repo) = &mut self.repo {
                repo.execute_result = None;
            }
        }
        if request_confirm && let Some(repo) = &mut self.repo {
            repo.show_confirm_dialog = true;
        }
        if request_restore_panel {
            self.load_backup_refs();
            if let Some(repo) = &mut self.repo {
                repo.show_restore_panel = true;
            }
        }
        if request_load_more && self.pending_load.is_none() {
            self.load_more_commits();
        }
        if let Some(msg) = draft_msg {
            self.apply_draft_msg(msg);
        }
        if let Some(msg) = selection_msg {
            self.apply_selection_msg(msg);
        }
    }
}

/// Render a single commit row in the list.
fn render_commit_row(
    ui: &mut egui::Ui,
    commit: &Commit,
    selected: bool,
    is_search_match: bool,
    prow: Option<&PreviewRow>,
) -> egui::Response {
    let merge_marker = if commit.is_merge() { "⑂ " } else { "" };
    let short_sha = commit.id.short();
    let author = &commit.author.name;
    let date = format_relative_date(commit.author.time);

    // Projected summary (reflects a reword/set-message in the draft).
    let summary = prow.map(|r| r.summary.as_str()).unwrap_or(&commit.summary);

    // Draft status badge + styling.
    let status = prow.map(|r| r.status).unwrap_or(RowStatus::Unchanged);
    let badge = match status {
        RowStatus::Unchanged => "",
        RowStatus::Reworded => "[reworded] ",
        RowStatus::Reauthored => "[reauthored] ",
        RowStatus::RewordedAndReauthored => "[reworded+reauthored] ",
        RowStatus::SquashKeep => "[squash←] ",
        RowStatus::Absorbed => "[absorbed] ",
        RowStatus::Flattened => "[flattened] ",
        RowStatus::Dropped => "[dropped] ",
    };
    let moved_marker = if prow.is_some_and(|r| r.moved) {
        "↕ "
    } else {
        ""
    };

    // Meta line: markers, badge, short hash, author, and relative time as
    // small, de-emphasised text.
    let meta_text =
        format!("{moved_marker}{merge_marker}{badge}{short_sha}  ·  {author}  ·  {date}");

    // Message line: the (projected) summary, rendered larger for readability.
    // When the card is selected its background is a saturated highlight, so
    // status tints are dropped in favour of high-contrast text.
    let mut message = egui::RichText::new(summary);
    match status {
        RowStatus::Dropped | RowStatus::Absorbed => {
            message = message.strikethrough();
            if !selected {
                message = message.color(egui::Color32::GRAY);
            }
        }
        RowStatus::Reworded
        | RowStatus::Reauthored
        | RowStatus::RewordedAndReauthored
        | RowStatus::SquashKeep
        | RowStatus::Flattened
            if !selected =>
        {
            message = message.color(egui::Color32::from_rgb(100, 150, 255));
        }
        _ => {}
    }
    if selected {
        message = message.color(egui::Color32::WHITE);
    }

    // Reuse last frame's response to know whether the card is hovered, so the
    // background can highlight before the content is painted.
    let id = ui.make_persistent_id(("commit-card", commit.id.0.as_str()));
    let hovered = ui
        .ctx()
        .read_response(id)
        .is_some_and(|r| r.hovered() || r.clicked());

    let visuals = ui.visuals();
    let fill = if selected {
        visuals.selection.bg_fill
    } else if hovered {
        visuals.widgets.hovered.weak_bg_fill
    } else if is_search_match {
        egui::Color32::from_rgba_premultiplied(80, 80, 0, 40)
    } else {
        egui::Color32::TRANSPARENT
    };

    let inner = egui::Frame::default()
        .fill(fill)
        .corner_radius(egui::CornerRadius::same(4))
        .inner_margin(egui::Margin::symmetric(8, 6))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 3.0;
            let mut meta = egui::RichText::new(&meta_text).small().monospace();
            meta = if selected {
                meta.color(egui::Color32::from_rgb(220, 230, 245))
            } else {
                meta.weak()
            };
            ui.label(meta);
            ui.label(message);
        });

    ui.interact(inner.response.rect, id, egui::Sense::click())
}

/// Render the detail pane for a selected commit.
fn render_detail(ui: &mut egui::Ui, commit: &Commit, detail: &CommitDetail) {
    ui.heading(&commit.summary);
    if !commit.body.is_empty() {
        ui.separator();
        ui.label(&commit.body);
    }
    ui.separator();

    egui::Grid::new("commit_meta").show(ui, |ui| {
        ui.label("SHA");
        ui.label(egui::RichText::new(&commit.id.0).monospace());
        ui.end_row();

        ui.label("Author");
        ui.label(format!("{} <{}>", commit.author.name, commit.author.email));
        ui.end_row();

        ui.label("Date");
        ui.label(format_relative_date(commit.author.time));
        ui.end_row();

        if commit.is_merge() {
            ui.label("Parents");
            let parents: Vec<&str> = commit.parents.iter().map(|p| p.short()).collect();
            ui.label(egui::RichText::new(parents.join(", ")).monospace());
            ui.end_row();
        }
    });

    ui.separator();

    // Changed files
    ui.strong(format!("Changed files ({})", detail.changed_paths.len()));
    egui::ScrollArea::vertical()
        .id_salt("changed_files")
        .max_height(150.0)
        .show(ui, |ui| {
            for change in &detail.changed_paths {
                let icon = match change.status {
                    ChangeStatus::Added => "A",
                    ChangeStatus::Modified => "M",
                    ChangeStatus::Deleted => "D",
                    ChangeStatus::Renamed => "R",
                    ChangeStatus::Copied => "C",
                    ChangeStatus::TypeChange => "T",
                    ChangeStatus::Unknown => "?",
                };
                let color = match change.status {
                    ChangeStatus::Added => egui::Color32::from_rgb(80, 200, 80),
                    ChangeStatus::Deleted => egui::Color32::from_rgb(200, 80, 80),
                    ChangeStatus::Modified => egui::Color32::from_rgb(200, 200, 80),
                    _ => egui::Color32::GRAY,
                };
                ui.horizontal(|ui| {
                    ui.colored_label(color, icon);
                    ui.label(&change.path);
                });
            }
        });

    ui.separator();

    // Diff — virtualized so a large diff (thousands of lines) only lays out
    // the visible rows each frame instead of every line.
    ui.strong("Diff");
    let row_height = ui.text_style_height(&egui::TextStyle::Monospace);
    egui::ScrollArea::vertical()
        .id_salt("diff_view")
        .auto_shrink([false; 2])
        .show_rows(ui, row_height, detail.diff_lines.len(), |ui, row_range| {
            for row in row_range {
                let line = &detail.diff_lines[row];
                let color = if line.starts_with('+') && !line.starts_with("+++") {
                    egui::Color32::from_rgb(80, 200, 80)
                } else if line.starts_with('-') && !line.starts_with("---") {
                    egui::Color32::from_rgb(200, 80, 80)
                } else if line.starts_with("@@") {
                    egui::Color32::from_rgb(100, 150, 255)
                } else {
                    ui.visuals().text_color()
                };
                ui.label(egui::RichText::new(line).monospace().color(color));
            }
        });
}

/// Format a timestamp as a relative date string.
fn format_relative_date(time: time::OffsetDateTime) -> String {
    let now = time::OffsetDateTime::now_utc();
    let duration = now - time;

    let days = duration.whole_days();
    if days < 1 {
        let hours = duration.whole_hours();
        if hours < 1 {
            let mins = duration.whole_minutes();
            if mins < 1 {
                return "just now".to_string();
            }
            return format!("{mins} min ago");
        }
        return format!("{hours} hours ago");
    }
    if days < 30 {
        return format!("{days} days ago");
    }
    if days < 365 {
        let months = days / 30;
        return format!("{months} months ago");
    }
    let years = days / 365;
    format!("{years} years ago")
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable;
    use time::OffsetDateTime;

    fn make_commit(oid: &str, summary: &str, is_merge: bool) -> Commit {
        let now = OffsetDateTime::now_utc();
        let identity = Identity {
            name: "Alice".to_string(),
            email: "alice@example.com".to_string(),
            time: now,
        };
        Commit {
            id: CommitId(oid.to_string()),
            parents: if is_merge {
                vec![
                    CommitId("aaaaaaa0000000".into()),
                    CommitId("bbbbbbb0000000".into()),
                ]
            } else {
                vec![CommitId("aaaaaaa0000000".into())]
            },
            author: identity.clone(),
            committer: identity,
            summary: summary.to_string(),
            body: String::new(),
            changed_paths: vec![],
        }
    }

    /// Build the expected meta-line text for a commit row, matching the
    /// metadata format string in `render_commit_row` exactly.
    fn expected_meta_text(
        moved: bool,
        merge: bool,
        badge: &str,
        short_sha: &str,
        author: &str,
    ) -> String {
        let moved_m = if moved { "↕ " } else { "" };
        let merge_m = if merge { "⑂ " } else { "" };
        format!("{moved_m}{merge_m}{badge}{short_sha}  ·  {author}  ·  just now")
    }

    // ── Pure function tests ────────────────────────────────────────

    #[test]
    fn commit_count_label_indicates_when_history_is_partial() {
        assert_eq!(commit_count_label(500, 1200), "500 of 1200 commits loaded");
        assert_eq!(commit_count_label(500, 500), "500 commits");
    }

    #[test]
    fn format_relative_date_just_now() {
        let now = OffsetDateTime::now_utc();
        assert_eq!(format_relative_date(now), "just now");
    }

    #[test]
    fn format_relative_date_minutes() {
        let t = OffsetDateTime::now_utc() - time::Duration::minutes(5);
        assert_eq!(format_relative_date(t), "5 min ago");
    }

    #[test]
    fn format_relative_date_hours() {
        let t = OffsetDateTime::now_utc() - time::Duration::hours(3);
        assert_eq!(format_relative_date(t), "3 hours ago");
    }

    #[test]
    fn format_relative_date_days() {
        let t = OffsetDateTime::now_utc() - time::Duration::days(15);
        assert_eq!(format_relative_date(t), "15 days ago");
    }

    #[test]
    fn format_relative_date_months() {
        let t = OffsetDateTime::now_utc() - time::Duration::days(90);
        assert_eq!(format_relative_date(t), "3 months ago");
    }

    #[test]
    fn format_relative_date_years() {
        let t = OffsetDateTime::now_utc() - time::Duration::days(400);
        assert_eq!(format_relative_date(t), "1 years ago");
    }

    // ── egui_kittest rendering tests ───────────────────────────────

    #[test]
    fn commit_row_unchanged_renders_sha_and_summary() {
        let commit = make_commit("abc1234567890abcdef", "Fix a critical bug", false);
        let harness = Harness::new_ui(|ui| {
            render_commit_row(ui, &commit, false, false, None);
        });
        let expected_meta = expected_meta_text(false, false, "", "abc1234", "Alice");
        harness.get_by_label(&expected_meta);
        harness.get_by_label("Fix a critical bug");
    }

    #[test]
    fn commit_row_merge_shows_marker() {
        let commit = make_commit("def5678901234abcde", "Merge feature branch", true);
        let harness = Harness::new_ui(|ui| {
            render_commit_row(ui, &commit, false, false, None);
        });
        let expected_meta = expected_meta_text(false, true, "", "def5678", "Alice");
        harness.get_by_label(&expected_meta);
        harness.get_by_label("Merge feature branch");
    }

    #[test]
    fn commit_row_dropped_shows_badge() {
        let commit = make_commit("111222333444555aabb", "Remove old code", false);
        let prow = PreviewRow {
            id: commit.id.clone(),
            summary: commit.summary.clone(),
            status: RowStatus::Dropped,
            moved: false,
        };
        let harness = Harness::new_ui(|ui| {
            render_commit_row(ui, &commit, false, false, Some(&prow));
        });
        let expected_meta = expected_meta_text(false, false, "[dropped] ", "1112223", "Alice");
        harness.get_by_label(&expected_meta);
        harness.get_by_label("Remove old code");
    }

    #[test]
    fn commit_row_reworded_shows_projected_summary() {
        let commit = make_commit("aabbccddeeff001122", "Old summary", false);
        let prow = PreviewRow {
            id: commit.id.clone(),
            summary: "Rewritten summary text".to_string(),
            status: RowStatus::Reworded,
            moved: false,
        };
        let harness = Harness::new_ui(|ui| {
            render_commit_row(ui, &commit, false, false, Some(&prow));
        });
        let expected_meta = expected_meta_text(false, false, "[reworded] ", "aabbccd", "Alice");
        harness.get_by_label(&expected_meta);
        harness.get_by_label("Rewritten summary text");
    }

    #[test]
    fn commit_row_squash_keep_shows_badge() {
        let commit = make_commit("ffee001122334455aa", "Keep this commit", false);
        let prow = PreviewRow {
            id: commit.id.clone(),
            summary: commit.summary.clone(),
            status: RowStatus::SquashKeep,
            moved: false,
        };
        let harness = Harness::new_ui(|ui| {
            render_commit_row(ui, &commit, false, false, Some(&prow));
        });
        let expected_meta = expected_meta_text(false, false, "[squash←] ", "ffee001", "Alice");
        harness.get_by_label(&expected_meta);
        harness.get_by_label("Keep this commit");
    }

    #[test]
    fn commit_row_moved_shows_arrow() {
        let commit = make_commit("9988776655443322ab", "Moved commit", false);
        let prow = PreviewRow {
            id: commit.id.clone(),
            summary: commit.summary.clone(),
            status: RowStatus::Unchanged,
            moved: true,
        };
        let harness = Harness::new_ui(|ui| {
            render_commit_row(ui, &commit, false, false, Some(&prow));
        });
        let expected_meta = expected_meta_text(true, false, "", "9988776", "Alice");
        harness.get_by_label(&expected_meta);
        harness.get_by_label("Moved commit");
    }

    #[test]
    fn commit_row_flattened_merge_shows_both_markers() {
        let commit = make_commit("aabb11223344556677", "Merge develop", true);
        let prow = PreviewRow {
            id: commit.id.clone(),
            summary: commit.summary.clone(),
            status: RowStatus::Flattened,
            moved: false,
        };
        let harness = Harness::new_ui(|ui| {
            render_commit_row(ui, &commit, false, false, Some(&prow));
        });
        let expected_meta = expected_meta_text(false, true, "[flattened] ", "aabb112", "Alice");
        harness.get_by_label(&expected_meta);
        harness.get_by_label("Merge develop");
    }

    #[test]
    fn detail_pane_shows_commit_metadata() {
        let commit = make_commit("deadbeef12345678ab", "Add new feature", false);
        let detail = CommitDetail::from_diff(
            vec![PathChange {
                path: "src/main.rs".to_string(),
                status: ChangeStatus::Modified,
            }],
            "+added line\n-removed line".to_string(),
        );
        let harness = Harness::new_ui(|ui| {
            render_detail(ui, &commit, &detail);
        });
        // The heading should contain the commit summary.
        harness.get_by_label("Add new feature");
        // The metadata grid should contain the author.
        harness.get_by_label("Alice <alice@example.com>");
        // SHA should be displayed.
        harness.get_by_label("deadbeef12345678ab");
        // Changed file should appear.
        harness.get_by_label("src/main.rs");
    }

    #[test]
    fn detail_pane_shows_body_when_present() {
        let mut commit = make_commit("cafe0123456789abcd", "Summary line", false);
        commit.body = "Extended description\nwith multiple lines.".to_string();
        let detail = CommitDetail::from_diff(vec![], String::new());
        let harness = Harness::new_ui(|ui| {
            render_detail(ui, &commit, &detail);
        });
        harness.get_by_label("Extended description\nwith multiple lines.");
    }

    #[test]
    fn detail_pane_shows_parents_for_merge() {
        let commit = make_commit("merge123456789abcd", "Merge PR #42", true);
        let detail = CommitDetail::from_diff(vec![], String::new());
        let harness = Harness::new_ui(|ui| {
            render_detail(ui, &commit, &detail);
        });
        // Parents label should show short hashes.
        harness.get_by_label("aaaaaaa, bbbbbbb");
    }
}

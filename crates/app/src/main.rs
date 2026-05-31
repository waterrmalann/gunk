use std::path::PathBuf;

use eframe::egui;
use gunk_core::{
    ChangeStatus, Commit, CommitId, DraftMsg, DraftState, Identity, PathChange, PathSpec,
    PreviewRow, RowStatus, SearchHit, SelectionMsg, SelectionState, plan, preview, search_commits,
    search_hit_indices,
};
use gunk_gitio::{
    BranchInfo, ExecuteResult, Git, execute_plan as gitio_execute_plan, has_filter_repo,
    list_backup_refs, restore_backup,
};
use std::collections::HashMap;

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
        Box::new(|_cc| Ok(Box::new(App::default()))),
    )
}

// ── State ──────────────────────────────────────────────────────────

/// The loaded repository state.
struct RepoState {
    git: Git,
    branches: Vec<BranchInfo>,
    selected_branch: usize,
    commits: Vec<Commit>,
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

struct CommitDetail {
    _oid: String,
    changed_paths: Vec<PathChange>,
    diff: String,
}

/// Top-level app state.
#[derive(Default)]
struct App {
    repo: Option<RepoState>,
    error: Option<String>,
}

// ── Logic (no UI) ──────────────────────────────────────────────────

impl App {
    fn open_repo(&mut self, path: PathBuf) {
        self.error = None;
        match Git::open(&path) {
            Ok(git) => match git.list_branches() {
                Ok(branches) if branches.is_empty() => {
                    self.error = Some("Repository has no branches.".into());
                }
                Ok(branches) => {
                    let commits = match git.walk_commits(&branches[0].name) {
                        Ok(c) => c,
                        Err(e) => {
                            self.error = Some(format!("Failed to read commits: {e}"));
                            return;
                        }
                    };
                    let selection = SelectionState::new(commits.len());
                    let filter_repo_available = has_filter_repo(&git);
                    self.repo = Some(RepoState {
                        git,
                        branches,
                        selected_branch: 0,
                        commits,
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
                        show_confirm_dialog: false,
                        execute_result: None,
                        show_restore_panel: false,
                        backup_refs: Vec::new(),
                        filter_repo_available,
                        files_selected_for_removal: std::collections::BTreeSet::new(),
                        add_to_gitignore: false,
                    });
                }
                Err(e) => self.error = Some(format!("Failed to list branches: {e}")),
            },
            Err(e) => self.error = Some(format!("Failed to open repo: {e}")),
        }
    }

    fn apply_selection_msg(&mut self, msg: SelectionMsg) {
        if let Some(repo) = &mut self.repo {
            repo.selection = repo.selection.reduce(msg);
            // Load detail for the last-clicked commit (if exactly one or the most recent click)
            self.load_detail_for_focus();
        }
    }

    fn load_detail_for_focus(&mut self) {
        let mut load_error = None;
        if let Some(repo) = &mut self.repo {
            // Show detail for the single selected commit, or the last one if multiple.
            let focus = if repo.selection.len() == 1 {
                repo.selection.selected.iter().next().copied()
            } else {
                None
            };

            if let Some(idx) = focus {
                let oid = repo.commits[idx].id.0.clone();
                match (repo.git.changed_paths(&oid), repo.git.show_diff(&oid)) {
                    (Ok(changed_paths), Ok(diff)) => {
                        repo.detail = Some(CommitDetail {
                            _oid: oid,
                            changed_paths,
                            diff,
                        });
                    }
                    (Err(e), _) | (_, Err(e)) => {
                        repo.detail = None;
                        load_error = Some(format!("Failed to load commit detail: {e}"));
                    }
                }
            } else {
                repo.detail = None;
            }
        }
        if load_error.is_some() {
            self.error = load_error;
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
    /// For composite plans, this handles the
    /// flatten → filter-repo → re-snapshot → rebase pipeline: flatten and
    /// filter-repo both rewrite OIDs, so subsequent phases must re-derive
    /// from a fresh snapshot.
    fn execute_plan(&mut self) {
        let Some(repo) = &mut self.repo else { return };

        let branch = repo.branches[repo.selected_branch].name.clone();

        // Separate operations into flatten, filter-repo, and rebase groups.
        let flatten_ops: Vec<_> = repo
            .draft
            .ops
            .iter()
            .filter(|op| matches!(op, gunk_core::Operation::FlattenMerge { .. }))
            .cloned()
            .collect();
        let filter_ops: Vec<_> = repo
            .draft
            .ops
            .iter()
            .filter(|op| matches!(op, gunk_core::Operation::RemovePaths { .. }))
            .cloned()
            .collect();
        let rebase_ops: Vec<_> = repo
            .draft
            .ops
            .iter()
            .filter(|op| {
                !matches!(
                    op,
                    gunk_core::Operation::RemovePaths { .. }
                        | gunk_core::Operation::FlattenMerge { .. }
                )
            })
            .cloned()
            .collect();

        let has_flatten = !flatten_ops.is_empty();
        let has_filter = !filter_ops.is_empty();
        let has_rebase = !rebase_ops.is_empty();
        let remaining_after_flatten = has_filter || has_rebase;
        let remaining_after_filter = has_rebase;

        // Phase 0: Execute flatten (if any) — must precede filter-repo and rebase.
        if has_flatten {
            let flatten_plan = plan(&repo.commits, &flatten_ops);
            match flatten_plan {
                Err(e) => {
                    repo.execute_result = Some(Err(format!("Plan error: {e}")));
                    return;
                }
                Ok(fp) => match gitio_execute_plan(&repo.git, &branch, &fp) {
                    Err(e) => {
                        repo.execute_result = Some(Err(format!("{e}")));
                        return;
                    }
                    Ok(flatten_result) => {
                        if !remaining_after_flatten {
                            repo.execute_result = Some(Ok(flatten_result));
                        }
                        // Re-snapshot after flatten (OIDs changed).
                        match repo.git.walk_commits(&branch) {
                            Ok(c) => repo.commits = c,
                            Err(e) => {
                                repo.execute_result =
                                    Some(Err(format!("Failed to reload after flatten: {e}")));
                                return;
                            }
                        }
                        if !remaining_after_flatten {
                            self.reset_draft_state();
                            return;
                        }
                    }
                },
            }
        }

        // Phase 1: Execute filter-repo (if any).
        if has_filter {
            let filter_plan = plan(&repo.commits, &filter_ops);
            match filter_plan {
                Err(e) => {
                    repo.execute_result = Some(Err(format!("Plan error: {e}")));
                    return;
                }
                Ok(fp) => match gitio_execute_plan(&repo.git, &branch, &fp) {
                    Err(e) => {
                        repo.execute_result = Some(Err(format!("{e}")));
                        return;
                    }
                    Ok(filter_result) => {
                        if !remaining_after_filter {
                            // Only filter-repo (and maybe flatten before it), no rebase needed.
                            repo.execute_result = Some(Ok(filter_result));
                        }
                        // Re-snapshot after filter-repo (OIDs changed).
                        match repo.git.walk_commits(&branch) {
                            Ok(c) => repo.commits = c,
                            Err(e) => {
                                repo.execute_result =
                                    Some(Err(format!("Failed to reload after filter-repo: {e}")));
                                return;
                            }
                        }
                        // If there are no rebase ops, we're done. Save the filter result.
                        if !remaining_after_filter {
                            self.reset_draft_state();
                            return;
                        }
                    }
                },
            }
        }

        // Phase 2: Execute rebase ops (against fresh or original snapshot).
        if has_rebase {
            let rebase_plan = plan(&repo.commits, &rebase_ops);
            match rebase_plan {
                Err(e) => {
                    repo.execute_result = Some(Err(format!("Plan error: {e}")));
                }
                Ok(rp) => match gitio_execute_plan(&repo.git, &branch, &rp) {
                    Ok(exec_result) => {
                        repo.execute_result = Some(Ok(exec_result));
                        self.reset_draft_state();
                    }
                    Err(e) => {
                        repo.execute_result = Some(Err(format!("{e}")));
                    }
                },
            }
        }
    }

    /// Reset draft state and reload commits after successful execution.
    fn reset_draft_state(&mut self) {
        let Some(repo) = &mut self.repo else { return };
        let branch = repo.branches[repo.selected_branch].name.clone();
        repo.draft = DraftState::new();
        repo.preview_rows.clear();
        repo.plan_error = None;
        repo.selection = SelectionState::new(0);
        repo.detail = None;
        repo.search_query.clear();
        repo.search_hits.clear();
        repo.search_match_indices.clear();
        repo.reword_summary.clear();
        repo.reword_body.clear();
        repo.author_name.clear();
        repo.author_email.clear();
        repo.files_selected_for_removal.clear();
        repo.add_to_gitignore = false;
        match repo.git.walk_commits(&branch) {
            Ok(c) => {
                repo.selection = SelectionState::new(c.len());
                repo.commits = c;
            }
            Err(e) => {
                self.error = Some(format!("Failed to reload commits: {e}"));
            }
        }
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
                repo.selection = SelectionState::new(0);
                repo.detail = None;
                repo.execute_result = None;
                match repo.git.walk_commits(&branch) {
                    Ok(c) => {
                        repo.selection = SelectionState::new(c.len());
                        repo.commits = c;
                    }
                    Err(e) => self.error = Some(format!("Failed to reload commits: {e}")),
                }
            }
            Err(e) => self.error = Some(format!("Restore failed: {e}")),
        }
    }
}

// ── UI ─────────────────────────────────────────────────────────────

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Top bar
        egui::TopBottomPanel::top("toolbar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                if ui.button("📂 Open Repository").clicked()
                    && let Some(path) = rfd::FileDialog::new().pick_folder()
                {
                    self.open_repo(path);
                }

                if let Some(repo) = &mut self.repo {
                    ui.separator();
                    let current = repo.branches[repo.selected_branch].name.clone();
                    let mut new_idx = repo.selected_branch;
                    egui::ComboBox::from_label("Branch")
                        .selected_text(&current)
                        .show_ui(ui, |ui| {
                            for (i, b) in repo.branches.iter().enumerate() {
                                ui.selectable_value(&mut new_idx, i, &b.name);
                            }
                        });
                    if new_idx != repo.selected_branch {
                        repo.selected_branch = new_idx;
                        repo.selection = SelectionState::new(0);
                        repo.detail = None;
                        repo.search_query.clear();
                        repo.search_hits.clear();
                        repo.search_match_indices.clear();
                        repo.draft = DraftState::new();
                        repo.preview_rows.clear();
                        repo.plan_error = None;
                        let branch_name = repo.branches[new_idx].name.clone();
                        match repo.git.walk_commits(&branch_name) {
                            Ok(c) => {
                                repo.selection = SelectionState::new(c.len());
                                repo.commits = c;
                            }
                            Err(e) => self.error = Some(format!("Failed to read commits: {e}")),
                        }
                    }

                    ui.separator();
                    ui.label(format!("{} commits", repo.commits.len()));

                    if !repo.selection.is_empty() {
                        ui.separator();
                        ui.label(format!("{} selected", repo.selection.len()));
                    }
                }
            });
        });

        // Search bar (below toolbar)
        if self.repo.is_some() {
            let mut search_changed = false;
            let mut select_all_results = false;

            egui::TopBottomPanel::top("search_bar").show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label("🔍");
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
                        if ui.button("✕ Clear").clicked() {
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
                ui.colored_label(egui::Color32::RED, format!("⚠ {err}"));
            });
        }

        // No repo loaded — show welcome
        if self.repo.is_none() {
            egui::CentralPanel::default().show(ctx, |ui| {
                ui.vertical_centered(|ui| {
                    ui.add_space(200.0);
                    ui.heading("gunk");
                    ui.label("Open a Git repository to get started.");
                });
            });
            return;
        }

        // Detail pane (right side)
        let mut selection_msg: Option<SelectionMsg> = None;
        let mut draft_msg: Option<DraftMsg> = None;
        let mut recompute_preview = false;

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
                                let is_flattened = repo.draft.ops.iter().any(|op| {
                                    matches!(op, gunk_core::Operation::FlattenMerge { merge } if *merge == id)
                                });
                                let flatten_label = if is_flattened {
                                    "↺ Unflatten"
                                } else {
                                    "⑂ Flatten merge"
                                };
                                if ui.button(flatten_label).clicked() {
                                    draft_msg = Some(DraftMsg::ToggleFlatten(id.clone()));
                                }
                            }
                        });
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

                        if ui.button("🗑 Drop all").clicked() {
                            // Drop is a per-commit toggle; add a drop for each
                            // target that is not already dropped.
                            for t in &targets {
                                let already = repo.draft.ops.iter().any(|op| {
                                    matches!(op, gunk_core::Operation::Drop { target } if target == t)
                                });
                                if !already {
                                    repo.draft = repo.draft.reduce(DraftMsg::ToggleDrop(t.clone()));
                                }
                            }
                            recompute_preview = true;
                        }
                        ui.horizontal(|ui| {
                            if ui.button("⬇ Squash into oldest").clicked() {
                                draft_msg = Some(DraftMsg::Squash {
                                    keep: keep.clone(),
                                    absorb: absorb.clone(),
                                });
                            }
                            if ui.button("⬇ Fixup into oldest").clicked() {
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
                    if ui.button("↻ Restore from backup").clicked() {
                        request_restore_panel = true;
                    }
                    if let Some(err) = &repo.plan_error {
                        ui.separator();
                        ui.colored_label(egui::Color32::RED, format!("⚠ invalid draft: {err}"));
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
                                        "⚠ {} commit(s) were reachable from remote-tracking refs (published history rewritten)",
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
                    ui.label("⚠ This will rewrite the branch history. This cannot be undone automatically (but a backup ref will be created).");
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
                    // Original index of each commit, for the reorder-aware lookup.
                    let index_by_id: HashMap<&CommitId, usize> = repo
                        .commits
                        .iter()
                        .enumerate()
                        .map(|(i, c)| (&c.id, i))
                        .collect();
                    repo.preview_rows
                        .iter()
                        .map(|r| (index_by_id[&r.id], Some(r)))
                        .collect()
                };

                egui::ScrollArea::vertical()
                    .auto_shrink([false; 2])
                    .show(ui, |ui| {
                        for (i, prow) in order {
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
        if let Some(msg) = draft_msg {
            self.apply_draft_msg(msg);
        } else if recompute_preview {
            self.recompute_preview();
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

    let text =
        format!("{moved_marker}{merge_marker}{badge}{short_sha}  {summary:<60}  {author}  {date}");

    let mut rich_text = egui::RichText::new(&text).monospace();
    match status {
        RowStatus::Dropped | RowStatus::Absorbed => {
            rich_text = rich_text.strikethrough().color(egui::Color32::GRAY);
        }
        RowStatus::Reworded
        | RowStatus::Reauthored
        | RowStatus::RewordedAndReauthored
        | RowStatus::SquashKeep
        | RowStatus::Flattened => {
            rich_text = rich_text.color(egui::Color32::from_rgb(100, 150, 255));
        }
        RowStatus::Unchanged => {}
    }
    if is_search_match && !selected {
        rich_text =
            rich_text.background_color(egui::Color32::from_rgba_premultiplied(80, 80, 0, 40));
    }

    let label = egui::SelectableLabel::new(selected, rich_text);
    ui.add(label)
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

    // Diff
    ui.strong("Diff");
    egui::ScrollArea::vertical()
        .id_salt("diff_view")
        .show(ui, |ui| {
            for line in detail.diff.lines() {
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

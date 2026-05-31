use std::path::PathBuf;

use eframe::egui;
use gunk_core::{ChangeStatus, Commit, PathChange};
use gunk_gitio::{BranchInfo, Git};

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
    selected_commit: Option<usize>,
    /// Lazily loaded detail for the selected commit.
    detail: Option<CommitDetail>,
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
                    self.repo = Some(RepoState {
                        git,
                        branches,
                        selected_branch: 0,
                        commits,
                        selected_commit: None,
                        detail: None,
                    });
                }
                Err(e) => self.error = Some(format!("Failed to list branches: {e}")),
            },
            Err(e) => self.error = Some(format!("Failed to open repo: {e}")),
        }
    }

    fn select_commit(&mut self, idx: usize) {
        if let Some(repo) = &mut self.repo {
            repo.selected_commit = Some(idx);
            // Load detail lazily
            let oid = &repo.commits[idx].id.0;
            let changed_paths = repo.git.changed_paths(oid).unwrap_or_default();
            let diff = repo.git.show_diff(oid).unwrap_or_default();
            repo.detail = Some(CommitDetail {
                _oid: oid.clone(),
                changed_paths,
                diff,
            });
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
                        repo.selected_commit = None;
                        repo.detail = None;
                        let branch_name = repo.branches[new_idx].name.clone();
                        match repo.git.walk_commits(&branch_name) {
                            Ok(c) => repo.commits = c,
                            Err(e) => self.error = Some(format!("Failed to read commits: {e}")),
                        }
                    }

                    ui.separator();
                    ui.label(format!("{} commits", repo.commits.len()));
                }
            });
        });

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
        let mut select_idx: Option<usize> = None;

        egui::SidePanel::right("detail_pane")
            .default_width(450.0)
            .show(ctx, |ui| {
                if let Some(repo) = &self.repo {
                    if let (Some(ci), Some(detail)) = (repo.selected_commit, &repo.detail) {
                        let commit = &repo.commits[ci];
                        render_detail(ui, commit, detail);
                    } else {
                        ui.vertical_centered(|ui| {
                            ui.add_space(200.0);
                            ui.label("Select a commit to view details.");
                        });
                    }
                }
            });

        // Commit list (main area)
        egui::CentralPanel::default().show(ctx, |ui| {
            if let Some(repo) = &self.repo {
                egui::ScrollArea::vertical()
                    .auto_shrink([false; 2])
                    .show(ui, |ui| {
                        for (i, commit) in repo.commits.iter().enumerate() {
                            let selected = repo.selected_commit == Some(i);
                            let response = render_commit_row(ui, commit, selected);
                            if response.clicked() {
                                select_idx = Some(i);
                            }
                        }
                    });
            }
        });

        if let Some(idx) = select_idx {
            self.select_commit(idx);
        }
    }
}

/// Render a single commit row in the list.
fn render_commit_row(ui: &mut egui::Ui, commit: &Commit, selected: bool) -> egui::Response {
    let merge_marker = if commit.is_merge() { "⑂ " } else { "" };
    let short_sha = commit.id.short();
    let author = &commit.author.name;
    let date = format_relative_date(commit.author.time);

    let text = format!(
        "{merge_marker}{short_sha}  {:<60}  {author}  {date}",
        commit.summary
    );

    let label = egui::SelectableLabel::new(selected, egui::RichText::new(&text).monospace());
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

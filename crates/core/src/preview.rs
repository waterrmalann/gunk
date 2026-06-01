use std::collections::{HashMap, HashSet};

use crate::model::{Commit, CommitId};
use crate::operation::Operation;
use crate::plan::{PlanError, plan};

/// How a commit is affected by the current draft, for display in the preview.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowStatus {
    /// Untouched by the draft.
    Unchanged,
    /// Message will change (reword or bulk set-message).
    Reworded,
    /// Author will change (bulk set-author).
    Reauthored,
    /// Both message and author will change.
    RewordedAndReauthored,
    /// This commit keeps its identity and absorbs others (squash/fixup target).
    SquashKeep,
    /// This commit will be folded into another and disappear (squash/fixup).
    Absorbed,
    /// This merge commit will be flattened into a single ordinary commit.
    Flattened,
    /// This commit will be removed entirely.
    Dropped,
}

/// A single row in the projected (draft) history.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreviewRow {
    pub id: CommitId,
    /// The projected summary (the new message if reworded, else the original).
    pub summary: String,
    pub status: RowStatus,
    /// Whether the commit's position changed relative to the original order.
    pub moved: bool,
}

/// Project the draft history for review.
///
/// Returns one row per commit in the (possibly reordered) display order,
/// newest-first, each tagged with how the draft affects it.
///
/// With no operations, every row is `Unchanged`. With operations, the draft is
/// validated through the plan engine first; an invalid draft returns the same
/// `PlanError` the engine would produce, so the UI can surface it.
pub fn preview(
    snapshot: &[Commit],
    operations: &[Operation],
) -> Result<Vec<PreviewRow>, PlanError> {
    // Validate using the real engine (skipped when there is nothing to plan).
    if !operations.is_empty() {
        plan(snapshot, operations)?;
    }

    let n = snapshot.len();
    let original_pos: HashMap<&CommitId, usize> = snapshot
        .iter()
        .enumerate()
        .map(|(i, c)| (&c.id, i))
        .collect();

    // ── Per-commit op classification ───────────────────────────────
    let mut dropped: HashSet<&CommitId> = HashSet::new();
    let mut reworded: HashSet<&CommitId> = HashSet::new();
    let mut reauthored: HashSet<&CommitId> = HashSet::new();
    let mut absorbed: HashSet<&CommitId> = HashSet::new();
    let mut squash_keep: HashSet<&CommitId> = HashSet::new();
    let mut flattened: HashSet<&CommitId> = HashSet::new();
    let mut new_summary: HashMap<&CommitId, &str> = HashMap::new();
    let mut reorder: Option<&[CommitId]> = None;

    for op in operations {
        match op {
            Operation::Reword {
                target, summary, ..
            } => {
                reworded.insert(target);
                new_summary.insert(target, summary);
            }
            Operation::SetMessage {
                targets, summary, ..
            } => {
                for t in targets {
                    reworded.insert(t);
                    new_summary.insert(t, summary);
                }
            }
            Operation::SetAuthor { targets, .. } => {
                for t in targets {
                    reauthored.insert(t);
                }
            }
            Operation::Squash { keep, absorb } => {
                squash_keep.insert(keep);
                absorbed.extend(absorb.iter());
            }
            Operation::Fixup { keep, absorb } => {
                squash_keep.insert(keep);
                absorbed.extend(absorb.iter());
            }
            Operation::Drop { target } => {
                dropped.insert(target);
            }
            Operation::FlattenMerge { merge, .. } => {
                flattened.insert(merge);
            }
            Operation::Reorder { new_order } => {
                reorder = Some(new_order);
            }
            Operation::RemovePaths { .. } => {}
            Operation::SetCoAuthors { targets, .. } => {
                for t in targets {
                    reworded.insert(t);
                }
            }
        }
    }

    // ── Display order (newest-first) ───────────────────────────────
    // A reorder op is already validated as a permutation by the plan engine.
    let display: Vec<&Commit> = match reorder {
        Some(order) => order.iter().map(|id| &snapshot[original_pos[id]]).collect(),
        None => snapshot.iter().collect(),
    };

    let mut rows = Vec::with_capacity(n);
    for (display_idx, commit) in display.iter().enumerate() {
        let id = &commit.id;
        let status = if dropped.contains(id) {
            RowStatus::Dropped
        } else if absorbed.contains(id) {
            RowStatus::Absorbed
        } else if squash_keep.contains(id) {
            RowStatus::SquashKeep
        } else if flattened.contains(id) {
            RowStatus::Flattened
        } else {
            match (reworded.contains(id), reauthored.contains(id)) {
                (true, true) => RowStatus::RewordedAndReauthored,
                (true, false) => RowStatus::Reworded,
                (false, true) => RowStatus::Reauthored,
                (false, false) => RowStatus::Unchanged,
            }
        };

        let summary = new_summary
            .get(id)
            .map(|s| s.to_string())
            .unwrap_or_else(|| commit.summary.clone());

        let moved = original_pos[id] != display_idx;

        rows.push(PreviewRow {
            id: id.clone(),
            summary,
            status,
            moved,
        });
    }

    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Identity;
    use time::OffsetDateTime;

    fn cid(s: &str) -> CommitId {
        CommitId(s.to_string())
    }

    fn ident(name: &str) -> Identity {
        Identity {
            name: name.to_string(),
            email: format!("{name}@example.com"),
            time: OffsetDateTime::UNIX_EPOCH,
        }
    }

    fn make_commit(name: &str, parents: &[&str]) -> Commit {
        Commit {
            id: cid(name),
            parents: parents.iter().map(|p| cid(p)).collect(),
            author: ident("Alice"),
            committer: ident("Alice"),
            summary: format!("Message {name}"),
            body: String::new(),
            changed_paths: vec![],
        }
    }

    /// Newest-first: E→D→C→B→A (A is root).
    fn linear() -> Vec<Commit> {
        vec![
            make_commit("E", &["D"]),
            make_commit("D", &["C"]),
            make_commit("C", &["B"]),
            make_commit("B", &["A"]),
            make_commit("A", &[]),
        ]
    }

    fn row<'a>(rows: &'a [PreviewRow], id: &str) -> &'a PreviewRow {
        rows.iter().find(|r| r.id == cid(id)).expect("row present")
    }

    #[test]
    fn empty_draft_is_all_unchanged() {
        let rows = preview(&linear(), &[]).unwrap();
        assert_eq!(rows.len(), 5);
        assert!(rows.iter().all(|r| r.status == RowStatus::Unchanged));
        assert!(rows.iter().all(|r| !r.moved));
    }

    #[test]
    fn reword_marks_row_and_updates_summary() {
        let ops = vec![Operation::Reword {
            target: cid("C"),
            summary: "Refactored C".into(),
            body: String::new(),
        }];
        let rows = preview(&linear(), &ops).unwrap();
        let c = row(&rows, "C");
        assert_eq!(c.status, RowStatus::Reworded);
        assert_eq!(c.summary, "Refactored C");
        // Other rows untouched.
        assert_eq!(row(&rows, "A").status, RowStatus::Unchanged);
    }

    #[test]
    fn set_author_marks_reauthored() {
        let ops = vec![Operation::SetAuthor {
            targets: vec![cid("B"), cid("D")],
            author: ident("Bob"),
        }];
        let rows = preview(&linear(), &ops).unwrap();
        assert_eq!(row(&rows, "B").status, RowStatus::Reauthored);
        assert_eq!(row(&rows, "D").status, RowStatus::Reauthored);
    }

    #[test]
    fn reword_and_set_author_same_commit_combines() {
        let ops = vec![
            Operation::Reword {
                target: cid("C"),
                summary: "x".into(),
                body: String::new(),
            },
            Operation::SetAuthor {
                targets: vec![cid("C")],
                author: ident("Bob"),
            },
        ];
        let rows = preview(&linear(), &ops).unwrap();
        assert_eq!(row(&rows, "C").status, RowStatus::RewordedAndReauthored);
    }

    #[test]
    fn drop_marks_row_dropped() {
        let ops = vec![Operation::Drop { target: cid("C") }];
        let rows = preview(&linear(), &ops).unwrap();
        assert_eq!(rows.len(), 5); // still present in preview, marked dropped
        assert_eq!(row(&rows, "C").status, RowStatus::Dropped);
    }

    #[test]
    fn squash_marks_keep_and_absorbed() {
        let ops = vec![Operation::Squash {
            keep: cid("B"),
            absorb: vec![cid("C")],
        }];
        let rows = preview(&linear(), &ops).unwrap();
        assert_eq!(row(&rows, "B").status, RowStatus::SquashKeep);
        assert_eq!(row(&rows, "C").status, RowStatus::Absorbed);
    }

    #[test]
    fn reorder_marks_moved_rows_in_new_order() {
        // Swap the two oldest: original newest-first E D C B A → E D C A B
        let ops = vec![Operation::Reorder {
            new_order: vec![cid("E"), cid("D"), cid("C"), cid("A"), cid("B")],
        }];
        let rows = preview(&linear(), &ops).unwrap();
        let ids: Vec<&str> = rows.iter().map(|r| r.id.0.as_str()).collect();
        assert_eq!(ids, vec!["E", "D", "C", "A", "B"]);
        assert!(row(&rows, "A").moved);
        assert!(row(&rows, "B").moved);
        assert!(!row(&rows, "E").moved);
    }

    #[test]
    fn invalid_draft_propagates_plan_error() {
        let ops = vec![
            Operation::Drop { target: cid("C") },
            Operation::Reword {
                target: cid("C"),
                summary: "x".into(),
                body: String::new(),
            },
        ];
        let err = preview(&linear(), &ops).unwrap_err();
        assert!(matches!(err, PlanError::ConflictingOps(_, _, _)));
    }

    #[test]
    fn unknown_commit_propagates_error() {
        let ops = vec![Operation::Drop { target: cid("ZZZ") }];
        let err = preview(&linear(), &ops).unwrap_err();
        assert_eq!(err, PlanError::CommitNotFound(cid("ZZZ")));
    }
}

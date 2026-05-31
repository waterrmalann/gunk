use std::collections::{HashMap, HashSet};

use crate::model::{Commit, CommitId, Identity};
use crate::operation::Operation;

use super::{PlanError, RebaseTodo, RebaseTodoLine, format_message, shell_escape};

/// Build a `RebaseTodo` from rebase-class operations.
///
/// The snapshot is newest-first; the todo is generated oldest-first.
pub(super) fn build_rebase_todo(
    snapshot: &[Commit],
    rebase_ops: &[&Operation],
    commit_index: &HashMap<&CommitId, usize>,
) -> Result<RebaseTodo, PlanError> {
    let n = snapshot.len();

    // Base: parent of oldest commit, or None for root.
    let oldest = &snapshot[n - 1];
    let base = oldest.parents.first().cloned();

    // Initial rebase ordering: oldest-first (reverse of snapshot).
    let mut line_order: Vec<usize> = (0..n).rev().collect();

    // ── Collect per-type data ──────────────────────────────────────

    let mut rewords: HashMap<&CommitId, String> = HashMap::new();
    let mut author_changes: HashMap<&CommitId, Identity> = HashMap::new();
    let mut drops: HashSet<&CommitId> = HashSet::new();
    let mut squash_groups: Vec<(&CommitId, Vec<&CommitId>, bool)> = Vec::new();
    let mut reorder: Option<&Vec<CommitId>> = None;

    for op in rebase_ops {
        match op {
            Operation::Reword {
                target,
                summary,
                body,
            } => {
                rewords.insert(target, format_message(summary, body));
            }
            Operation::SetMessage {
                targets,
                summary,
                body,
            } => {
                let msg = format_message(summary, body);
                for t in targets {
                    rewords.insert(t, msg.clone());
                }
            }
            Operation::SetAuthor { targets, author } => {
                for t in targets {
                    author_changes.insert(t, author.clone());
                }
            }
            Operation::Drop { target } => {
                drops.insert(target);
            }
            Operation::Squash { keep, absorb } => {
                squash_groups.push((keep, absorb.iter().collect(), false));
            }
            Operation::Fixup { keep, absorb } => {
                squash_groups.push((keep, absorb.iter().collect(), true));
            }
            Operation::Reorder { new_order } => {
                reorder = Some(new_order);
            }
            _ => {}
        }
    }

    // ── Apply user reorder ─────────────────────────────────────────

    if let Some(order) = reorder {
        let snapshot_ids: HashSet<&CommitId> = snapshot.iter().map(|c| &c.id).collect();
        let order_ids: HashSet<&CommitId> = order.iter().collect();
        if order_ids != snapshot_ids || order.len() != n {
            return Err(PlanError::InvalidPermutation);
        }
        // new_order is newest-first (UI convention). Reverse for rebase.
        line_order = order.iter().rev().map(|id| commit_index[id]).collect();
    }

    // ── Ensure squash/fixup adjacency ──────────────────────────────

    let absorbed_set: HashSet<&CommitId> = squash_groups
        .iter()
        .flat_map(|(_, abs, _)| abs.iter().copied())
        .collect();

    let fixup_absorbed: HashSet<&CommitId> = squash_groups
        .iter()
        .filter(|(_, _, is_fixup)| *is_fixup)
        .flat_map(|(_, abs, _)| abs.iter().copied())
        .collect();

    let keep_set: HashSet<&CommitId> = squash_groups.iter().map(|(k, _, _)| *k).collect();

    let keep_absorb_count: HashMap<&CommitId, usize> = squash_groups
        .iter()
        .map(|(k, abs, _)| (*k, abs.len()))
        .collect();

    // Sort groups by keep's snapshot position for deterministic processing.
    squash_groups.sort_by_key(|(keep, _, _)| commit_index[*keep]);

    for (keep_id, absorb_ids, _) in &squash_groups {
        let keep_snap = commit_index[*keep_id];

        // Remove absorbed from current positions.
        let absorbed_snap: HashSet<usize> = absorb_ids.iter().map(|id| commit_index[*id]).collect();
        line_order.retain(|idx| !absorbed_snap.contains(idx));

        // Find keep's position after removal.
        let keep_pos = line_order
            .iter()
            .position(|&idx| idx == keep_snap)
            .expect("keep commit must be in line_order");

        // Insert absorbed right after keep, preserving absorb-Vec order.
        for (i, abs_id) in absorb_ids.iter().enumerate() {
            line_order.insert(keep_pos + 1 + i, commit_index[*abs_id]);
        }
    }

    // ── Generate todo lines ────────────────────────────────────────

    let mut lines: Vec<RebaseTodoLine> = Vec::new();
    let mut i = 0;

    while i < line_order.len() {
        let snap_idx = line_order[i];
        let commit = &snapshot[snap_idx];
        let id = &commit.id;

        if drops.contains(id) {
            lines.push(RebaseTodoLine::Drop(id.clone()));
            i += 1;
            continue;
        }

        if absorbed_set.contains(id) {
            if fixup_absorbed.contains(id) {
                lines.push(RebaseTodoLine::Fixup(id.clone()));
            } else {
                lines.push(RebaseTodoLine::Squash(id.clone()));
            }
            i += 1;
            continue;
        }

        // Primary commit (standalone or keep).
        if rewords.contains_key(id) {
            lines.push(RebaseTodoLine::Reword(id.clone()));
        } else {
            lines.push(RebaseTodoLine::Pick(id.clone()));
        }
        i += 1;

        // If this is a keep commit, emit absorbed lines immediately after.
        if keep_set.contains(id) {
            let count = keep_absorb_count[id];
            for _ in 0..count {
                if i >= line_order.len() {
                    break;
                }
                let abs_snap = line_order[i];
                let abs_id = &snapshot[abs_snap].id;
                if fixup_absorbed.contains(abs_id) {
                    lines.push(RebaseTodoLine::Fixup(abs_id.clone()));
                } else {
                    lines.push(RebaseTodoLine::Squash(abs_id.clone()));
                }
                i += 1;
            }
        }

        // Exec line for author change (after the entire group).
        if let Some(author) = author_changes.get(id) {
            lines.push(RebaseTodoLine::Exec(format!(
                "git commit --amend --no-edit --author=\"{} <{}>\"",
                shell_escape(&author.name),
                shell_escape(&author.email)
            )));
        }
    }

    // ── Build deterministically-sorted output maps ─────────────────

    let mut message_map: Vec<(CommitId, String)> = rewords
        .into_iter()
        .map(|(id, msg)| (id.clone(), msg))
        .collect();
    message_map.sort_by_key(|(id, _)| commit_index[id]);

    let mut author_map: Vec<(CommitId, Identity)> = author_changes
        .into_iter()
        .map(|(id, a)| (id.clone(), a))
        .collect();
    author_map.sort_by_key(|(id, _)| commit_index[id]);

    Ok(RebaseTodo {
        base,
        lines,
        message_map,
        author_map,
    })
}

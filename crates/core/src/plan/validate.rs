use std::collections::{HashMap, HashSet};

use crate::model::{Commit, CommitId};
use crate::operation::Operation;

use super::PlanError;

pub(super) fn collect_commit_ids(op: &Operation) -> Vec<&CommitId> {
    match op {
        Operation::Reword { target, .. } => vec![target],
        Operation::SetAuthor { targets, .. } => targets.iter().collect(),
        Operation::SetMessage { targets, .. } => targets.iter().collect(),
        Operation::Squash { keep, absorb } | Operation::Fixup { keep, absorb } => {
            std::iter::once(keep).chain(absorb.iter()).collect()
        }
        Operation::Drop { target } => vec![target],
        Operation::Reorder { new_order } => new_order.iter().collect(),
        Operation::RemovePaths { .. } => vec![],
        Operation::FlattenMerge { merge } => vec![merge],
        Operation::SetCoAuthors { targets, .. } => targets.iter().collect(),
    }
}

pub(super) fn validate_commit_ids(
    operations: &[Operation],
    commit_index: &HashMap<&CommitId, usize>,
) -> Result<(), PlanError> {
    for op in operations {
        for id in collect_commit_ids(op) {
            if !commit_index.contains_key(id) {
                return Err(PlanError::CommitNotFound(id.clone()));
            }
        }
    }
    Ok(())
}

pub(super) fn validate_operations(
    operations: &[Operation],
    snapshot: &[Commit],
    commit_index: &HashMap<&CommitId, usize>,
) -> Result<(), PlanError> {
    for op in operations {
        match op {
            Operation::FlattenMerge { merge } => {
                let commit = &snapshot[commit_index[merge]];
                if commit.parents.len() < 2 {
                    return Err(PlanError::NotAMergeCommit(merge.clone()));
                }
                if commit.parents.len() >= 3 {
                    return Err(PlanError::OctopusMergeUnsupported(merge.clone()));
                }
            }
            Operation::Squash { keep, absorb } | Operation::Fixup { keep, absorb } => {
                if absorb.contains(keep) {
                    return Err(PlanError::SelfAbsorb(keep.clone()));
                }
                if absorb.is_empty() {
                    return Err(PlanError::EmptyAbsorb(keep.clone()));
                }
            }
            Operation::RemovePaths { paths, .. } => {
                // An empty path set sends `filter-repo --invert-paths` with no
                // `--path` args. That happens to be a no-op today, but it still
                // rewrites every commit id for no benefit and leans on an
                // external tool's defensive default. Refuse it at the boundary.
                if paths.is_empty() {
                    return Err(PlanError::EmptyPathRemoval);
                }
            }
            _ => {}
        }
    }
    Ok(())
}

pub(super) fn detect_conflicts(rebase_ops: &[&Operation]) -> Result<(), PlanError> {
    let mut dropped = HashSet::new();
    let mut reworded = HashSet::new();
    let mut set_messaged = HashSet::new();
    let mut set_authored = HashSet::new();
    let mut set_co_authored = HashSet::new();
    let mut keeps = HashSet::new();
    let mut absorbed = HashSet::new();
    let mut reorder_count = 0u32;

    for op in rebase_ops {
        match op {
            Operation::Drop { target } => {
                dropped.insert(target);
            }
            Operation::Reword { target, .. } => {
                reworded.insert(target);
            }
            Operation::SetMessage { targets, .. } => {
                for t in targets {
                    set_messaged.insert(t);
                }
            }
            Operation::SetAuthor { targets, .. } => {
                for t in targets {
                    set_authored.insert(t);
                }
            }
            Operation::SetCoAuthors { targets, .. } => {
                for t in targets {
                    set_co_authored.insert(t);
                }
            }
            Operation::Squash { keep, absorb: abs } | Operation::Fixup { keep, absorb: abs } => {
                if !keeps.insert(keep) {
                    return Err(PlanError::ConflictingOps(
                        keep.clone(),
                        "squash/fixup (keep)".into(),
                        "squash/fixup (keep)".into(),
                    ));
                }
                for a in abs {
                    if !absorbed.insert(a) {
                        return Err(PlanError::ConflictingOps(
                            a.clone(),
                            "squash/fixup (absorb)".into(),
                            "squash/fixup (absorb)".into(),
                        ));
                    }
                }
            }
            Operation::Reorder { .. } => {
                reorder_count += 1;
                if reorder_count > 1 {
                    return Err(PlanError::MultipleReorders);
                }
            }
            _ => {}
        }
    }

    // Check that dropped commits don't conflict with other operations.
    let conflict_sets: &[(&HashSet<&CommitId>, &str)] = &[
        (&reworded, "reword"),
        (&set_messaged, "set-message"),
        (&set_authored, "set-author"),
        (&set_co_authored, "set-co-authors"),
        (&keeps, "squash/fixup (keep)"),
        (&absorbed, "squash/fixup (absorb)"),
    ];

    for id in &dropped {
        for (set, label) in conflict_sets {
            if set.contains(id) {
                return Err(PlanError::ConflictingOps(
                    (*id).clone(),
                    "drop".into(),
                    (*label).into(),
                ));
            }
        }
    }

    for id in &reworded {
        if set_messaged.contains(id) {
            return Err(PlanError::ConflictingOps(
                (*id).clone(),
                "reword".into(),
                "set-message".into(),
            ));
        }
    }

    for id in &keeps {
        if absorbed.contains(id) {
            return Err(PlanError::ConflictingOps(
                (*id).clone(),
                "squash/fixup (keep)".into(),
                "squash/fixup (absorb)".into(),
            ));
        }
    }

    // Absorbed commits cannot be individually reworded, re-messaged, or re-authored.
    let absorbed_conflict_sets: &[(&HashSet<&CommitId>, &str)] = &[
        (&reworded, "reword"),
        (&set_messaged, "set-message"),
        (&set_authored, "set-author"),
        (&set_co_authored, "set-co-authors"),
    ];

    for id in &absorbed {
        for (set, label) in absorbed_conflict_sets {
            if set.contains(id) {
                return Err(PlanError::ConflictingOps(
                    (*id).clone(),
                    "squash/fixup (absorb)".into(),
                    (*label).into(),
                ));
            }
        }
    }

    Ok(())
}

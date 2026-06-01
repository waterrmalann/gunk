use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::model::{Commit, CommitId, Identity, PathSpec};
use crate::operation::Operation;

mod rebase;
mod validate;

#[cfg(test)]
mod tests;

// ── Types ──────────────────────────────────────────────────────────

/// A single line in a rebase todo.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RebaseTodoLine {
    Pick(CommitId),
    Reword(CommitId),
    Squash(CommitId),
    Fixup(CommitId),
    Drop(CommitId),
    Exec(String),
}

/// The full rebase instruction set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RebaseTodo {
    /// `None` means `--root` (the range includes the initial commit).
    pub base: Option<CommitId>,
    pub lines: Vec<RebaseTodoLine>,
    /// Map from commit id to prepared message (for reword/squash).
    pub message_map: Vec<(CommitId, String)>,
    /// Map from commit id to author override.
    pub author_map: Vec<(CommitId, Identity)>,
}

/// Specification for git-filter-repo path removal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FilterRepoSpec {
    pub paths: Vec<PathSpec>,
    pub add_to_gitignore: bool,
}

/// Specification for flattening a merge commit via plumbing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FlattenSpec {
    pub merge: CommitId,
    pub mainline_parent: CommitId,
    pub message: String,
}

/// The output of the plan engine. Deterministic and snapshot-testable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExecutionPlan {
    Rebase(RebaseTodo),
    FilterRepo(FilterRepoSpec),
    Flatten(FlattenSpec),
    Composite(Vec<ExecutionPlan>),
}

/// Maps pre-rewrite commit ids to their post-rewrite ids.
///
/// A value of `None` means the commit was dropped by the rewrite (it no longer
/// exists). Ids absent from the map are assumed unchanged (identity). Produced
/// by history-rewriting phases (flatten, filter-repo) so that operations planned
/// against the original snapshot can be retargeted onto the rewritten history.
pub type OidMap = HashMap<CommitId, Option<CommitId>>;

/// Resolve a commit id through an [`OidMap`]. Returns `CommitNotFound` if the
/// id was dropped by a preceding rewrite, since an operation can no longer
/// target it.
fn remap_required(id: &CommitId, map: &OidMap) -> Result<CommitId, PlanError> {
    match map.get(id) {
        Some(Some(new)) => Ok(new.clone()),
        Some(None) => Err(PlanError::CommitNotFound(id.clone())),
        None => Ok(id.clone()),
    }
}

/// Compose two rewrite maps so that `compose(first, second)` maps original ids
/// (the domain of `first`) all the way to post-`second` ids.
///
/// Ids that were identity through `first` but rewritten by `second` are carried
/// over, so the result is a complete original→final map across both phases.
pub fn compose_oid_maps(first: &OidMap, second: &OidMap) -> OidMap {
    // Start from `second`: covers ids that passed through `first` unchanged.
    let mut result = second.clone();
    for (orig, mapped) in first {
        let final_value = match mapped {
            // Already dropped in the first phase — stays dropped.
            None => None,
            // Survived the first phase as `cur`; apply the second phase to it.
            Some(cur) => match second.get(cur) {
                Some(Some(next)) => Some(next.clone()),
                Some(None) => None,
                None => Some(cur.clone()),
            },
        };
        result.insert(orig.clone(), final_value);
    }
    result
}

impl ExecutionPlan {
    /// Retarget every commit id in this plan through `map`.
    ///
    /// Used by the executor to thread a rewrite map from one composite phase
    /// into the next, so a plan built against the original snapshot can run
    /// against already-rewritten history. Identity maps are a cheap clone.
    pub fn remap_oids(&self, map: &OidMap) -> Result<ExecutionPlan, PlanError> {
        if map.is_empty() {
            return Ok(self.clone());
        }
        Ok(match self {
            ExecutionPlan::FilterRepo(spec) => ExecutionPlan::FilterRepo(spec.clone()),
            ExecutionPlan::Flatten(spec) => ExecutionPlan::Flatten(FlattenSpec {
                merge: remap_required(&spec.merge, map)?,
                mainline_parent: remap_required(&spec.mainline_parent, map)?,
                message: spec.message.clone(),
            }),
            ExecutionPlan::Rebase(todo) => ExecutionPlan::Rebase(remap_rebase_todo(todo, map)?),
            ExecutionPlan::Composite(plans) => ExecutionPlan::Composite(
                plans
                    .iter()
                    .map(|p| p.remap_oids(map))
                    .collect::<Result<_, _>>()?,
            ),
        })
    }
}

fn remap_rebase_todo(todo: &RebaseTodo, map: &OidMap) -> Result<RebaseTodo, PlanError> {
    let base = match &todo.base {
        Some(id) => Some(remap_required(id, map)?),
        None => None,
    };

    let lines = todo
        .lines
        .iter()
        .map(|line| {
            Ok(match line {
                RebaseTodoLine::Pick(id) => RebaseTodoLine::Pick(remap_required(id, map)?),
                RebaseTodoLine::Reword(id) => RebaseTodoLine::Reword(remap_required(id, map)?),
                RebaseTodoLine::Squash(id) => RebaseTodoLine::Squash(remap_required(id, map)?),
                RebaseTodoLine::Fixup(id) => RebaseTodoLine::Fixup(remap_required(id, map)?),
                RebaseTodoLine::Drop(id) => RebaseTodoLine::Drop(remap_required(id, map)?),
                RebaseTodoLine::Exec(cmd) => RebaseTodoLine::Exec(cmd.clone()),
            })
        })
        .collect::<Result<_, PlanError>>()?;

    let message_map = todo
        .message_map
        .iter()
        .map(|(id, msg)| Ok((remap_required(id, map)?, msg.clone())))
        .collect::<Result<_, PlanError>>()?;

    let author_map = todo
        .author_map
        .iter()
        .map(|(id, a)| Ok((remap_required(id, map)?, a.clone())))
        .collect::<Result<_, PlanError>>()?;

    Ok(RebaseTodo {
        base,
        lines,
        message_map,
        author_map,
    })
}

/// Errors from plan validation.
#[derive(Debug, Clone, PartialEq, Eq, Error, Serialize, Deserialize)]
pub enum PlanError {
    #[error("commit {0} not found in snapshot")]
    CommitNotFound(CommitId),
    #[error("conflicting operations on commit {0}: cannot both {1} and {2}")]
    ConflictingOps(CommitId, String, String),
    #[error("squash target {0} is not in the editable range")]
    SquashTargetOutOfRange(CommitId),
    #[error("reorder is not a valid permutation of the editable range")]
    InvalidPermutation,
    #[error("octopus merge {0} (3+ parents) is not supported in v1")]
    OctopusMergeUnsupported(CommitId),
    #[error("commit {0} is not a merge commit")]
    NotAMergeCommit(CommitId),
    #[error("squash/fixup on {0} has no commits to absorb")]
    EmptyAbsorb(CommitId),
    #[error("commit {0} cannot be both keep and absorbed in squash/fixup")]
    SelfAbsorb(CommitId),
    #[error("multiple reorder operations are not allowed")]
    MultipleReorders,
    #[error("remove-paths operation has no paths; refusing to rewrite history for nothing")]
    EmptyPathRemoval,
    #[error("no operations to plan")]
    NoOperations,
}

// ── Helpers ────────────────────────────────────────────────────────

fn format_message(summary: &str, body: &str) -> String {
    if body.is_empty() {
        summary.to_string()
    } else {
        format!("{summary}\n\n{body}")
    }
}

/// Escape a string for safe embedding inside a double-quoted shell argument.
fn shell_escape(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('$', "\\$")
        .replace('`', "\\`")
}

// ── Public API ─────────────────────────────────────────────────────

/// Compute an execution plan from a history snapshot and a set of operations.
///
/// The snapshot must be **newest-first** (index 0 = branch tip).
/// Operations are order-independent — the same set in any order produces the
/// same plan.
///
/// Returns `Err(PlanError)` if operations reference unknown commits, are
/// mutually contradictory, or violate structural invariants.
#[must_use = "the computed plan must be used; discarding it loses the planning work"]
pub fn plan(snapshot: &[Commit], operations: &[Operation]) -> Result<ExecutionPlan, PlanError> {
    if operations.is_empty() {
        return Err(PlanError::NoOperations);
    }

    let commit_index: HashMap<&CommitId, usize> = snapshot
        .iter()
        .enumerate()
        .map(|(i, c)| (&c.id, i))
        .collect();

    validate::validate_commit_ids(operations, &commit_index)?;
    validate::validate_operations(operations, snapshot, &commit_index)?;

    // ── Classify operations ────────────────────────────────────────

    let mut flatten_ops = Vec::new();
    let mut filter_ops = Vec::new();
    let mut rebase_ops = Vec::new();

    for op in operations {
        match op {
            Operation::FlattenMerge { .. } => flatten_ops.push(op),
            Operation::RemovePaths { .. } => filter_ops.push(op),
            _ => rebase_ops.push(op),
        }
    }

    validate::detect_conflicts(&rebase_ops)?;

    // ── Build sub-plans in execution order ─────────────────────────

    let mut plans = Vec::new();

    // 1. Flatten (must precede rebase).
    for op in &flatten_ops {
        if let Operation::FlattenMerge { merge } = op {
            let commit = &snapshot[commit_index[merge]];
            plans.push(ExecutionPlan::Flatten(FlattenSpec {
                merge: merge.clone(),
                mainline_parent: commit.parents[0].clone(),
                message: format_message(&commit.summary, &commit.body),
            }));
        }
    }

    // 2. Filter-repo (must precede rebase).
    if !filter_ops.is_empty() {
        let mut all_paths = Vec::new();
        let mut add_to_gitignore = false;
        for op in &filter_ops {
            if let Operation::RemovePaths {
                paths,
                add_to_gitignore: agi,
            } = op
            {
                all_paths.extend(paths.iter().cloned());
                add_to_gitignore = add_to_gitignore || *agi;
            }
        }
        plans.push(ExecutionPlan::FilterRepo(FilterRepoSpec {
            paths: all_paths,
            add_to_gitignore,
        }));
    }

    // 3. Rebase.
    if !rebase_ops.is_empty() {
        let todo = rebase::build_rebase_todo(snapshot, &rebase_ops, &commit_index)?;
        plans.push(ExecutionPlan::Rebase(todo));
    }

    Ok(match plans.len() {
        1 => plans.into_iter().next().unwrap(),
        _ => ExecutionPlan::Composite(plans),
    })
}

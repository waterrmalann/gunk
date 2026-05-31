use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::model::{Commit, CommitId, Identity, PathSpec};
use crate::operation::Operation;

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
    pub base: CommitId,
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
    #[error("{0}")]
    Other(String),
}

/// Compute an execution plan from a history snapshot and a set of operations.
///
/// This is the heart of the app. It validates operations against the snapshot,
/// detects conflicts, auto-reorders when necessary, and produces a deterministic
/// `ExecutionPlan`.
pub fn plan(_snapshot: &[Commit], _operations: &[Operation]) -> Result<ExecutionPlan, PlanError> {
    // Phase 3 will implement this. For now, return a placeholder.
    todo!("plan engine implementation in Phase 3")
}

use serde::{Deserialize, Serialize};

use crate::model::{CommitId, Identity, PathSpec};

/// A single user intent captured in draft mode.
/// The plan engine turns a set of these into a concrete `ExecutionPlan`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Operation {
    /// Reword a single commit's message.
    Reword {
        target: CommitId,
        summary: String,
        body: String,
    },
    /// Set author on one or more commits (bulk-capable).
    SetAuthor {
        targets: Vec<CommitId>,
        author: Identity,
    },
    /// Set message on one or more commits (bulk reword).
    SetMessage {
        targets: Vec<CommitId>,
        summary: String,
        body: String,
    },
    /// Squash: absorb commits into `keep`, combining messages.
    Squash {
        keep: CommitId,
        absorb: Vec<CommitId>,
    },
    /// Fixup: absorb commits into `keep`, discarding absorbed messages.
    Fixup {
        keep: CommitId,
        absorb: Vec<CommitId>,
    },
    /// Drop a commit entirely.
    Drop { target: CommitId },
    /// Reorder commits. `new_order` is a permutation of the editable range.
    Reorder { new_order: Vec<CommitId> },
    /// Remove paths from the entire history.
    RemovePaths {
        paths: Vec<PathSpec>,
        add_to_gitignore: bool,
    },
    /// Flatten a merge commit into a single ordinary commit.
    FlattenMerge { merge: CommitId },
}

use serde::{Deserialize, Serialize};

use crate::model::{CoAuthor, CommitId, Identity, PathSpec};

/// How to replay descendants when flattening a merge whose descendants include
/// another merge commit.
///
/// When the selected merge is the branch tip, or its only descendants are
/// ordinary commits, both variants behave identically. The distinction matters
/// solely for *descendant merges* — merges newer than the one being flattened
/// that the user never selected:
///
/// - [`PreserveDescendantMerges`](FlattenStrategy::PreserveDescendantMerges)
///   replays with `git rebase --rebase-merges`, so unselected merges are
///   recreated. If recreating one hits a conflict the operation fails loudly
///   rather than diverging silently. This is the safe default.
/// - [`Linearize`](FlattenStrategy::Linearize) replays with a plain
///   `git rebase --onto`, which flattens *every* merge in the range, not just
///   the selected one. This is the power-user choice: it can silently collapse
///   topology, so the UI should warn and confirm before using it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum FlattenStrategy {
    /// Recreate unselected descendant merges via `git rebase --rebase-merges`.
    #[default]
    PreserveDescendantMerges,
    /// Linearize the whole range, dropping every descendant merge.
    Linearize,
}

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
    ///
    /// `strategy` only matters when the merge has descendant commits that
    /// include *another* merge: it decides whether those descendant merges are
    /// preserved or linearized. See [`FlattenStrategy`].
    FlattenMerge {
        merge: CommitId,
        strategy: FlattenStrategy,
    },
    /// Set co-authors on one or more commits (replaces existing co-author trailers).
    SetCoAuthors {
        targets: Vec<CommitId>,
        co_authors: Vec<CoAuthor>,
    },
}

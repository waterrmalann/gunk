use crate::model::{CoAuthor, CommitId, Identity, PathSpec};
use crate::operation::Operation;

/// The pending set of draft operations the user has accumulated.
///
/// Nothing here touches a real repository — it is the input to the plan engine
/// (`crate::plan`) and the preview projection (`crate::preview`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DraftState {
    /// The accumulated operations, in insertion order.
    pub ops: Vec<Operation>,
}

/// Messages that drive draft-state transitions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DraftMsg {
    /// Reword a single commit. Replaces any existing reword for the same target.
    Reword {
        target: CommitId,
        summary: String,
        body: String,
    },
    /// Bulk set-message across targets. Replaces any existing bulk set-message
    /// covering the exact same target set.
    SetMessage {
        targets: Vec<CommitId>,
        summary: String,
        body: String,
    },
    /// Bulk set-author across targets. Replaces any existing set-author covering
    /// the exact same target set.
    SetAuthor {
        targets: Vec<CommitId>,
        author: Identity,
    },
    /// Squash `absorb` into `keep`. Replaces any existing squash/fixup with the
    /// same keep.
    Squash {
        keep: CommitId,
        absorb: Vec<CommitId>,
    },
    /// Fixup `absorb` into `keep`. Replaces any existing squash/fixup with the
    /// same keep.
    Fixup {
        keep: CommitId,
        absorb: Vec<CommitId>,
    },
    /// Toggle a drop for the given commit (add if absent, remove if present).
    ToggleDrop(CommitId),
    /// Reorder the range. Replaces any existing reorder.
    Reorder { new_order: Vec<CommitId> },
    /// Remove paths from history. Merges with any existing RemovePaths op.
    RemovePaths {
        paths: Vec<PathSpec>,
        add_to_gitignore: bool,
    },
    /// Toggle flatten for a merge commit (add if absent, remove if present).
    ToggleFlatten(CommitId),
    /// Set co-authors on one or more commits. Replaces any existing co-author
    /// op covering the exact same target set.
    SetCoAuthors {
        targets: Vec<CommitId>,
        co_authors: Vec<CoAuthor>,
    },
    /// Remove the draft operation at the given index (no-op if out of range).
    RemoveOp(usize),
    /// Discard all drafts.
    Clear,
}

impl DraftState {
    /// Create an empty draft.
    pub fn new() -> Self {
        Self::default()
    }

    /// Reducer: apply a message to produce the next state.
    #[must_use = "reduce returns a new draft; the original is unchanged"]
    pub fn reduce(&self, msg: DraftMsg) -> Self {
        let mut ops = self.ops.clone();
        match msg {
            DraftMsg::Reword {
                target,
                summary,
                body,
            } => {
                ops.retain(|op| !matches!(op, Operation::Reword { target: t, .. } if *t == target));
                ops.push(Operation::Reword {
                    target,
                    summary,
                    body,
                });
            }
            DraftMsg::SetMessage {
                targets,
                summary,
                body,
            } => {
                ops.retain(
                    |op| !matches!(op, Operation::SetMessage { targets: t, .. } if *t == targets),
                );
                ops.push(Operation::SetMessage {
                    targets,
                    summary,
                    body,
                });
            }
            DraftMsg::SetAuthor { targets, author } => {
                ops.retain(
                    |op| !matches!(op, Operation::SetAuthor { targets: t, .. } if *t == targets),
                );
                ops.push(Operation::SetAuthor { targets, author });
            }
            DraftMsg::Squash { keep, absorb } => {
                ops.retain(|op| !is_squash_or_fixup_with_keep(op, &keep));
                ops.push(Operation::Squash { keep, absorb });
            }
            DraftMsg::Fixup { keep, absorb } => {
                ops.retain(|op| !is_squash_or_fixup_with_keep(op, &keep));
                ops.push(Operation::Fixup { keep, absorb });
            }
            DraftMsg::ToggleDrop(target) => {
                let existing = ops
                    .iter()
                    .position(|op| matches!(op, Operation::Drop { target: t } if *t == target));
                match existing {
                    Some(idx) => {
                        ops.remove(idx);
                    }
                    None => ops.push(Operation::Drop { target }),
                }
            }
            DraftMsg::Reorder { new_order } => {
                ops.retain(|op| !matches!(op, Operation::Reorder { .. }));
                ops.push(Operation::Reorder { new_order });
            }
            DraftMsg::RemovePaths {
                paths,
                add_to_gitignore,
            } => {
                // Merge into an existing RemovePaths op if present, otherwise add new.
                let existing = ops
                    .iter()
                    .position(|op| matches!(op, Operation::RemovePaths { .. }));
                match existing {
                    Some(idx) => {
                        if let Operation::RemovePaths {
                            paths: ref mut existing_paths,
                            add_to_gitignore: ref mut existing_agi,
                        } = ops[idx]
                        {
                            for p in paths {
                                if !existing_paths.contains(&p) {
                                    existing_paths.push(p);
                                }
                            }
                            *existing_agi = *existing_agi || add_to_gitignore;
                        }
                    }
                    None => {
                        ops.push(Operation::RemovePaths {
                            paths,
                            add_to_gitignore,
                        });
                    }
                }
            }
            DraftMsg::ToggleFlatten(merge) => {
                let existing = ops.iter().position(
                    |op| matches!(op, Operation::FlattenMerge { merge: m } if *m == merge),
                );
                match existing {
                    Some(idx) => {
                        ops.remove(idx);
                    }
                    None => ops.push(Operation::FlattenMerge { merge }),
                }
            }
            DraftMsg::SetCoAuthors {
                targets,
                co_authors,
            } => {
                ops.retain(
                    |op| !matches!(op, Operation::SetCoAuthors { targets: t, .. } if *t == targets),
                );
                ops.push(Operation::SetCoAuthors {
                    targets,
                    co_authors,
                });
            }
            DraftMsg::RemoveOp(idx) => {
                if idx < ops.len() {
                    ops.remove(idx);
                }
            }
            DraftMsg::Clear => ops.clear(),
        }
        Self { ops }
    }

    /// Returns true if there are no draft operations.
    pub fn is_empty(&self) -> bool {
        self.ops.is_empty()
    }

    /// Returns the number of draft operations.
    pub fn len(&self) -> usize {
        self.ops.len()
    }
}

fn is_squash_or_fixup_with_keep(op: &Operation, keep: &CommitId) -> bool {
    matches!(
        op,
        Operation::Squash { keep: k, .. } | Operation::Fixup { keep: k, .. } if k == keep
    )
}

#[cfg(test)]
mod tests {
    use super::*;
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

    #[test]
    fn new_draft_is_empty() {
        let d = DraftState::new();
        assert!(d.is_empty());
        assert_eq!(d.len(), 0);
    }

    #[test]
    fn reword_adds_operation() {
        let d = DraftState::new().reduce(DraftMsg::Reword {
            target: cid("A"),
            summary: "new".into(),
            body: String::new(),
        });
        assert_eq!(d.len(), 1);
        assert!(matches!(d.ops[0], Operation::Reword { .. }));
    }

    #[test]
    fn reword_same_target_replaces() {
        let d = DraftState::new()
            .reduce(DraftMsg::Reword {
                target: cid("A"),
                summary: "first".into(),
                body: String::new(),
            })
            .reduce(DraftMsg::Reword {
                target: cid("A"),
                summary: "second".into(),
                body: String::new(),
            });
        assert_eq!(d.len(), 1);
        if let Operation::Reword { summary, .. } = &d.ops[0] {
            assert_eq!(summary, "second");
        } else {
            panic!("expected Reword");
        }
    }

    #[test]
    fn reword_different_targets_coexist() {
        let d = DraftState::new()
            .reduce(DraftMsg::Reword {
                target: cid("A"),
                summary: "a".into(),
                body: String::new(),
            })
            .reduce(DraftMsg::Reword {
                target: cid("B"),
                summary: "b".into(),
                body: String::new(),
            });
        assert_eq!(d.len(), 2);
    }

    #[test]
    fn toggle_drop_adds_then_removes() {
        let d = DraftState::new().reduce(DraftMsg::ToggleDrop(cid("A")));
        assert_eq!(d.len(), 1);
        assert!(matches!(d.ops[0], Operation::Drop { .. }));

        let d = d.reduce(DraftMsg::ToggleDrop(cid("A")));
        assert!(d.is_empty());
    }

    #[test]
    fn toggle_drop_independent_per_commit() {
        let d = DraftState::new()
            .reduce(DraftMsg::ToggleDrop(cid("A")))
            .reduce(DraftMsg::ToggleDrop(cid("B")))
            .reduce(DraftMsg::ToggleDrop(cid("A")));
        assert_eq!(d.len(), 1);
        assert!(matches!(&d.ops[0], Operation::Drop { target } if *target == cid("B")));
    }

    #[test]
    fn squash_same_keep_replaces() {
        let d = DraftState::new()
            .reduce(DraftMsg::Squash {
                keep: cid("A"),
                absorb: vec![cid("B")],
            })
            .reduce(DraftMsg::Fixup {
                keep: cid("A"),
                absorb: vec![cid("B"), cid("C")],
            });
        assert_eq!(d.len(), 1);
        assert!(matches!(d.ops[0], Operation::Fixup { .. }));
    }

    #[test]
    fn set_author_same_targets_replaces() {
        let d = DraftState::new()
            .reduce(DraftMsg::SetAuthor {
                targets: vec![cid("A"), cid("B")],
                author: ident("alice"),
            })
            .reduce(DraftMsg::SetAuthor {
                targets: vec![cid("A"), cid("B")],
                author: ident("bob"),
            });
        assert_eq!(d.len(), 1);
        if let Operation::SetAuthor { author, .. } = &d.ops[0] {
            assert_eq!(author.name, "bob");
        } else {
            panic!("expected SetAuthor");
        }
    }

    #[test]
    fn reorder_replaces_previous() {
        let d = DraftState::new()
            .reduce(DraftMsg::Reorder {
                new_order: vec![cid("A"), cid("B")],
            })
            .reduce(DraftMsg::Reorder {
                new_order: vec![cid("B"), cid("A")],
            });
        assert_eq!(d.len(), 1);
        if let Operation::Reorder { new_order } = &d.ops[0] {
            assert_eq!(new_order, &vec![cid("B"), cid("A")]);
        } else {
            panic!("expected Reorder");
        }
    }

    #[test]
    fn remove_op_by_index() {
        let d = DraftState::new()
            .reduce(DraftMsg::ToggleDrop(cid("A")))
            .reduce(DraftMsg::ToggleDrop(cid("B")))
            .reduce(DraftMsg::RemoveOp(0));
        assert_eq!(d.len(), 1);
        assert!(matches!(&d.ops[0], Operation::Drop { target } if *target == cid("B")));
    }

    #[test]
    fn remove_op_out_of_range_is_noop() {
        let d = DraftState::new()
            .reduce(DraftMsg::ToggleDrop(cid("A")))
            .reduce(DraftMsg::RemoveOp(99));
        assert_eq!(d.len(), 1);
    }

    #[test]
    fn clear_discards_everything() {
        let d = DraftState::new()
            .reduce(DraftMsg::ToggleDrop(cid("A")))
            .reduce(DraftMsg::Reword {
                target: cid("B"),
                summary: "x".into(),
                body: String::new(),
            })
            .reduce(DraftMsg::Clear);
        assert!(d.is_empty());
    }

    #[test]
    fn toggle_flatten_adds_then_removes() {
        let d = DraftState::new().reduce(DraftMsg::ToggleFlatten(cid("M")));
        assert_eq!(d.len(), 1);
        assert!(matches!(d.ops[0], Operation::FlattenMerge { .. }));

        let d = d.reduce(DraftMsg::ToggleFlatten(cid("M")));
        assert!(d.is_empty());
    }

    #[test]
    fn toggle_flatten_independent_per_merge() {
        let d = DraftState::new()
            .reduce(DraftMsg::ToggleFlatten(cid("M1")))
            .reduce(DraftMsg::ToggleFlatten(cid("M2")))
            .reduce(DraftMsg::ToggleFlatten(cid("M1")));
        assert_eq!(d.len(), 1);
        assert!(matches!(&d.ops[0], Operation::FlattenMerge { merge } if *merge == cid("M2")));
    }

    #[test]
    fn set_co_authors_adds_operation() {
        let d = DraftState::new().reduce(DraftMsg::SetCoAuthors {
            targets: vec![cid("A")],
            co_authors: vec![CoAuthor {
                name: "Alice".into(),
                email: "alice@x.com".into(),
            }],
        });
        assert_eq!(d.len(), 1);
        assert!(matches!(d.ops[0], Operation::SetCoAuthors { .. }));
    }

    #[test]
    fn set_co_authors_same_targets_replaces() {
        let d = DraftState::new()
            .reduce(DraftMsg::SetCoAuthors {
                targets: vec![cid("A")],
                co_authors: vec![CoAuthor {
                    name: "Alice".into(),
                    email: "alice@x.com".into(),
                }],
            })
            .reduce(DraftMsg::SetCoAuthors {
                targets: vec![cid("A")],
                co_authors: vec![CoAuthor {
                    name: "Bob".into(),
                    email: "bob@x.com".into(),
                }],
            });
        assert_eq!(d.len(), 1);
        if let Operation::SetCoAuthors { co_authors, .. } = &d.ops[0] {
            assert_eq!(co_authors[0].name, "Bob");
        } else {
            panic!("expected SetCoAuthors");
        }
    }

    #[test]
    fn set_co_authors_different_targets_accumulates() {
        let d = DraftState::new()
            .reduce(DraftMsg::SetCoAuthors {
                targets: vec![cid("A")],
                co_authors: vec![CoAuthor {
                    name: "Alice".into(),
                    email: "alice@x.com".into(),
                }],
            })
            .reduce(DraftMsg::SetCoAuthors {
                targets: vec![cid("B")],
                co_authors: vec![CoAuthor {
                    name: "Bob".into(),
                    email: "bob@x.com".into(),
                }],
            });
        assert_eq!(d.len(), 2);
    }
}

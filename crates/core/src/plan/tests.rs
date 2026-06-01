use super::*;
use crate::model::*;
use time::OffsetDateTime;

// ── Test helpers ───────────────────────────────────────────────

fn cid(s: &str) -> CommitId {
    CommitId(s.to_string())
}

fn test_identity(name: &str) -> Identity {
    Identity {
        name: name.to_string(),
        email: format!("{}@example.com", name.to_lowercase()),
        time: OffsetDateTime::UNIX_EPOCH,
    }
}

fn make_commit(name: &str, parents: &[&str]) -> Commit {
    Commit {
        id: cid(name),
        parents: parents.iter().map(|p| cid(p)).collect(),
        author: test_identity("Alice"),
        committer: test_identity("Alice"),
        summary: format!("Message {name}"),
        body: String::new(),
        changed_paths: vec![],
    }
}

/// 5 commits, newest-first: E→D→C→B→A (A is root).
fn linear_snapshot() -> Vec<Commit> {
    vec![
        make_commit("E", &["D"]),
        make_commit("D", &["C"]),
        make_commit("C", &["B"]),
        make_commit("B", &["A"]),
        make_commit("A", &[]),
    ]
}

/// 6 commits with a merge: E→M(D,X)→D→C→B→A  (M merges D and X).
fn snapshot_with_merge() -> Vec<Commit> {
    vec![
        make_commit("E", &["M"]),
        {
            let mut m = make_commit("M", &["D", "X"]);
            m.summary = "Merge branch 'feature'".to_string();
            m.body = "Merged feature branch".to_string();
            m
        },
        make_commit("D", &["C"]),
        make_commit("C", &["B"]),
        make_commit("B", &["A"]),
        make_commit("A", &[]),
    ]
}

// ── Validation tests ───────────────────────────────────────────

#[test]
fn empty_operations_returns_error() {
    let snap = linear_snapshot();
    let err = plan(&snap, &[]).unwrap_err();
    assert_eq!(err, PlanError::NoOperations);
}

#[test]
fn unknown_commit_id_returns_error() {
    let snap = linear_snapshot();
    let ops = vec![Operation::Reword {
        target: cid("UNKNOWN"),
        summary: "x".into(),
        body: String::new(),
    }];
    let err = plan(&snap, &ops).unwrap_err();
    assert_eq!(err, PlanError::CommitNotFound(cid("UNKNOWN")));
}

#[test]
fn flatten_non_merge_returns_error() {
    let snap = linear_snapshot();
    let ops = vec![Operation::FlattenMerge { merge: cid("C") }];
    let err = plan(&snap, &ops).unwrap_err();
    assert_eq!(err, PlanError::NotAMergeCommit(cid("C")));
}

#[test]
fn octopus_merge_returns_error() {
    let mut snap = linear_snapshot();
    snap[2].parents = vec![cid("B"), cid("X"), cid("Y")]; // 3 parents
    let ops = vec![Operation::FlattenMerge { merge: cid("C") }];
    let err = plan(&snap, &ops).unwrap_err();
    assert_eq!(err, PlanError::OctopusMergeUnsupported(cid("C")));
}

#[test]
fn squash_self_absorb_returns_error() {
    let snap = linear_snapshot();
    let ops = vec![Operation::Squash {
        keep: cid("C"),
        absorb: vec![cid("C")],
    }];
    let err = plan(&snap, &ops).unwrap_err();
    assert_eq!(err, PlanError::SelfAbsorb(cid("C")));
}

#[test]
fn squash_empty_absorb_returns_error() {
    let snap = linear_snapshot();
    let ops = vec![Operation::Squash {
        keep: cid("C"),
        absorb: vec![],
    }];
    let err = plan(&snap, &ops).unwrap_err();
    assert_eq!(err, PlanError::EmptyAbsorb(cid("C")));
}

// ── Conflict detection tests ───────────────────────────────────

#[test]
fn conflict_drop_plus_reword() {
    let snap = linear_snapshot();
    let ops = vec![
        Operation::Drop { target: cid("C") },
        Operation::Reword {
            target: cid("C"),
            summary: "x".into(),
            body: String::new(),
        },
    ];
    let err = plan(&snap, &ops).unwrap_err();
    assert!(matches!(err, PlanError::ConflictingOps(_, _, _)));
}

#[test]
fn conflict_drop_plus_set_message() {
    let snap = linear_snapshot();
    let ops = vec![
        Operation::Drop { target: cid("C") },
        Operation::SetMessage {
            targets: vec![cid("C")],
            summary: "x".into(),
            body: String::new(),
        },
    ];
    let err = plan(&snap, &ops).unwrap_err();
    assert!(matches!(err, PlanError::ConflictingOps(_, _, _)));
}

#[test]
fn conflict_drop_plus_set_author() {
    let snap = linear_snapshot();
    let ops = vec![
        Operation::Drop { target: cid("C") },
        Operation::SetAuthor {
            targets: vec![cid("C")],
            author: test_identity("Bob"),
        },
    ];
    let err = plan(&snap, &ops).unwrap_err();
    assert!(matches!(err, PlanError::ConflictingOps(_, _, _)));
}

#[test]
fn conflict_drop_plus_squash_keep() {
    let snap = linear_snapshot();
    let ops = vec![
        Operation::Drop { target: cid("C") },
        Operation::Squash {
            keep: cid("C"),
            absorb: vec![cid("D")],
        },
    ];
    let err = plan(&snap, &ops).unwrap_err();
    assert!(matches!(err, PlanError::ConflictingOps(_, _, _)));
}

#[test]
fn conflict_drop_plus_squash_absorb() {
    let snap = linear_snapshot();
    let ops = vec![
        Operation::Drop { target: cid("D") },
        Operation::Squash {
            keep: cid("C"),
            absorb: vec![cid("D")],
        },
    ];
    let err = plan(&snap, &ops).unwrap_err();
    assert!(matches!(err, PlanError::ConflictingOps(_, _, _)));
}

#[test]
fn conflict_reword_plus_set_message() {
    let snap = linear_snapshot();
    let ops = vec![
        Operation::Reword {
            target: cid("C"),
            summary: "a".into(),
            body: String::new(),
        },
        Operation::SetMessage {
            targets: vec![cid("C")],
            summary: "b".into(),
            body: String::new(),
        },
    ];
    let err = plan(&snap, &ops).unwrap_err();
    assert!(matches!(err, PlanError::ConflictingOps(_, _, _)));
}

#[test]
fn conflict_commit_is_keep_and_absorbed() {
    let snap = linear_snapshot();
    let ops = vec![
        Operation::Squash {
            keep: cid("C"),
            absorb: vec![cid("D")],
        },
        Operation::Squash {
            keep: cid("B"),
            absorb: vec![cid("C")],
        },
    ];
    let err = plan(&snap, &ops).unwrap_err();
    assert!(matches!(err, PlanError::ConflictingOps(_, _, _)));
}

#[test]
fn conflict_commit_absorbed_twice() {
    let snap = linear_snapshot();
    let ops = vec![
        Operation::Squash {
            keep: cid("B"),
            absorb: vec![cid("D")],
        },
        Operation::Squash {
            keep: cid("C"),
            absorb: vec![cid("D")],
        },
    ];
    let err = plan(&snap, &ops).unwrap_err();
    assert!(matches!(err, PlanError::ConflictingOps(_, _, _)));
}

#[test]
fn conflict_duplicate_keep() {
    let snap = linear_snapshot();
    let ops = vec![
        Operation::Squash {
            keep: cid("C"),
            absorb: vec![cid("B")],
        },
        Operation::Squash {
            keep: cid("C"),
            absorb: vec![cid("D")],
        },
    ];
    let err = plan(&snap, &ops).unwrap_err();
    assert!(matches!(err, PlanError::ConflictingOps(_, _, _)));
}

#[test]
fn conflict_multiple_reorders() {
    let snap = linear_snapshot();
    let ids: Vec<CommitId> = snap.iter().map(|c| c.id.clone()).collect();
    let ops = vec![
        Operation::Reorder {
            new_order: ids.clone(),
        },
        Operation::Reorder { new_order: ids },
    ];
    let err = plan(&snap, &ops).unwrap_err();
    assert_eq!(err, PlanError::MultipleReorders);
}

#[test]
fn invalid_reorder_permutation() {
    let snap = linear_snapshot();
    let ops = vec![Operation::Reorder {
        new_order: vec![cid("A"), cid("B")], // incomplete
    }];
    let err = plan(&snap, &ops).unwrap_err();
    assert_eq!(err, PlanError::InvalidPermutation);
}

// ── Snapshot tests: single operations ──────────────────────────

#[test]
fn reword_single_commit() {
    let snap = linear_snapshot();
    let ops = vec![Operation::Reword {
        target: cid("C"),
        summary: "Refactored C".into(),
        body: "Detailed description".into(),
    }];
    let result = plan(&snap, &ops).unwrap();
    insta::assert_yaml_snapshot!(result);
}

#[test]
fn bulk_set_message() {
    let snap = linear_snapshot();
    let ops = vec![Operation::SetMessage {
        targets: vec![cid("B"), cid("D")],
        summary: "Standardised message".into(),
        body: String::new(),
    }];
    let result = plan(&snap, &ops).unwrap();
    insta::assert_yaml_snapshot!(result);
}

#[test]
fn bulk_set_author() {
    let snap = linear_snapshot();
    let ops = vec![Operation::SetAuthor {
        targets: vec![cid("B"), cid("D")],
        author: test_identity("Bob"),
    }];
    let result = plan(&snap, &ops).unwrap();
    insta::assert_yaml_snapshot!(result);
}

#[test]
fn squash_adjacent() {
    let snap = linear_snapshot();
    let ops = vec![Operation::Squash {
        keep: cid("B"),
        absorb: vec![cid("C")],
    }];
    let result = plan(&snap, &ops).unwrap();
    insta::assert_yaml_snapshot!(result);
}

#[test]
fn squash_non_adjacent_auto_reorder() {
    let snap = linear_snapshot();
    let ops = vec![Operation::Squash {
        keep: cid("B"),
        absorb: vec![cid("D")],
    }];
    let result = plan(&snap, &ops).unwrap();
    insta::assert_yaml_snapshot!(result);
}

#[test]
fn squash_multiple_absorbed() {
    let snap = linear_snapshot();
    let ops = vec![Operation::Squash {
        keep: cid("B"),
        absorb: vec![cid("C"), cid("D")],
    }];
    let result = plan(&snap, &ops).unwrap();
    insta::assert_yaml_snapshot!(result);
}

#[test]
fn fixup_adjacent() {
    let snap = linear_snapshot();
    let ops = vec![Operation::Fixup {
        keep: cid("B"),
        absorb: vec![cid("C")],
    }];
    let result = plan(&snap, &ops).unwrap();
    insta::assert_yaml_snapshot!(result);
}

#[test]
fn fixup_non_adjacent_auto_reorder() {
    let snap = linear_snapshot();
    let ops = vec![Operation::Fixup {
        keep: cid("B"),
        absorb: vec![cid("D")],
    }];
    let result = plan(&snap, &ops).unwrap();
    insta::assert_yaml_snapshot!(result);
}

#[test]
fn drop_single_commit() {
    let snap = linear_snapshot();
    let ops = vec![Operation::Drop { target: cid("C") }];
    let result = plan(&snap, &ops).unwrap();
    insta::assert_yaml_snapshot!(result);
}

#[test]
fn drop_multiple_commits() {
    let snap = linear_snapshot();
    let ops = vec![
        Operation::Drop { target: cid("B") },
        Operation::Drop { target: cid("D") },
    ];
    let result = plan(&snap, &ops).unwrap();
    insta::assert_yaml_snapshot!(result);
}

#[test]
fn reorder_commits() {
    let snap = linear_snapshot();
    let ops = vec![Operation::Reorder {
        new_order: vec![cid("A"), cid("B"), cid("C"), cid("D"), cid("E")],
    }];
    let result = plan(&snap, &ops).unwrap();
    insta::assert_yaml_snapshot!(result);
}

#[test]
fn remove_paths() {
    let snap = linear_snapshot();
    let ops = vec![Operation::RemovePaths {
        paths: vec![PathSpec("secrets.env".into()), PathSpec("*.log".into())],
        add_to_gitignore: true,
    }];
    let result = plan(&snap, &ops).unwrap();
    insta::assert_yaml_snapshot!(result);
}

#[test]
fn remove_paths_with_empty_set_is_rejected() {
    // An empty path set would rewrite every commit id for no benefit; the plan
    // engine must refuse it rather than emit a pointless filter-repo phase.
    let snap = linear_snapshot();
    let ops = vec![Operation::RemovePaths {
        paths: vec![],
        add_to_gitignore: false,
    }];
    let err = plan(&snap, &ops).unwrap_err();
    assert_eq!(err, PlanError::EmptyPathRemoval);
}

#[test]
fn flatten_merge() {
    let snap = snapshot_with_merge();
    let ops = vec![Operation::FlattenMerge { merge: cid("M") }];
    let result = plan(&snap, &ops).unwrap();
    insta::assert_yaml_snapshot!(result);
}

// ── Snapshot tests: combined operations ─────────────────────────

#[test]
fn reword_plus_set_author_same_commit() {
    let snap = linear_snapshot();
    let ops = vec![
        Operation::Reword {
            target: cid("C"),
            summary: "New message".into(),
            body: String::new(),
        },
        Operation::SetAuthor {
            targets: vec![cid("C")],
            author: test_identity("Bob"),
        },
    ];
    let result = plan(&snap, &ops).unwrap();
    insta::assert_yaml_snapshot!(result);
}

#[test]
fn squash_plus_reword_keep() {
    let snap = linear_snapshot();
    let ops = vec![
        Operation::Squash {
            keep: cid("B"),
            absorb: vec![cid("C")],
        },
        Operation::Reword {
            target: cid("B"),
            summary: "Combined B+C".into(),
            body: "Squashed commit".into(),
        },
    ];
    let result = plan(&snap, &ops).unwrap();
    insta::assert_yaml_snapshot!(result);
}

#[test]
fn squash_with_author_change_on_keep() {
    let snap = linear_snapshot();
    let ops = vec![
        Operation::Squash {
            keep: cid("B"),
            absorb: vec![cid("C")],
        },
        Operation::SetAuthor {
            targets: vec![cid("B")],
            author: test_identity("Charlie"),
        },
    ];
    let result = plan(&snap, &ops).unwrap();
    insta::assert_yaml_snapshot!(result);
}

#[test]
fn flatten_plus_rebase_composite() {
    let snap = snapshot_with_merge();
    let ops = vec![
        Operation::FlattenMerge { merge: cid("M") },
        Operation::Reword {
            target: cid("C"),
            summary: "Updated C".into(),
            body: String::new(),
        },
    ];
    let result = plan(&snap, &ops).unwrap();
    insta::assert_yaml_snapshot!(result);
}

#[test]
fn filter_repo_plus_rebase_composite() {
    let snap = linear_snapshot();
    let ops = vec![
        Operation::RemovePaths {
            paths: vec![PathSpec("secrets.env".into())],
            add_to_gitignore: false,
        },
        Operation::Reword {
            target: cid("C"),
            summary: "Updated C".into(),
            body: String::new(),
        },
    ];
    let result = plan(&snap, &ops).unwrap();
    insta::assert_yaml_snapshot!(result);
}

#[test]
fn all_three_plan_types_composite() {
    let snap = snapshot_with_merge();
    let ops = vec![
        Operation::FlattenMerge { merge: cid("M") },
        Operation::RemovePaths {
            paths: vec![PathSpec("build/".into())],
            add_to_gitignore: true,
        },
        Operation::Drop { target: cid("C") },
    ];
    let result = plan(&snap, &ops).unwrap();
    insta::assert_yaml_snapshot!(result);
}

#[test]
fn independent_squash_groups() {
    let snap = linear_snapshot();
    let ops = vec![
        Operation::Squash {
            keep: cid("B"),
            absorb: vec![cid("C")],
        },
        Operation::Fixup {
            keep: cid("D"),
            absorb: vec![cid("E")],
        },
    ];
    let result = plan(&snap, &ops).unwrap();
    insta::assert_yaml_snapshot!(result);
}

#[test]
fn root_commit_reword() {
    let snap = linear_snapshot();
    let ops = vec![Operation::Reword {
        target: cid("A"),
        summary: "Initial commit (reworded)".into(),
        body: String::new(),
    }];
    let result = plan(&snap, &ops).unwrap();
    insta::assert_yaml_snapshot!(result);
}

// ── Order independence ─────────────────────────────────────────

#[test]
fn operations_order_independent() {
    let snap = linear_snapshot();

    let ops_forward = vec![
        Operation::Reword {
            target: cid("C"),
            summary: "New C".into(),
            body: String::new(),
        },
        Operation::SetAuthor {
            targets: vec![cid("D")],
            author: test_identity("Bob"),
        },
        Operation::Drop { target: cid("E") },
    ];

    let ops_reversed = vec![
        Operation::Drop { target: cid("E") },
        Operation::SetAuthor {
            targets: vec![cid("D")],
            author: test_identity("Bob"),
        },
        Operation::Reword {
            target: cid("C"),
            summary: "New C".into(),
            body: String::new(),
        },
    ];

    let plan_fwd = plan(&snap, &ops_forward).unwrap();
    let plan_rev = plan(&snap, &ops_reversed).unwrap();
    assert_eq!(plan_fwd, plan_rev);
}

// ── Absorbed-commit conflict tests ─────────────────────────────

#[test]
fn conflict_set_author_on_absorbed_commit() {
    let snap = linear_snapshot();
    let ops = vec![
        Operation::Squash {
            keep: cid("B"),
            absorb: vec![cid("C")],
        },
        Operation::SetAuthor {
            targets: vec![cid("C")],
            author: test_identity("Bob"),
        },
    ];
    let err = plan(&snap, &ops).unwrap_err();
    assert!(matches!(err, PlanError::ConflictingOps(_, _, _)));
}

#[test]
fn conflict_reword_on_absorbed_commit() {
    let snap = linear_snapshot();
    let ops = vec![
        Operation::Squash {
            keep: cid("B"),
            absorb: vec![cid("C")],
        },
        Operation::Reword {
            target: cid("C"),
            summary: "x".into(),
            body: String::new(),
        },
    ];
    let err = plan(&snap, &ops).unwrap_err();
    assert!(matches!(err, PlanError::ConflictingOps(_, _, _)));
}

#[test]
fn conflict_set_message_on_absorbed_commit() {
    let snap = linear_snapshot();
    let ops = vec![
        Operation::Fixup {
            keep: cid("B"),
            absorb: vec![cid("C")],
        },
        Operation::SetMessage {
            targets: vec![cid("C")],
            summary: "x".into(),
            body: String::new(),
        },
    ];
    let err = plan(&snap, &ops).unwrap_err();
    assert!(matches!(err, PlanError::ConflictingOps(_, _, _)));
}

// ── Shell escaping in author exec lines ────────────────────────

#[test]
fn author_with_special_chars_is_escaped() {
    let snap = linear_snapshot();
    let mut author = test_identity("O'Brien");
    author.name = "O\"Brien".to_string();
    author.email = "ob@example.com".to_string();
    let ops = vec![Operation::SetAuthor {
        targets: vec![cid("C")],
        author,
    }];
    let result = plan(&snap, &ops).unwrap();
    if let ExecutionPlan::Rebase(todo) = &result {
        let exec = todo.lines.iter().find_map(|l| match l {
            RebaseTodoLine::Exec(s) => Some(s),
            _ => None,
        });
        assert!(exec.is_some());
        let exec = exec.unwrap();
        // The double quote in O"Brien must be escaped
        assert!(exec.contains("O\\\"Brien"));
        assert!(!exec.contains("O\"Brien <"));
    } else {
        panic!("expected Rebase plan");
    }
}

// ── Single commit snapshot ─────────────────────────────────────

#[test]
fn single_commit_reword() {
    let snap = vec![make_commit("A", &[])];
    let ops = vec![Operation::Reword {
        target: cid("A"),
        summary: "Reworded root".into(),
        body: String::new(),
    }];
    let result = plan(&snap, &ops).unwrap();
    insta::assert_yaml_snapshot!(result);
}

// ── Property tests (proptest) ──────────────────────────────────

mod prop {
    use super::*;
    use proptest::prelude::*;

    /// Generate a linear snapshot of `n` commits (newest-first).
    fn arb_linear_snapshot(n: usize) -> Vec<Commit> {
        (0..n)
            .rev()
            .map(|i| {
                let name = format!("c{i}");
                let parent = if i > 0 {
                    vec![format!("c{}", i - 1)]
                } else {
                    vec![]
                };
                Commit {
                    id: CommitId(name.clone()),
                    parents: parent.into_iter().map(CommitId).collect(),
                    author: test_identity("Alice"),
                    committer: test_identity("Alice"),
                    summary: format!("Message {name}"),
                    body: String::new(),
                    changed_paths: vec![],
                }
            })
            .collect()
    }

    /// Strategy: pick a valid reorder permutation of `n` commit ids.
    fn arb_permutation(n: usize) -> impl Strategy<Value = Vec<CommitId>> {
        Just((0..n).collect::<Vec<usize>>())
            .prop_shuffle()
            .prop_map(|perm| {
                perm.into_iter()
                    .map(|i| CommitId(format!("c{i}")))
                    .collect()
            })
    }

    proptest! {
        #[test]
        fn reorder_is_valid_permutation(n in 3..8usize) {
            let snap = arb_linear_snapshot(n);
            let snap_ids: std::collections::HashSet<&CommitId> =
                snap.iter().map(|c| &c.id).collect();

            // Identity permutation (same order as snapshot = newest-first).
            let identity_order: Vec<CommitId> = snap.iter().map(|c| c.id.clone()).collect();
            let ops = vec![Operation::Reorder { new_order: identity_order }];
            let result = plan(&snap, &ops).unwrap();
            if let ExecutionPlan::Rebase(todo) = &result {
                // Every non-dropped commit appears exactly once.
                let todo_ids: std::collections::HashSet<&CommitId> = todo.lines.iter().filter_map(|l| match l {
                    RebaseTodoLine::Pick(id) | RebaseTodoLine::Reword(id) => Some(id),
                    _ => None,
                }).collect();
                prop_assert_eq!(todo_ids.len(), snap_ids.len());
                for id in &snap_ids {
                    prop_assert!(todo_ids.contains(*id));
                }
            } else {
                prop_assert!(false, "expected Rebase plan");
            }
        }

        #[test]
        fn shuffled_reorder_produces_correct_plan(perm in arb_permutation(5)) {
            let snap = arb_linear_snapshot(5);
            let ops = vec![Operation::Reorder { new_order: perm.clone() }];
            let result = plan(&snap, &ops).unwrap();
            if let ExecutionPlan::Rebase(todo) = &result {
                // All commits appear as Pick, exactly once.
                let pick_ids: Vec<&CommitId> = todo.lines.iter().filter_map(|l| match l {
                    RebaseTodoLine::Pick(id) => Some(id),
                    _ => None,
                }).collect();
                prop_assert_eq!(pick_ids.len(), 5);
                // The todo (oldest-first) must be the reverse of the requested
                // newest-first order — not merely some permutation. This catches
                // a builder that ignores `new_order`.
                let expected: Vec<&CommitId> = perm.iter().rev().collect();
                prop_assert_eq!(pick_ids, expected);
            } else {
                prop_assert!(false, "expected Rebase plan");
            }
        }

        /// Plan generation must be order-independent: the same set of
        /// non-conflicting operations, in any order, yields the same plan.
        #[test]
        fn arbitrary_order_independence(order in Just((0..3usize).collect::<Vec<_>>()).prop_shuffle()) {
            let snap = arb_linear_snapshot(5);
            // Three non-conflicting ops on distinct commits.
            let make = |which: usize| match which {
                0 => Operation::Reword {
                    target: snap[1].id.clone(),
                    summary: "new".into(),
                    body: String::new(),
                },
                1 => Operation::SetAuthor {
                    targets: vec![snap[2].id.clone()],
                    author: test_identity("Bob"),
                },
                _ => Operation::Drop { target: snap[3].id.clone() },
            };
            let canonical: Vec<Operation> = (0..3).map(make).collect();
            let shuffled: Vec<Operation> = order.iter().map(|&w| make(w)).collect();
            prop_assert_eq!(plan(&snap, &canonical).unwrap(), plan(&snap, &shuffled).unwrap());
        }

        /// Stronger order-independence: multiple non-conflicting ops that all
        /// target the *same* commit (reword + set-author touch different axes
        /// of one commit) must yield an identical plan regardless of input
        /// order. The distinct-commit variant above cannot catch a builder
        /// that is sensitive to per-commit op ordering.
        #[test]
        fn arbitrary_order_independence_same_commit(
            order in Just((0..2usize).collect::<Vec<_>>()).prop_shuffle()
        ) {
            let snap = arb_linear_snapshot(5);
            let make = |which: usize| match which {
                0 => Operation::Reword {
                    target: snap[2].id.clone(),
                    summary: "reworded".into(),
                    body: String::new(),
                },
                _ => Operation::SetAuthor {
                    targets: vec![snap[2].id.clone()],
                    author: test_identity("Carol"),
                },
            };
            let canonical: Vec<Operation> = (0..2).map(make).collect();
            let shuffled: Vec<Operation> = order.iter().map(|&w| make(w)).collect();
            prop_assert_eq!(plan(&snap, &canonical).unwrap(), plan(&snap, &shuffled).unwrap());
        }

        #[test]
        fn drop_reduces_pick_count(drop_idx in 1..4usize) {
            let snap = arb_linear_snapshot(5);
            let target = snap[drop_idx].id.clone();
            let ops = vec![Operation::Drop { target }];
            let result = plan(&snap, &ops).unwrap();
            if let ExecutionPlan::Rebase(todo) = &result {
                let pick_count = todo.lines.iter().filter(|l| matches!(l, RebaseTodoLine::Pick(_))).count();
                let drop_count = todo.lines.iter().filter(|l| matches!(l, RebaseTodoLine::Drop(_))).count();
                prop_assert_eq!(pick_count, 4);
                prop_assert_eq!(drop_count, 1);
                prop_assert_eq!(todo.lines.len(), 5);
            } else {
                prop_assert!(false, "expected Rebase plan");
            }
        }

        #[test]
        fn squash_preserves_total_commit_count(
            keep_idx in 0..4usize,
            absorb_offset in 1..4usize,
        ) {
            let snap = arb_linear_snapshot(5);
            let absorb_idx = (keep_idx + absorb_offset) % 5;
            if keep_idx == absorb_idx {
                return Ok(()); // skip identical indices
            }
            let keep = snap[keep_idx].id.clone();
            let absorb = snap[absorb_idx].id.clone();
            let ops = vec![Operation::Squash { keep, absorb: vec![absorb] }];
            let result = plan(&snap, &ops).unwrap();
            if let ExecutionPlan::Rebase(todo) = &result {
                // Total lines should equal snapshot length.
                prop_assert_eq!(todo.lines.len(), 5);
                // Exactly one Squash line.
                let squash_count = todo.lines.iter().filter(|l| matches!(l, RebaseTodoLine::Squash(_))).count();
                prop_assert_eq!(squash_count, 1);
            } else {
                prop_assert!(false, "expected Rebase plan");
            }
        }

        #[test]
        fn plan_is_deterministic(n in 3..6usize) {
            let snap = arb_linear_snapshot(n);
            let ops = vec![Operation::Reword {
                target: snap[0].id.clone(),
                summary: "new".into(),
                body: String::new(),
            }];
            let plan1 = plan(&snap, &ops).unwrap();
            let plan2 = plan(&snap, &ops).unwrap();
            prop_assert_eq!(plan1, plan2);
        }
    }
}

// ── SetCoAuthors tests ─────────────────────────────────────────

#[test]
fn set_co_authors_produces_reword_with_trailers() {
    let mut snap = linear_snapshot();
    snap[2].body = "Original body".to_string();
    let ops = vec![Operation::SetCoAuthors {
        targets: vec![cid("C")],
        co_authors: vec![crate::model::CoAuthor {
            name: "Alice".into(),
            email: "alice@x.com".into(),
        }],
    }];
    let result = plan(&snap, &ops).unwrap();
    if let ExecutionPlan::Rebase(todo) = result {
        // Should have a message_map entry for C with co-author trailer.
        let msg = todo.message_map.iter().find(|(id, _)| *id == cid("C"));
        assert!(msg.is_some(), "expected message_map entry for C");
        let (_, content) = msg.unwrap();
        assert!(content.contains("Co-authored-by: Alice <alice@x.com>"));
        assert!(content.contains("Message C"));
    } else {
        panic!("expected Rebase plan");
    }
}

#[test]
fn set_co_authors_removes_existing_trailers() {
    let mut snap = linear_snapshot();
    snap[2].body = "Body text\n\nCo-authored-by: Old <old@x.com>".to_string();
    let ops = vec![Operation::SetCoAuthors {
        targets: vec![cid("C")],
        co_authors: vec![crate::model::CoAuthor {
            name: "New".into(),
            email: "new@x.com".into(),
        }],
    }];
    let result = plan(&snap, &ops).unwrap();
    if let ExecutionPlan::Rebase(todo) = result {
        let (_, content) = todo
            .message_map
            .iter()
            .find(|(id, _)| *id == cid("C"))
            .unwrap();
        assert!(!content.contains("Old"));
        assert!(content.contains("Co-authored-by: New <new@x.com>"));
        assert!(content.contains("Body text"));
    } else {
        panic!("expected Rebase plan");
    }
}

#[test]
fn set_co_authors_empty_clears_trailers() {
    let mut snap = linear_snapshot();
    snap[2].body = "Body\n\nCo-authored-by: Alice <a@x.com>".to_string();
    let ops = vec![Operation::SetCoAuthors {
        targets: vec![cid("C")],
        co_authors: vec![],
    }];
    let result = plan(&snap, &ops).unwrap();
    if let ExecutionPlan::Rebase(todo) = result {
        let (_, content) = todo
            .message_map
            .iter()
            .find(|(id, _)| *id == cid("C"))
            .unwrap();
        assert!(!content.contains("Co-authored-by"));
        assert!(content.contains("Body"));
    } else {
        panic!("expected Rebase plan");
    }
}

#[test]
fn set_co_authors_combined_with_reword() {
    let snap = linear_snapshot();
    let ops = vec![
        Operation::Reword {
            target: cid("C"),
            summary: "New summary".into(),
            body: "New body".into(),
        },
        Operation::SetCoAuthors {
            targets: vec![cid("C")],
            co_authors: vec![crate::model::CoAuthor {
                name: "Alice".into(),
                email: "alice@x.com".into(),
            }],
        },
    ];
    let result = plan(&snap, &ops).unwrap();
    if let ExecutionPlan::Rebase(todo) = result {
        let (_, content) = todo
            .message_map
            .iter()
            .find(|(id, _)| *id == cid("C"))
            .unwrap();
        assert!(content.contains("New summary"));
        assert!(content.contains("New body"));
        assert!(content.contains("Co-authored-by: Alice <alice@x.com>"));
    } else {
        panic!("expected Rebase plan");
    }
}

#[test]
fn set_co_authors_conflicts_with_drop() {
    let snap = linear_snapshot();
    let ops = vec![
        Operation::Drop { target: cid("C") },
        Operation::SetCoAuthors {
            targets: vec![cid("C")],
            co_authors: vec![],
        },
    ];
    let err = plan(&snap, &ops).unwrap_err();
    assert!(matches!(err, PlanError::ConflictingOps(_, _, _)));
}

#[test]
fn set_co_authors_bulk_targets() {
    let snap = linear_snapshot();
    let ops = vec![Operation::SetCoAuthors {
        targets: vec![cid("B"), cid("C"), cid("D")],
        co_authors: vec![crate::model::CoAuthor {
            name: "Bob".into(),
            email: "bob@x.com".into(),
        }],
    }];
    let result = plan(&snap, &ops).unwrap();
    if let ExecutionPlan::Rebase(todo) = result {
        assert_eq!(todo.message_map.len(), 3);
        for (_, content) in &todo.message_map {
            assert!(content.contains("Co-authored-by: Bob <bob@x.com>"));
        }
    } else {
        panic!("expected Rebase plan");
    }
}

// ── OID remap across rewrite phases ────────────────────────────

fn omap(pairs: &[(&str, Option<&str>)]) -> OidMap {
    pairs.iter().map(|(k, v)| (cid(k), v.map(cid))).collect()
}

#[test]
fn remap_rebase_todo_retargets_all_ids() {
    let map = omap(&[("a", Some("A")), ("b", Some("B")), ("c", Some("C"))]);
    let todo = RebaseTodo {
        base: Some(cid("a")),
        lines: vec![
            RebaseTodoLine::Pick(cid("b")),
            RebaseTodoLine::Reword(cid("c")),
            RebaseTodoLine::Exec("git commit --amend".into()),
        ],
        message_map: vec![(cid("c"), "msg".into())],
        author_map: vec![(cid("b"), test_identity("Alice"))],
    };

    let remapped = ExecutionPlan::Rebase(todo).remap_oids(&map).unwrap();
    let ExecutionPlan::Rebase(t) = remapped else {
        panic!("expected Rebase");
    };
    assert_eq!(t.base, Some(cid("A")));
    assert_eq!(t.lines[0], RebaseTodoLine::Pick(cid("B")));
    assert_eq!(t.lines[1], RebaseTodoLine::Reword(cid("C")));
    assert_eq!(
        t.lines[2],
        RebaseTodoLine::Exec("git commit --amend".into())
    );
    assert_eq!(t.message_map[0].0, cid("C"));
    assert_eq!(t.author_map[0].0, cid("B"));
}

#[test]
fn remap_leaves_unmapped_ids_unchanged() {
    // `b` is not in the map → identity.
    let map = omap(&[("a", Some("A"))]);
    let plan = ExecutionPlan::Flatten(FlattenSpec {
        merge: cid("a"),
        mainline_parent: cid("b"),
        message: "m".into(),
    });
    let ExecutionPlan::Flatten(spec) = plan.remap_oids(&map).unwrap() else {
        panic!("expected Flatten");
    };
    assert_eq!(spec.merge, cid("A"));
    assert_eq!(spec.mainline_parent, cid("b"));
}

#[test]
fn remap_dropped_target_errors() {
    // A commit dropped by a prior rewrite can no longer be a target.
    let map = omap(&[("a", None)]);
    let plan = ExecutionPlan::Rebase(RebaseTodo {
        base: None,
        lines: vec![RebaseTodoLine::Reword(cid("a"))],
        message_map: vec![],
        author_map: vec![],
    });
    assert_eq!(
        plan.remap_oids(&map),
        Err(PlanError::CommitNotFound(cid("a")))
    );
}

#[test]
fn remap_empty_map_is_identity_clone() {
    let plan = ExecutionPlan::Rebase(RebaseTodo {
        base: Some(cid("x")),
        lines: vec![RebaseTodoLine::Pick(cid("y"))],
        message_map: vec![],
        author_map: vec![],
    });
    assert_eq!(plan.remap_oids(&OidMap::new()).unwrap(), plan);
}

#[test]
fn compose_chains_two_phases() {
    // Phase 1: a→A, b→B (b later dropped), d dropped.
    let first = omap(&[("a", Some("A")), ("b", Some("B")), ("d", None)]);
    // Phase 2 (keyed by post-phase-1 ids): A→A2, B dropped, plus new id e→E.
    let second = omap(&[("A", Some("A2")), ("B", None), ("e", Some("E"))]);

    let composed = compose_oid_maps(&first, &second);
    assert_eq!(composed.get(&cid("a")), Some(&Some(cid("A2"))));
    assert_eq!(composed.get(&cid("b")), Some(&None)); // dropped in phase 2
    assert_eq!(composed.get(&cid("d")), Some(&None)); // dropped in phase 1
    // `e` was identity through phase 1, rewritten by phase 2.
    assert_eq!(composed.get(&cid("e")), Some(&Some(cid("E"))));
}

#[test]
fn compose_carries_unchanged_first_phase_ids() {
    // `a` unchanged in phase 1 (absent), rewritten in phase 2.
    let first = omap(&[("b", Some("B"))]);
    let second = omap(&[("a", Some("A2"))]);
    let composed = compose_oid_maps(&first, &second);
    assert_eq!(composed.get(&cid("a")), Some(&Some(cid("A2"))));
    // `b`→B in phase 1, B unchanged in phase 2 → stays B.
    assert_eq!(composed.get(&cid("b")), Some(&Some(cid("B"))));
}

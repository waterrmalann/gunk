use gunk_core::{
    CommitId, ExecutionPlan, FilterRepoSpec, FlattenSpec, Identity, Operation, PathSpec,
    RebaseTodo, RebaseTodoLine, plan,
};
use gunk_gitio::{
    Git, check_clean, create_backup_ref, execute_filter_repo, execute_flatten, execute_plan,
    execute_rebase, format_rebase_todo, has_filter_repo, list_backup_refs, restore_backup,
    stash_pop, stash_push,
};
use gunk_testkit::RepoFixture;
use time::OffsetDateTime;

// ── Safety: dirty tree check ───────────────────────────────────────

#[test]
fn check_clean_on_clean_repo_succeeds() {
    let mut fixture = RepoFixture::new();
    fixture.commit("c1", "init", &[("f.txt", "hello")]);

    let git = Git::open(fixture.path()).unwrap();
    assert!(check_clean(&git).is_ok());
}

#[test]
fn check_clean_on_dirty_repo_returns_error() {
    let mut fixture = RepoFixture::new();
    fixture.commit("c1", "init", &[("f.txt", "hello")]);

    // Dirty the working tree.
    std::fs::write(fixture.path().join("f.txt"), "modified").unwrap();

    let git = Git::open(fixture.path()).unwrap();
    let err = check_clean(&git).unwrap_err();
    assert!(
        err.to_string().contains("dirty"),
        "expected dirty tree error, got: {err}"
    );
}

#[test]
fn check_clean_on_staged_changes_returns_error() {
    let mut fixture = RepoFixture::new();
    fixture.commit("c1", "init", &[("f.txt", "hello")]);

    // Stage a change without committing.
    std::fs::write(fixture.path().join("f.txt"), "staged").unwrap();
    fixture.git(["add", "f.txt"]);

    let git = Git::open(fixture.path()).unwrap();
    let err = check_clean(&git).unwrap_err();
    assert!(err.to_string().contains("dirty"));
}

// ── Safety: stash ──────────────────────────────────────────────────

#[test]
fn stash_push_and_pop_round_trips() {
    let mut fixture = RepoFixture::new();
    fixture.commit("c1", "init", &[("f.txt", "original")]);

    // Dirty the tree.
    std::fs::write(fixture.path().join("f.txt"), "dirty").unwrap();

    let git = Git::open(fixture.path()).unwrap();

    // Stash should succeed.
    let stashed = stash_push(&git).unwrap();
    assert!(stashed, "should have stashed something");

    // Tree should now be clean.
    assert!(check_clean(&git).is_ok());

    // File should be back to original.
    let content = std::fs::read_to_string(fixture.path().join("f.txt")).unwrap();
    assert_eq!(content, "original");

    // Pop should restore the dirty state.
    stash_pop(&git).unwrap();
    let content = std::fs::read_to_string(fixture.path().join("f.txt")).unwrap();
    assert_eq!(content, "dirty");
}

#[test]
fn stash_push_on_clean_tree_returns_false() {
    let mut fixture = RepoFixture::new();
    fixture.commit("c1", "init", &[("f.txt", "hello")]);

    let git = Git::open(fixture.path()).unwrap();
    let stashed = stash_push(&git).unwrap();
    assert!(!stashed, "nothing to stash on clean tree");
}

// ── Backup refs ────────────────────────────────────────────────────

#[test]
fn create_backup_ref_creates_restorable_ref() {
    let mut fixture = RepoFixture::new();
    fixture.commit("c1", "init", &[("f.txt", "hello")]);

    let git = Git::open(fixture.path()).unwrap();
    let original_tip = fixture.rev_parse("main");

    let ref_name = create_backup_ref(&git, "main").unwrap();
    assert!(ref_name.starts_with("refs/gunk/backup/main/"));

    // Ref should point at the original tip.
    let ref_oid = fixture.rev_parse(&ref_name);
    assert_eq!(ref_oid, original_tip);
}

#[test]
fn list_backup_refs_returns_refs_for_branch() {
    let mut fixture = RepoFixture::new();
    fixture.commit("c1", "init", &[("f.txt", "hello")]);

    let git = Git::open(fixture.path()).unwrap();

    // Create a backup ref (just one to avoid timestamp collision).
    let _ref1 = create_backup_ref(&git, "main").unwrap();

    let refs = list_backup_refs(&git, "main").unwrap();
    assert!(!refs.is_empty(), "expected at least 1 backup ref");

    // All should point at the same commit.
    let tip = fixture.rev_parse("main");
    for (_, oid) in &refs {
        assert_eq!(oid, &tip);
    }
}

#[test]
fn restore_backup_resets_branch_to_backup() {
    let mut fixture = RepoFixture::new();
    fixture.commit("c1", "first", &[("f.txt", "one")]);
    let original_tip = fixture.rev_parse("main").to_string();

    fixture.commit("c2", "second", &[("f.txt", "two")]);
    let new_tip = fixture.rev_parse("main").to_string();
    assert_ne!(original_tip, new_tip);

    let git = Git::open(fixture.path()).unwrap();

    // Manually create a backup ref pointing at original_tip.
    git.run(["update-ref", "refs/gunk/backup/main/12345", &original_tip])
        .unwrap();

    // Restore.
    restore_backup(&git, "main", "refs/gunk/backup/main/12345").unwrap();

    // Branch should now point at original.
    let current = fixture.rev_parse("main");
    assert_eq!(current, original_tip);
}

// ── Rebase todo formatting ─────────────────────────────────────────

#[test]
fn format_rebase_todo_produces_correct_text() {
    let todo = RebaseTodo {
        base: Some(CommitId("abc123".to_string())),
        lines: vec![
            RebaseTodoLine::Pick(CommitId("aaa".to_string())),
            RebaseTodoLine::Squash(CommitId("bbb".to_string())),
            RebaseTodoLine::Reword(CommitId("ccc".to_string())),
            RebaseTodoLine::Fixup(CommitId("ddd".to_string())),
            RebaseTodoLine::Drop(CommitId("eee".to_string())),
            RebaseTodoLine::Exec("git commit --amend --no-edit".to_string()),
        ],
        message_map: vec![],
        author_map: vec![],
    };

    let text = format_rebase_todo(&todo);
    let expected = "\
pick aaa
squash bbb
reword ccc
fixup ddd
drop eee
exec git commit --amend --no-edit
";
    assert_eq!(text, expected);
}

// ── Execution: rebase drop ─────────────────────────────────────────

#[test]
fn execute_rebase_drops_commit() {
    let mut fixture = RepoFixture::new();
    fixture.commit("c1", "first", &[("a.txt", "a")]);
    fixture.commit("c2", "second", &[("b.txt", "b")]);
    fixture.commit("c3", "third", &[("c.txt", "c")]);

    let git = Git::open(fixture.path()).unwrap();
    let commits = git.walk_commits("main").unwrap();
    // commits: [c3, c2, c1] (newest first)

    // Plan: drop c2.
    let operations = vec![Operation::Drop {
        target: commits[1].id.clone(),
    }];

    let plan_result = plan(&commits, &operations).unwrap();
    let todo = match plan_result {
        ExecutionPlan::Rebase(todo) => todo,
        _ => panic!("expected rebase plan"),
    };

    let result = execute_rebase(&git, "main", &todo).unwrap();

    // Verify: history should have 2 commits now.
    let new_commits = git.walk_commits("main").unwrap();
    assert_eq!(new_commits.len(), 2);
    assert_eq!(new_commits[0].summary, "third");
    assert_eq!(new_commits[1].summary, "first");

    // Backup ref should exist.
    assert!(result.backup_ref.starts_with("refs/gunk/backup/main/"));
    let backup_oid = fixture.rev_parse(&result.backup_ref);
    assert_eq!(backup_oid, commits[0].id.0); // original tip
}

#[test]
fn execute_rebase_squashes_commits() {
    let mut fixture = RepoFixture::new();
    fixture.commit("c1", "first", &[("a.txt", "a")]);
    fixture.commit("c2", "second", &[("b.txt", "b")]);
    fixture.commit("c3", "third", &[("c.txt", "c")]);

    let git = Git::open(fixture.path()).unwrap();
    let commits = git.walk_commits("main").unwrap();

    // Plan: squash c2 into c1 (absorb c2 into c1).
    let operations = vec![Operation::Squash {
        keep: commits[2].id.clone(),         // c1
        absorb: vec![commits[1].id.clone()], // c2
    }];

    let plan_result = plan(&commits, &operations).unwrap();
    let todo = match plan_result {
        ExecutionPlan::Rebase(todo) => todo,
        _ => panic!("expected rebase plan"),
    };

    let _result = execute_rebase(&git, "main", &todo).unwrap();

    // Verify: history should have 2 commits (c1+c2 squashed, c3).
    let new_commits = git.walk_commits("main").unwrap();
    assert_eq!(new_commits.len(), 2);
    assert_eq!(new_commits[0].summary, "third");

    // The squashed commit must retain both source messages (git's default
    // squash combines them). The kept commit's summary leads; the absorbed
    // message survives in the combined body.
    let squashed = &new_commits[1];
    let full_message = format!("{}\n{}", squashed.summary, squashed.body);
    assert!(
        full_message.contains("first"),
        "squashed message should contain kept message 'first', got: {full_message:?}"
    );
    assert!(
        full_message.contains("second"),
        "squashed message should contain absorbed message 'second', got: {full_message:?}"
    );
}

#[test]
fn execute_rebase_fixups_commits() {
    let mut fixture = RepoFixture::new();
    fixture.commit("c1", "first", &[("a.txt", "a")]);
    fixture.commit("c2", "second", &[("b.txt", "b")]);
    fixture.commit("c3", "third", &[("c.txt", "c")]);

    let git = Git::open(fixture.path()).unwrap();
    let commits = git.walk_commits("main").unwrap();

    // Plan: fixup c2 into c1 (discard c2's message).
    let operations = vec![Operation::Fixup {
        keep: commits[2].id.clone(),         // c1
        absorb: vec![commits[1].id.clone()], // c2
    }];

    let plan_result = plan(&commits, &operations).unwrap();
    let todo = match plan_result {
        ExecutionPlan::Rebase(todo) => todo,
        _ => panic!("expected rebase plan"),
    };

    let _result = execute_rebase(&git, "main", &todo).unwrap();

    let new_commits = git.walk_commits("main").unwrap();
    assert_eq!(new_commits.len(), 2);
    assert_eq!(new_commits[0].summary, "third");
    // The fixuped commit should only have c1's message.
    assert_eq!(new_commits[1].summary, "first");
}

// ── Execution: dirty tree refused ──────────────────────────────────

#[test]
fn execute_rebase_refuses_on_dirty_tree() {
    let mut fixture = RepoFixture::new();
    fixture.commit("c1", "first", &[("a.txt", "a")]);
    fixture.commit("c2", "second", &[("b.txt", "b")]);

    // Dirty the tree.
    std::fs::write(fixture.path().join("a.txt"), "dirty").unwrap();

    let git = Git::open(fixture.path()).unwrap();
    let commits = git.walk_commits("main").unwrap();

    let operations = vec![Operation::Drop {
        target: commits[0].id.clone(),
    }];
    let plan_result = plan(&commits, &operations).unwrap();
    let todo = match plan_result {
        ExecutionPlan::Rebase(todo) => todo,
        _ => panic!("expected rebase plan"),
    };

    let err = execute_rebase(&git, "main", &todo).unwrap_err();
    assert!(
        err.to_string().contains("dirty"),
        "expected dirty tree error, got: {err}"
    );

    // Branch should be untouched.
    let current_commits = git.walk_commits("main").unwrap();
    assert_eq!(current_commits.len(), 2);
}

// ── Execution: backup exists on failure ────────────────────────────

#[test]
fn execute_rebase_branch_untouched_on_conflict() {
    let mut fixture = RepoFixture::new();
    // Create a scenario that will conflict during rebase:
    // c1: creates file.txt with "line1"
    // c2: modifies file.txt to "line2"
    // c3: modifies file.txt to "line3"
    // Reordering c2 and c3 will cause a conflict since c3 depends on c2.
    fixture.commit("c1", "first", &[("file.txt", "line1")]);
    fixture.commit("c2", "second", &[("file.txt", "line2")]);
    fixture.commit("c3", "third", &[("file.txt", "line3")]);

    let git = Git::open(fixture.path()).unwrap();
    let commits = git.walk_commits("main").unwrap();
    let original_tip = commits[0].id.0.clone();

    // Reorder: put c3 before c2 (will conflict).
    let operations = vec![Operation::Reorder {
        new_order: vec![
            commits[2].id.clone(), // c3 first (was last)
            commits[0].id.clone(), // c1 second (was first)
            commits[1].id.clone(), // c2 third (was middle)
        ],
    }];

    let plan_result = plan(&commits, &operations).unwrap();
    let todo = match plan_result {
        ExecutionPlan::Rebase(todo) => todo,
        _ => panic!("expected rebase plan"),
    };

    let err = execute_rebase(&git, "main", &todo);
    // This should fail (conflict or rehearsal failure).
    assert!(err.is_err(), "rebase with conflict should fail");

    // The real branch must be untouched.
    let current_tip = fixture.rev_parse("main");
    assert_eq!(
        current_tip, original_tip,
        "branch must not be mutated on failure"
    );
}

// ── Execution: restore from backup ────────────────────────────────

#[test]
fn restore_backup_after_successful_rebase() {
    let mut fixture = RepoFixture::new();
    fixture.commit("c1", "first", &[("a.txt", "a")]);
    fixture.commit("c2", "second", &[("b.txt", "b")]);
    fixture.commit("c3", "third", &[("c.txt", "c")]);

    let git = Git::open(fixture.path()).unwrap();
    let commits = git.walk_commits("main").unwrap();
    let original_tip = commits[0].id.0.clone();

    // Drop c2.
    let operations = vec![Operation::Drop {
        target: commits[1].id.clone(),
    }];
    let plan_result = plan(&commits, &operations).unwrap();
    let todo = match plan_result {
        ExecutionPlan::Rebase(todo) => todo,
        _ => panic!("expected rebase plan"),
    };

    let result = execute_rebase(&git, "main", &todo).unwrap();

    // History changed.
    let after = git.walk_commits("main").unwrap();
    assert_eq!(after.len(), 2);

    // Restore.
    restore_backup(&git, "main", &result.backup_ref).unwrap();

    // History should be back to original.
    let restored = git.walk_commits("main").unwrap();
    assert_eq!(restored.len(), 3);
    assert_eq!(restored[0].id.0, original_tip);
}

// ── Execution: reorder succeeds ────────────────────────────────────

#[test]
fn execute_rebase_reorders_independent_commits() {
    let mut fixture = RepoFixture::new();
    // Create commits that touch different files (no conflicts on reorder).
    fixture.commit("c1", "first", &[("a.txt", "a")]);
    fixture.commit("c2", "second", &[("b.txt", "b")]);
    fixture.commit("c3", "third", &[("c.txt", "c")]);

    let git = Git::open(fixture.path()).unwrap();
    let commits = git.walk_commits("main").unwrap();
    // commits: [c3, c2, c1]

    // Reorder: c2, c3, c1 (newest-first in UI terms).
    let operations = vec![Operation::Reorder {
        new_order: vec![
            commits[1].id.clone(), // c2 on top
            commits[0].id.clone(), // c3
            commits[2].id.clone(), // c1 at bottom
        ],
    }];

    let plan_result = plan(&commits, &operations).unwrap();
    let todo = match plan_result {
        ExecutionPlan::Rebase(todo) => todo,
        _ => panic!("expected rebase plan"),
    };

    let _result = execute_rebase(&git, "main", &todo).unwrap();

    // Verify new order (newest first).
    let new_commits = git.walk_commits("main").unwrap();
    assert_eq!(new_commits.len(), 3);
    assert_eq!(new_commits[0].summary, "second");
    assert_eq!(new_commits[1].summary, "third");
    assert_eq!(new_commits[2].summary, "first");

    // All files should still exist.
    assert!(fixture.path().join("a.txt").exists());
    assert!(fixture.path().join("b.txt").exists());
    assert!(fixture.path().join("c.txt").exists());
}

// ── Worktree cleanup on success ────────────────────────────────────

#[test]
fn worktree_is_cleaned_up_after_execution() {
    let mut fixture = RepoFixture::new();
    fixture.commit("c1", "first", &[("a.txt", "a")]);
    fixture.commit("c2", "second", &[("b.txt", "b")]);

    let git = Git::open(fixture.path()).unwrap();
    let commits = git.walk_commits("main").unwrap();

    let operations = vec![Operation::Drop {
        target: commits[0].id.clone(),
    }];
    let plan_result = plan(&commits, &operations).unwrap();
    let todo = match plan_result {
        ExecutionPlan::Rebase(todo) => todo,
        _ => panic!("expected rebase plan"),
    };

    let _ = execute_rebase(&git, "main", &todo).unwrap();

    // Worktree directory should not exist.
    let worktree_path = fixture.path().join(".gunk-rehearsal");
    assert!(
        !worktree_path.exists(),
        "worktree should be cleaned up after execution"
    );
}

// ── Worktree cleanup on failure ────────────────────────────────────

#[test]
fn worktree_is_cleaned_up_on_failure() {
    let mut fixture = RepoFixture::new();
    fixture.commit("c1", "first", &[("file.txt", "line1")]);
    fixture.commit("c2", "second", &[("file.txt", "line2")]);
    fixture.commit("c3", "third", &[("file.txt", "line3")]);

    let git = Git::open(fixture.path()).unwrap();
    let commits = git.walk_commits("main").unwrap();

    // Reorder to cause conflict.
    let operations = vec![Operation::Reorder {
        new_order: vec![
            commits[2].id.clone(),
            commits[0].id.clone(),
            commits[1].id.clone(),
        ],
    }];

    let plan_result = plan(&commits, &operations).unwrap();
    let todo = match plan_result {
        ExecutionPlan::Rebase(todo) => todo,
        _ => panic!("expected rebase plan"),
    };

    // All three commits touch the same file, so reordering them must conflict.
    let original_tip = git
        .run(["rev-parse", "main"])
        .unwrap()
        .stdout
        .trim()
        .to_string();
    let result = execute_rebase(&git, "main", &todo);

    // The rehearsal must fail (otherwise this test asserts nothing meaningful).
    assert!(
        result.is_err(),
        "reordering conflicting commits should fail the rehearsal"
    );

    // Safety guarantee: the real branch is untouched after a failed rehearsal.
    let tip_after = git
        .run(["rev-parse", "main"])
        .unwrap()
        .stdout
        .trim()
        .to_string();
    assert_eq!(
        original_tip, tip_after,
        "real branch must be untouched when the rehearsal fails"
    );

    // A backup ref was created before the rehearsal and survives the failure.
    let backups = list_backup_refs(&git, "main").unwrap();
    assert!(
        !backups.is_empty(),
        "a backup ref should exist even when the rehearsal fails"
    );

    // Worktree directory should not exist regardless.
    let worktree_path = fixture.path().join(".gunk-rehearsal");
    assert!(
        !worktree_path.exists(),
        "worktree should be cleaned up even on failure"
    );
}

// ── Parentage assertions ───────────────────────────────────────────

#[test]
fn execute_rebase_drop_preserves_linear_parentage() {
    let mut fixture = RepoFixture::new();
    fixture.commit("c1", "first", &[("a.txt", "a")]);
    fixture.commit("c2", "second", &[("b.txt", "b")]);
    fixture.commit("c3", "third", &[("c.txt", "c")]);

    let git = Git::open(fixture.path()).unwrap();
    let commits = git.walk_commits("main").unwrap();

    // Drop the middle commit.
    let operations = vec![Operation::Drop {
        target: commits[1].id.clone(),
    }];
    let plan_result = plan(&commits, &operations).unwrap();
    let todo = match plan_result {
        ExecutionPlan::Rebase(todo) => todo,
        _ => panic!("expected rebase plan"),
    };

    let _result = execute_rebase(&git, "main", &todo).unwrap();

    // Verify parentage: c3' -> c1' and c1' is root.
    let new_commits = git.walk_commits("main").unwrap();
    assert_eq!(new_commits.len(), 2);
    // Third commit should have first as parent.
    assert_eq!(new_commits[0].parents.len(), 1);
    assert_eq!(new_commits[0].parents[0], new_commits[1].id);
    // First commit is root (no parents).
    assert!(new_commits[1].parents.is_empty());
}

// ── Pushed commits warning ─────────────────────────────────────────

#[test]
fn check_pushed_commits_empty_when_no_remotes() {
    use gunk_gitio::check_pushed_commits;

    let mut fixture = RepoFixture::new();
    fixture.commit("c1", "init", &[("f.txt", "hello")]);

    let git = Git::open(fixture.path()).unwrap();
    let commits = git.walk_commits("main").unwrap();

    let pushed = check_pushed_commits(&git, &[&commits[0].id.0]).unwrap();
    assert!(pushed.is_empty(), "no remotes means no pushed commits");
}

// ── Execute on different branch than HEAD ──────────────────────────

#[test]
fn execute_rebase_on_non_checked_out_branch() {
    let mut fixture = RepoFixture::new();
    fixture.commit("c1", "first", &[("a.txt", "a")]);
    fixture.checkout_new_branch("feature");
    fixture.commit("c2", "feature work", &[("b.txt", "b")]);
    fixture.commit("c3", "more feature", &[("c.txt", "c")]);
    fixture.checkout("main");

    let git = Git::open(fixture.path()).unwrap();
    let feature_commits = git.walk_commits("feature").unwrap();

    // Drop a commit on `feature` while on `main`.
    let operations = vec![Operation::Drop {
        target: feature_commits[0].id.clone(),
    }];
    let plan_result = plan(&feature_commits, &operations).unwrap();
    let todo = match plan_result {
        ExecutionPlan::Rebase(todo) => todo,
        _ => panic!("expected rebase plan"),
    };

    let result = execute_rebase(&git, "feature", &todo).unwrap();

    // Feature branch should be rewritten.
    let new_feature = git.walk_commits("feature").unwrap();
    assert_eq!(new_feature.len(), 2);
    assert_eq!(new_feature[0].summary, "feature work");

    // Main should be untouched.
    let main_commits = git.walk_commits("main").unwrap();
    assert_eq!(main_commits.len(), 1);
    assert_eq!(main_commits[0].summary, "first");

    // Backup ref should exist.
    assert!(result.backup_ref.starts_with("refs/gunk/backup/feature/"));
}

// ════════════════════════════════════════════════════════════════════
// Phase 5 — End-to-end wiring of rebase-class features
// ════════════════════════════════════════════════════════════════════

// ── Reword ─────────────────────────────────────────────────────────

#[test]
fn execute_rebase_rewords_single_commit() {
    let mut fixture = RepoFixture::new();
    fixture.commit("c1", "first", &[("a.txt", "a")]);
    fixture.commit("c2", "second", &[("b.txt", "b")]);
    fixture.commit("c3", "third", &[("c.txt", "c")]);

    let git = Git::open(fixture.path()).unwrap();
    let commits = git.walk_commits("main").unwrap();
    // commits: [c3, c2, c1]

    // Reword c2 (middle commit).
    let operations = vec![Operation::Reword {
        target: commits[1].id.clone(),
        summary: "reworded second".to_string(),
        body: String::new(),
    }];

    let plan_result = plan(&commits, &operations).unwrap();
    let todo = match plan_result {
        ExecutionPlan::Rebase(todo) => todo,
        _ => panic!("expected rebase plan"),
    };

    let _result = execute_rebase(&git, "main", &todo).unwrap();

    let new_commits = git.walk_commits("main").unwrap();
    assert_eq!(new_commits.len(), 3);
    assert_eq!(new_commits[0].summary, "third");
    assert_eq!(new_commits[1].summary, "reworded second");
    assert_eq!(new_commits[2].summary, "first");
}

#[test]
fn execute_rebase_rewords_with_multiline_message() {
    let mut fixture = RepoFixture::new();
    fixture.commit("c1", "first", &[("a.txt", "a")]);
    fixture.commit("c2", "second", &[("b.txt", "b")]);

    let git = Git::open(fixture.path()).unwrap();
    let commits = git.walk_commits("main").unwrap();

    let operations = vec![Operation::Reword {
        target: commits[0].id.clone(),
        summary: "new subject".to_string(),
        body: "This is a detailed body.\n\nWith multiple paragraphs.".to_string(),
    }];

    let plan_result = plan(&commits, &operations).unwrap();
    let todo = match plan_result {
        ExecutionPlan::Rebase(todo) => todo,
        _ => panic!("expected rebase plan"),
    };

    let _result = execute_rebase(&git, "main", &todo).unwrap();

    let new_commits = git.walk_commits("main").unwrap();
    assert_eq!(new_commits[0].summary, "new subject");
    assert!(
        new_commits[0].body.contains("With multiple paragraphs."),
        "body should contain multiline content, got: {:?}",
        new_commits[0].body
    );
}

#[test]
fn execute_rebase_rewords_with_special_characters() {
    let mut fixture = RepoFixture::new();
    fixture.commit("c1", "first", &[("a.txt", "a")]);
    fixture.commit("c2", "second", &[("b.txt", "b")]);

    let git = Git::open(fixture.path()).unwrap();
    let commits = git.walk_commits("main").unwrap();

    let operations = vec![Operation::Reword {
        target: commits[0].id.clone(),
        summary: "fix: handle \"quotes\" & $pecial chars".to_string(),
        body: "Backticks `code` and single 'quotes' too.".to_string(),
    }];

    let plan_result = plan(&commits, &operations).unwrap();
    let todo = match plan_result {
        ExecutionPlan::Rebase(todo) => todo,
        _ => panic!("expected rebase plan"),
    };

    let _result = execute_rebase(&git, "main", &todo).unwrap();

    let new_commits = git.walk_commits("main").unwrap();
    assert_eq!(
        new_commits[0].summary,
        "fix: handle \"quotes\" & $pecial chars"
    );
    assert!(new_commits[0].body.contains("Backticks `code`"));
}

#[test]
fn execute_rebase_rewords_root_commit() {
    let mut fixture = RepoFixture::new();
    fixture.commit("c1", "initial", &[("a.txt", "a")]);
    fixture.commit("c2", "second", &[("b.txt", "b")]);

    let git = Git::open(fixture.path()).unwrap();
    let commits = git.walk_commits("main").unwrap();

    // Reword the root commit.
    let operations = vec![Operation::Reword {
        target: commits[1].id.clone(), // c1 is the root (oldest)
        summary: "reworded root".to_string(),
        body: String::new(),
    }];

    let plan_result = plan(&commits, &operations).unwrap();
    let todo = match plan_result {
        ExecutionPlan::Rebase(todo) => todo,
        _ => panic!("expected rebase plan"),
    };

    let _result = execute_rebase(&git, "main", &todo).unwrap();

    let new_commits = git.walk_commits("main").unwrap();
    assert_eq!(new_commits.len(), 2);
    assert_eq!(new_commits[1].summary, "reworded root");
    assert_eq!(new_commits[0].summary, "second");
}

// ── Bulk set-message ───────────────────────────────────────────────

#[test]
fn execute_rebase_bulk_set_message() {
    let mut fixture = RepoFixture::new();
    fixture.commit("c1", "first", &[("a.txt", "a")]);
    fixture.commit("c2", "second", &[("b.txt", "b")]);
    fixture.commit("c3", "third", &[("c.txt", "c")]);

    let git = Git::open(fixture.path()).unwrap();
    let commits = git.walk_commits("main").unwrap();

    // Set the same message on c1 and c3.
    let operations = vec![Operation::SetMessage {
        targets: vec![commits[2].id.clone(), commits[0].id.clone()],
        summary: "unified message".to_string(),
        body: String::new(),
    }];

    let plan_result = plan(&commits, &operations).unwrap();
    let todo = match plan_result {
        ExecutionPlan::Rebase(todo) => todo,
        _ => panic!("expected rebase plan"),
    };

    let _result = execute_rebase(&git, "main", &todo).unwrap();

    let new_commits = git.walk_commits("main").unwrap();
    assert_eq!(new_commits.len(), 3);
    assert_eq!(new_commits[0].summary, "unified message");
    assert_eq!(new_commits[1].summary, "second"); // untouched
    assert_eq!(new_commits[2].summary, "unified message");
}

// ── Set-author ─────────────────────────────────────────────────────

#[test]
fn execute_rebase_set_author_single_commit() {
    let mut fixture = RepoFixture::new();
    fixture.commit("c1", "first", &[("a.txt", "a")]);
    fixture.commit("c2", "second", &[("b.txt", "b")]);

    let git = Git::open(fixture.path()).unwrap();
    let commits = git.walk_commits("main").unwrap();

    let operations = vec![Operation::SetAuthor {
        targets: vec![commits[0].id.clone()],
        author: Identity {
            name: "New Author".to_string(),
            email: "new@example.com".to_string(),
            time: OffsetDateTime::now_utc(),
        },
    }];

    let plan_result = plan(&commits, &operations).unwrap();
    let todo = match plan_result {
        ExecutionPlan::Rebase(todo) => todo,
        _ => panic!("expected rebase plan"),
    };

    let _result = execute_rebase(&git, "main", &todo).unwrap();

    let new_commits = git.walk_commits("main").unwrap();
    assert_eq!(new_commits[0].author.name, "New Author");
    assert_eq!(new_commits[0].author.email, "new@example.com");
    // Message should be preserved.
    assert_eq!(new_commits[0].summary, "second");
}

#[test]
fn execute_rebase_set_author_bulk() {
    let mut fixture = RepoFixture::new();
    fixture.commit("c1", "first", &[("a.txt", "a")]);
    fixture.commit("c2", "second", &[("b.txt", "b")]);
    fixture.commit("c3", "third", &[("c.txt", "c")]);

    let git = Git::open(fixture.path()).unwrap();
    let commits = git.walk_commits("main").unwrap();

    let operations = vec![Operation::SetAuthor {
        targets: vec![
            commits[0].id.clone(),
            commits[1].id.clone(),
            commits[2].id.clone(),
        ],
        author: Identity {
            name: "Bulk Author".to_string(),
            email: "bulk@example.com".to_string(),
            time: OffsetDateTime::now_utc(),
        },
    }];

    let plan_result = plan(&commits, &operations).unwrap();
    let todo = match plan_result {
        ExecutionPlan::Rebase(todo) => todo,
        _ => panic!("expected rebase plan"),
    };

    let _result = execute_rebase(&git, "main", &todo).unwrap();

    let new_commits = git.walk_commits("main").unwrap();
    for c in &new_commits {
        assert_eq!(c.author.name, "Bulk Author");
        assert_eq!(c.author.email, "bulk@example.com");
    }
}

#[test]
fn execute_rebase_set_author_with_special_characters() {
    let mut fixture = RepoFixture::new();
    fixture.commit("c1", "first", &[("a.txt", "a")]);
    fixture.commit("c2", "second", &[("b.txt", "b")]);

    let git = Git::open(fixture.path()).unwrap();
    let commits = git.walk_commits("main").unwrap();

    // The author name flows through an `exec git commit --amend --author=...`
    // shell line in the rebase todo, so shell-special characters (quotes,
    // ampersands, dollar signs, spaces) must survive escaping end-to-end.
    let operations = vec![Operation::SetAuthor {
        targets: vec![commits[0].id.clone()],
        author: Identity {
            name: "O'Brien & \"Co.\" $USER".to_string(),
            email: "weird+name@example.com".to_string(),
            time: OffsetDateTime::now_utc(),
        },
    }];

    let plan_result = plan(&commits, &operations).unwrap();
    let todo = match plan_result {
        ExecutionPlan::Rebase(todo) => todo,
        _ => panic!("expected rebase plan"),
    };

    let _result = execute_rebase(&git, "main", &todo).unwrap();

    let new_commits = git.walk_commits("main").unwrap();
    assert_eq!(new_commits[0].author.name, "O'Brien & \"Co.\" $USER");
    assert_eq!(new_commits[0].author.email, "weird+name@example.com");
    // Message should be preserved untouched.
    assert_eq!(new_commits[0].summary, "second");
}

// ── Reword with empty body ─────────────────────────────────────────

#[test]
fn execute_rebase_rewords_with_empty_body() {
    let mut fixture = RepoFixture::new();
    fixture.commit("c1", "first\n\noriginal body here", &[("a.txt", "a")]);
    fixture.commit("c2", "second", &[("b.txt", "b")]);

    let git = Git::open(fixture.path()).unwrap();
    let commits = git.walk_commits("main").unwrap();

    // Reword to have only a summary (no body).
    let operations = vec![Operation::Reword {
        target: commits[1].id.clone(),
        summary: "clean subject only".to_string(),
        body: String::new(),
    }];

    let plan_result = plan(&commits, &operations).unwrap();
    let todo = match plan_result {
        ExecutionPlan::Rebase(todo) => todo,
        _ => panic!("expected rebase plan"),
    };

    let _result = execute_rebase(&git, "main", &todo).unwrap();

    let new_commits = git.walk_commits("main").unwrap();
    assert_eq!(new_commits[1].summary, "clean subject only");
    assert!(
        new_commits[1].body.trim().is_empty(),
        "body should be empty after reword, got: {:?}",
        new_commits[1].body
    );
}

// ── Squash with prepared message ───────────────────────────────────

#[test]
fn execute_rebase_squash_with_custom_message() {
    let mut fixture = RepoFixture::new();
    fixture.commit("c1", "first", &[("a.txt", "a")]);
    fixture.commit("c2", "second", &[("b.txt", "b")]);
    fixture.commit("c3", "third", &[("c.txt", "c")]);

    let git = Git::open(fixture.path()).unwrap();
    let commits = git.walk_commits("main").unwrap();

    // Squash c2 into c1 AND reword c1 with a specific message.
    let operations = vec![
        Operation::Squash {
            keep: commits[2].id.clone(),         // c1
            absorb: vec![commits[1].id.clone()], // c2
        },
        Operation::Reword {
            target: commits[2].id.clone(),
            summary: "combined: first and second".to_string(),
            body: "This squash combines both changes.".to_string(),
        },
    ];

    let plan_result = plan(&commits, &operations).unwrap();
    let todo = match plan_result {
        ExecutionPlan::Rebase(todo) => todo,
        _ => panic!("expected rebase plan"),
    };

    let _result = execute_rebase(&git, "main", &todo).unwrap();

    let new_commits = git.walk_commits("main").unwrap();
    assert_eq!(new_commits.len(), 2);
    assert_eq!(new_commits[0].summary, "third");
    assert_eq!(new_commits[1].summary, "combined: first and second");
    assert!(
        new_commits[1].body.contains("squash combines both"),
        "body should contain the custom message, got: {:?}",
        new_commits[1].body
    );

    // Both files should exist.
    assert!(fixture.path().join("a.txt").exists());
    assert!(fixture.path().join("b.txt").exists());
}

#[test]
fn execute_rebase_squash_preserves_combined_message_when_no_reword() {
    let mut fixture = RepoFixture::new();
    fixture.commit("c1", "first", &[("a.txt", "a")]);
    fixture.commit("c2", "second", &[("b.txt", "b")]);

    let git = Git::open(fixture.path()).unwrap();
    let commits = git.walk_commits("main").unwrap();

    // Squash without reword: should preserve combined messages.
    let operations = vec![Operation::Squash {
        keep: commits[1].id.clone(),
        absorb: vec![commits[0].id.clone()],
    }];

    let plan_result = plan(&commits, &operations).unwrap();
    let todo = match plan_result {
        ExecutionPlan::Rebase(todo) => todo,
        _ => panic!("expected rebase plan"),
    };

    let _result = execute_rebase(&git, "main", &todo).unwrap();

    let new_commits = git.walk_commits("main").unwrap();
    assert_eq!(new_commits.len(), 1);
    // Default squash combines messages — both should appear.
    let full_msg = fixture.message(&new_commits[0].id.0);
    assert!(
        full_msg.contains("first") && full_msg.contains("second"),
        "combined message should contain both subjects, got: {full_msg:?}"
    );
}

// ── Fixup ──────────────────────────────────────────────────────────

#[test]
fn execute_rebase_fixup_discards_absorbed_message() {
    let mut fixture = RepoFixture::new();
    fixture.commit("c1", "first", &[("a.txt", "a")]);
    fixture.commit("c2", "fixme: typo fix", &[("a.txt", "a-fixed")]);
    fixture.commit("c3", "third", &[("c.txt", "c")]);

    let git = Git::open(fixture.path()).unwrap();
    let commits = git.walk_commits("main").unwrap();

    let operations = vec![Operation::Fixup {
        keep: commits[2].id.clone(),         // c1
        absorb: vec![commits[1].id.clone()], // c2
    }];

    let plan_result = plan(&commits, &operations).unwrap();
    let todo = match plan_result {
        ExecutionPlan::Rebase(todo) => todo,
        _ => panic!("expected rebase plan"),
    };

    let _result = execute_rebase(&git, "main", &todo).unwrap();

    let new_commits = git.walk_commits("main").unwrap();
    assert_eq!(new_commits.len(), 2);
    assert_eq!(new_commits[0].summary, "third");
    // Fixup: only the keep commit's message survives.
    assert_eq!(new_commits[1].summary, "first");
    let full_msg = fixture.message(&new_commits[1].id.0);
    assert!(
        !full_msg.contains("fixme"),
        "fixup should discard absorbed message, got: {full_msg:?}"
    );
}

// ── Combined operations ────────────────────────────────────────────

#[test]
fn execute_rebase_combined_reword_and_author() {
    let mut fixture = RepoFixture::new();
    fixture.commit("c1", "first", &[("a.txt", "a")]);
    fixture.commit("c2", "second", &[("b.txt", "b")]);

    let git = Git::open(fixture.path()).unwrap();
    let commits = git.walk_commits("main").unwrap();

    // Reword AND change author on the same commit.
    let operations = vec![
        Operation::Reword {
            target: commits[0].id.clone(),
            summary: "reworded".to_string(),
            body: String::new(),
        },
        Operation::SetAuthor {
            targets: vec![commits[0].id.clone()],
            author: Identity {
                name: "New Person".to_string(),
                email: "new@example.com".to_string(),
                time: OffsetDateTime::now_utc(),
            },
        },
    ];

    let plan_result = plan(&commits, &operations).unwrap();
    let todo = match plan_result {
        ExecutionPlan::Rebase(todo) => todo,
        _ => panic!("expected rebase plan"),
    };

    let _result = execute_rebase(&git, "main", &todo).unwrap();

    let new_commits = git.walk_commits("main").unwrap();
    assert_eq!(new_commits[0].summary, "reworded");
    assert_eq!(new_commits[0].author.name, "New Person");
    assert_eq!(new_commits[0].author.email, "new@example.com");
}

#[test]
fn execute_rebase_combined_squash_reword_author_drop() {
    let mut fixture = RepoFixture::new();
    fixture.commit("c1", "first", &[("a.txt", "a")]);
    fixture.commit("c2", "second", &[("b.txt", "b")]);
    fixture.commit("c3", "third", &[("c.txt", "c")]);
    fixture.commit("c4", "fourth", &[("d.txt", "d")]);

    let git = Git::open(fixture.path()).unwrap();
    let commits = git.walk_commits("main").unwrap();
    // commits: [c4, c3, c2, c1]

    let operations = vec![
        // Squash c2 into c1 with a custom message.
        Operation::Squash {
            keep: commits[3].id.clone(),         // c1
            absorb: vec![commits[2].id.clone()], // c2
        },
        Operation::Reword {
            target: commits[3].id.clone(),
            summary: "merged first+second".to_string(),
            body: String::new(),
        },
        // Drop c3.
        Operation::Drop {
            target: commits[1].id.clone(),
        },
        // Change author on c4.
        Operation::SetAuthor {
            targets: vec![commits[0].id.clone()],
            author: Identity {
                name: "Changed".to_string(),
                email: "changed@example.com".to_string(),
                time: OffsetDateTime::now_utc(),
            },
        },
    ];

    let plan_result = plan(&commits, &operations).unwrap();
    let todo = match plan_result {
        ExecutionPlan::Rebase(todo) => todo,
        _ => panic!("expected rebase plan"),
    };

    let _result = execute_rebase(&git, "main", &todo).unwrap();

    let new_commits = git.walk_commits("main").unwrap();
    // Should have 2 commits: c4 (author changed) and c1+c2 (squashed+reworded).
    assert_eq!(new_commits.len(), 2);

    // c4 should have the new author.
    assert_eq!(new_commits[0].summary, "fourth");
    assert_eq!(new_commits[0].author.name, "Changed");

    // c1+c2 should be squashed with custom message.
    assert_eq!(new_commits[1].summary, "merged first+second");

    // All relevant files should exist (c3's file might be gone since it was dropped).
    assert!(fixture.path().join("a.txt").exists());
    assert!(fixture.path().join("b.txt").exists());
    assert!(fixture.path().join("d.txt").exists());
}

// ── Message file cleanup ───────────────────────────────────────────

#[test]
fn execute_rebase_cleans_up_message_files() {
    let mut fixture = RepoFixture::new();
    fixture.commit("c1", "first", &[("a.txt", "a")]);
    fixture.commit("c2", "second", &[("b.txt", "b")]);

    let git = Git::open(fixture.path()).unwrap();
    let commits = git.walk_commits("main").unwrap();

    let operations = vec![Operation::Reword {
        target: commits[0].id.clone(),
        summary: "reworded".to_string(),
        body: String::new(),
    }];

    let plan_result = plan(&commits, &operations).unwrap();
    let todo = match plan_result {
        ExecutionPlan::Rebase(todo) => todo,
        _ => panic!("expected rebase plan"),
    };

    let _result = execute_rebase(&git, "main", &todo).unwrap();

    // Worktree and all temp files should be cleaned up.
    assert!(
        !fixture.path().join(".gunk-rehearsal").exists(),
        "worktree should be removed after execution"
    );
}

// ════════════════════════════════════════════════════════════════════
// Phase 6 — Remove files from history (git-filter-repo)
// ════════════════════════════════════════════════════════════════════

// ── Detection ──────────────────────────────────────────────────────

#[test]
fn has_filter_repo_returns_bool() {
    let mut fixture = RepoFixture::new();
    fixture.commit("c1", "init", &[("f.txt", "x")]);
    let git = Git::open(fixture.path()).unwrap();
    // Just verify it returns without panic; the result depends on the system.
    let _ = has_filter_repo(&git);
}

// ── Single path removal ───────────────────────────────────────────

#[test]
fn filter_repo_removes_single_file_from_history() {
    let mut fixture = RepoFixture::new();
    fixture.commit("c1", "add secret", &[("secret.env", "API_KEY=xxx")]);
    fixture.commit("c2", "add code", &[("main.rs", "fn main() {}")]);
    fixture.commit(
        "c3",
        "update code",
        &[("main.rs", "fn main() { println!(\"hi\"); }")],
    );

    let git = Git::open(fixture.path()).unwrap();
    if !has_filter_repo(&git) {
        eprintln!("SKIPPED: git-filter-repo not installed");
        return;
    }

    let spec = FilterRepoSpec {
        paths: vec![PathSpec("secret.env".into())],
        add_to_gitignore: false,
    };

    let result = execute_filter_repo(&git, "main", &spec).unwrap();

    // Backup ref should exist.
    assert!(result.backup_ref.starts_with("refs/gunk/backup/main/"));

    // The file should not exist in the working tree.
    assert!(
        !fixture.path().join("secret.env").exists(),
        "secret.env should be removed from working tree"
    );

    // The file should not appear in the branch's history.
    let log = fixture.git(["log", "main", "--diff-filter=A", "--name-only", "--format="]);
    assert!(
        !log.contains("secret.env"),
        "secret.env should not appear in branch history, got: {log}"
    );

    // main.rs should still exist.
    assert!(fixture.path().join("main.rs").exists());
}

// ── Glob pattern removal ──────────────────────────────────────────

#[test]
fn filter_repo_removes_glob_pattern() {
    let mut fixture = RepoFixture::new();
    fixture.commit(
        "c1",
        "add logs",
        &[("app.log", "log1"), ("debug.log", "log2")],
    );
    fixture.commit("c2", "add code", &[("main.rs", "fn main() {}")]);

    let git = Git::open(fixture.path()).unwrap();
    if !has_filter_repo(&git) {
        eprintln!("SKIPPED: git-filter-repo not installed");
        return;
    }

    let spec = FilterRepoSpec {
        paths: vec![PathSpec("*.log".into())],
        add_to_gitignore: false,
    };

    let result = execute_filter_repo(&git, "main", &spec).unwrap();
    assert!(result.backup_ref.starts_with("refs/gunk/backup/main/"));

    // Log files should be gone.
    assert!(!fixture.path().join("app.log").exists());
    assert!(!fixture.path().join("debug.log").exists());

    // Code should remain.
    assert!(fixture.path().join("main.rs").exists());
}

// ── Path that exists only in old commits ──────────────────────────

#[test]
fn filter_repo_removes_file_deleted_in_later_commit() {
    let mut fixture = RepoFixture::new();
    fixture.commit("c1", "add temp file", &[("temp.dat", "temporary data")]);
    fixture.commit("c2", "add code", &[("main.rs", "fn main() {}")]);
    // Delete the file in c3 — but it's still in history.
    std::fs::remove_file(fixture.path().join("temp.dat")).unwrap();
    fixture.git(["add", "-A"]);
    fixture.commit("c3", "clean up", &[]);

    let git = Git::open(fixture.path()).unwrap();
    if !has_filter_repo(&git) {
        eprintln!("SKIPPED: git-filter-repo not installed");
        return;
    }

    let spec = FilterRepoSpec {
        paths: vec![PathSpec("temp.dat".into())],
        add_to_gitignore: false,
    };

    let _result = execute_filter_repo(&git, "main", &spec).unwrap();

    // File should not appear in the branch's history.
    let log = fixture.git(["log", "main", "--diff-filter=A", "--name-only", "--format="]);
    assert!(
        !log.contains("temp.dat"),
        "temp.dat should be purged from branch history"
    );
}

// ── Binary file removal ───────────────────────────────────────────

#[test]
fn filter_repo_removes_binary_file() {
    let mut fixture = RepoFixture::new();
    // Write some binary content (non-UTF8 bytes).
    let binary_content = vec![0x00, 0xFF, 0xFE, 0xAB, 0xCD];
    let binary_path = fixture.path().join("image.bin");
    std::fs::write(&binary_path, &binary_content).unwrap();
    fixture.git(["add", "image.bin"]);
    fixture.commit("c1", "add binary", &[]);
    fixture.commit("c2", "add code", &[("main.rs", "fn main() {}")]);

    let git = Git::open(fixture.path()).unwrap();
    if !has_filter_repo(&git) {
        eprintln!("SKIPPED: git-filter-repo not installed");
        return;
    }

    let spec = FilterRepoSpec {
        paths: vec![PathSpec("image.bin".into())],
        add_to_gitignore: false,
    };

    let _result = execute_filter_repo(&git, "main", &spec).unwrap();

    assert!(!fixture.path().join("image.bin").exists());
}

// ── add_to_gitignore behavior ─────────────────────────────────────

#[test]
fn filter_repo_appends_to_gitignore_when_requested() {
    let mut fixture = RepoFixture::new();
    fixture.commit("c1", "add secrets", &[("secret.env", "key=val")]);
    fixture.commit("c2", "add code", &[("main.rs", "fn main() {}")]);

    let git = Git::open(fixture.path()).unwrap();
    if !has_filter_repo(&git) {
        eprintln!("SKIPPED: git-filter-repo not installed");
        return;
    }

    let spec = FilterRepoSpec {
        paths: vec![PathSpec("secret.env".into())],
        add_to_gitignore: true,
    };

    let _result = execute_filter_repo(&git, "main", &spec).unwrap();

    // .gitignore should exist and contain the removed path.
    let gitignore = std::fs::read_to_string(fixture.path().join(".gitignore")).unwrap();
    assert!(
        gitignore.contains("secret.env"),
        ".gitignore should contain 'secret.env', got: {gitignore}"
    );

    // The .gitignore commit should be in history.
    let log = fixture.log();
    assert!(
        log.iter()
            .any(|(_, msg)| msg.contains("add removed paths to .gitignore")),
        "should have a gitignore commit, log: {log:?}"
    );
}

#[test]
fn filter_repo_preserves_existing_gitignore_content() {
    let mut fixture = RepoFixture::new();
    fixture.commit(
        "c1",
        "initial",
        &[(".gitignore", "target/\n*.tmp\n"), ("secret.env", "x")],
    );
    fixture.commit("c2", "add code", &[("main.rs", "fn main() {}")]);

    let git = Git::open(fixture.path()).unwrap();
    if !has_filter_repo(&git) {
        eprintln!("SKIPPED: git-filter-repo not installed");
        return;
    }

    let spec = FilterRepoSpec {
        paths: vec![PathSpec("secret.env".into())],
        add_to_gitignore: true,
    };

    let _result = execute_filter_repo(&git, "main", &spec).unwrap();

    let gitignore = std::fs::read_to_string(fixture.path().join(".gitignore")).unwrap();
    // Existing entries should still be present.
    assert!(gitignore.contains("target/"), "should preserve 'target/'");
    assert!(gitignore.contains("*.tmp"), "should preserve '*.tmp'");
    // New entry should also be present.
    assert!(gitignore.contains("secret.env"), "should add 'secret.env'");
}

// ── Dirty tree refused ────────────────────────────────────────────

#[test]
fn filter_repo_refuses_on_dirty_tree() {
    let mut fixture = RepoFixture::new();
    fixture.commit("c1", "init", &[("f.txt", "hello"), ("secret.env", "x")]);

    // Dirty the tree.
    std::fs::write(fixture.path().join("f.txt"), "modified").unwrap();

    let git = Git::open(fixture.path()).unwrap();

    let spec = FilterRepoSpec {
        paths: vec![PathSpec("secret.env".into())],
        add_to_gitignore: false,
    };

    let err = execute_filter_repo(&git, "main", &spec).unwrap_err();
    assert!(
        err.to_string().contains("dirty"),
        "expected dirty tree error, got: {err}"
    );
}

// ── Backup ref exists after filter-repo ───────────────────────────

#[test]
fn filter_repo_creates_backup_ref() {
    let mut fixture = RepoFixture::new();
    fixture.commit("c1", "add secret", &[("secret.env", "key=val")]);
    fixture.commit("c2", "add code", &[("main.rs", "fn main() {}")]);
    let original_tip = fixture.rev_parse("main");

    let git = Git::open(fixture.path()).unwrap();
    if !has_filter_repo(&git) {
        eprintln!("SKIPPED: git-filter-repo not installed");
        return;
    }

    let spec = FilterRepoSpec {
        paths: vec![PathSpec("secret.env".into())],
        add_to_gitignore: false,
    };

    let result = execute_filter_repo(&git, "main", &spec).unwrap();

    // Backup ref should point at the original tip.
    let backup_oid = fixture.rev_parse(&result.backup_ref);
    assert_eq!(backup_oid, original_tip);

    // New tip should be different (history was rewritten).
    assert_ne!(result.new_tip, original_tip);
}

// ── Restore from backup after filter-repo ─────────────────────────

#[test]
fn restore_after_filter_repo() {
    let mut fixture = RepoFixture::new();
    fixture.commit("c1", "add secret", &[("secret.env", "key=val")]);
    fixture.commit("c2", "add code", &[("main.rs", "fn main() {}")]);

    let git = Git::open(fixture.path()).unwrap();
    if !has_filter_repo(&git) {
        eprintln!("SKIPPED: git-filter-repo not installed");
        return;
    }

    let original_tip = fixture.rev_parse("main");
    let original_commits = git.walk_commits("main").unwrap();

    let spec = FilterRepoSpec {
        paths: vec![PathSpec("secret.env".into())],
        add_to_gitignore: false,
    };

    let result = execute_filter_repo(&git, "main", &spec).unwrap();

    // Restore.
    restore_backup(&git, "main", &result.backup_ref).unwrap();

    // Branch should be back to original.
    let restored_tip = fixture.rev_parse("main");
    assert_eq!(restored_tip, original_tip);

    // The file should be back.
    assert!(fixture.path().join("secret.env").exists());

    // History should be restored.
    let restored_commits = git.walk_commits("main").unwrap();
    assert_eq!(restored_commits.len(), original_commits.len());
}

// ── execute_plan dispatches correctly ─────────────────────────────

#[test]
fn execute_plan_handles_rebase() {
    let mut fixture = RepoFixture::new();
    fixture.commit("c1", "first", &[("a.txt", "a")]);
    fixture.commit("c2", "second", &[("b.txt", "b")]);
    fixture.commit("c3", "third", &[("c.txt", "c")]);

    let git = Git::open(fixture.path()).unwrap();
    let commits = git.walk_commits("main").unwrap();

    let operations = vec![Operation::Drop {
        target: commits[1].id.clone(),
    }];
    let exec_plan = plan(&commits, &operations).unwrap();

    let result = execute_plan(&git, "main", &exec_plan).unwrap();

    let new_commits = git.walk_commits("main").unwrap();
    assert_eq!(new_commits.len(), 2);
    assert!(result.backup_ref.starts_with("refs/gunk/backup/main/"));
}

#[test]
fn execute_plan_handles_filter_repo() {
    let mut fixture = RepoFixture::new();
    fixture.commit("c1", "add secret", &[("secret.env", "key=val")]);
    fixture.commit("c2", "add code", &[("main.rs", "fn main() {}")]);

    let git = Git::open(fixture.path()).unwrap();
    if !has_filter_repo(&git) {
        eprintln!("SKIPPED: git-filter-repo not installed");
        return;
    }

    let commits = git.walk_commits("main").unwrap();
    let operations = vec![Operation::RemovePaths {
        paths: vec![PathSpec("secret.env".into())],
        add_to_gitignore: false,
    }];

    let exec_plan = plan(&commits, &operations).unwrap();
    let result = execute_plan(&git, "main", &exec_plan).unwrap();

    assert!(!fixture.path().join("secret.env").exists());
    assert!(result.backup_ref.starts_with("refs/gunk/backup/main/"));
}

// ── Composite: filter-repo only (no rebase) ───────────────────────

#[test]
fn execute_plan_filter_repo_only_via_plan() {
    let mut fixture = RepoFixture::new();
    fixture.commit(
        "c1",
        "add secret",
        &[("secret.env", "key=val"), ("main.rs", "fn main() {}")],
    );

    let git = Git::open(fixture.path()).unwrap();
    if !has_filter_repo(&git) {
        eprintln!("SKIPPED: git-filter-repo not installed");
        return;
    }

    let commits = git.walk_commits("main").unwrap();
    let operations = vec![Operation::RemovePaths {
        paths: vec![PathSpec("secret.env".into())],
        add_to_gitignore: true,
    }];

    let exec_plan = plan(&commits, &operations).unwrap();
    let result = execute_plan(&git, "main", &exec_plan).unwrap();

    // File removed.
    assert!(!fixture.path().join("secret.env").exists());
    // .gitignore updated.
    let gitignore = std::fs::read_to_string(fixture.path().join(".gitignore")).unwrap();
    assert!(gitignore.contains("secret.env"));
    // Backup exists.
    assert!(result.backup_ref.starts_with("refs/gunk/backup/main/"));
}

// ── Multiple path removal at once ─────────────────────────────────

#[test]
fn filter_repo_removes_multiple_paths() {
    let mut fixture = RepoFixture::new();
    fixture.commit(
        "c1",
        "add files",
        &[
            ("secret.env", "key=val"),
            ("debug.log", "log data"),
            ("main.rs", "fn main() {}"),
        ],
    );

    let git = Git::open(fixture.path()).unwrap();
    if !has_filter_repo(&git) {
        eprintln!("SKIPPED: git-filter-repo not installed");
        return;
    }

    let spec = FilterRepoSpec {
        paths: vec![PathSpec("secret.env".into()), PathSpec("debug.log".into())],
        add_to_gitignore: false,
    };

    let _result = execute_filter_repo(&git, "main", &spec).unwrap();

    assert!(!fixture.path().join("secret.env").exists());
    assert!(!fixture.path().join("debug.log").exists());
    assert!(fixture.path().join("main.rs").exists());
}

// ══════════════════════════════════════════════════════════════════
// Phase 7 — Flatten merge
// ══════════════════════════════════════════════════════════════════

/// Helper: create a fixture with a merge.
///
/// Layout (newest first):
///   c4 "after merge"  ← main
///   M  "Merge feature" (parents: c2, c3)
///   c3 "feature work"  ← was on feature branch
///   c2 "second"
///   c1 "first"          ← root
fn fixture_with_merge() -> (RepoFixture, Vec<gunk_core::Commit>) {
    let mut fixture = RepoFixture::new();
    fixture.commit("c1", "first", &[("a.txt", "a")]);
    fixture.commit("c2", "second", &[("b.txt", "b")]);
    fixture.checkout_new_branch("feature");
    fixture.commit("c3", "feature work", &[("c.txt", "c")]);
    fixture.checkout("main");
    fixture.merge("M", "feature", "Merge feature");
    fixture.commit("c4", "after merge", &[("d.txt", "d")]);

    let git = Git::open(fixture.path()).unwrap();
    let commits = git.walk_commits("main").unwrap();
    (fixture, commits)
}

// ── Flatten: basic merge at tip ────────────────────────────────────

#[test]
fn flatten_merge_at_tip() {
    let mut fixture = RepoFixture::new();
    fixture.commit("c1", "first", &[("a.txt", "a")]);
    fixture.commit("c2", "second", &[("b.txt", "b")]);
    fixture.checkout_new_branch("feature");
    fixture.commit("c3", "feature work", &[("c.txt", "c")]);
    fixture.checkout("main");
    fixture.merge("M", "feature", "Merge feature");
    // M is the branch tip.

    let git = Git::open(fixture.path()).unwrap();
    let commits = git.walk_commits("main").unwrap();
    let merge_commit = commits.iter().find(|c| c.is_merge()).unwrap();

    let spec = FlattenSpec {
        merge: merge_commit.id.clone(),
        mainline_parent: merge_commit.parents[0].clone(),
        message: merge_commit.summary.clone(),
    };

    let result = execute_flatten(&git, "main", &spec).unwrap();

    // Result should have a backup ref.
    assert!(result.backup_ref.starts_with("refs/gunk/backup/main/"));

    // New history should be linear (no merge commits).
    let new_commits = git.walk_commits("main").unwrap();
    for c in &new_commits {
        assert!(
            c.parents.len() <= 1,
            "expected linear history, but commit {} has {} parents",
            c.id.short(),
            c.parents.len()
        );
    }

    // The tree at the new tip should be identical to the original merge tree.
    let new_tree = fixture.git(["rev-parse", &format!("{}^{{tree}}", result.new_tip)]);
    let old_tree = fixture.git(["rev-parse", &format!("{}^{{tree}}", merge_commit.id.0)]);
    assert_eq!(
        new_tree.trim(),
        old_tree.trim(),
        "flattened commit tree should be byte-identical to merge tree"
    );

    // All files should still exist.
    assert!(fixture.path().join("a.txt").exists());
    assert!(fixture.path().join("b.txt").exists());
    assert!(fixture.path().join("c.txt").exists());
}

// ── Flatten: merge in the middle with descendants ──────────────────

#[test]
fn flatten_merge_in_middle_rebases_descendants() {
    let (fixture, commits) = fixture_with_merge();
    let merge_commit = commits.iter().find(|c| c.is_merge()).unwrap();

    let git = Git::open(fixture.path()).unwrap();

    let spec = FlattenSpec {
        merge: merge_commit.id.clone(),
        mainline_parent: merge_commit.parents[0].clone(),
        message: merge_commit.summary.clone(),
    };

    let result = execute_flatten(&git, "main", &spec).unwrap();

    // History should be linear.
    let new_commits = git.walk_commits("main").unwrap();
    for c in &new_commits {
        assert!(
            c.parents.len() <= 1,
            "expected linear history after flatten"
        );
    }

    // Should still have a commit after the flattened merge (c4).
    let summaries: Vec<&str> = new_commits.iter().map(|c| c.summary.as_str()).collect();
    assert!(
        summaries.contains(&"after merge"),
        "descendant commit should be preserved: {summaries:?}"
    );

    // All files should exist.
    assert!(fixture.path().join("a.txt").exists());
    assert!(fixture.path().join("b.txt").exists());
    assert!(fixture.path().join("c.txt").exists());
    assert!(fixture.path().join("d.txt").exists());

    // Backup ref should exist.
    assert!(result.backup_ref.starts_with("refs/gunk/backup/main/"));
}

// ── Flatten: tree is byte-identical to merge result ────────────────

#[test]
fn flatten_preserves_merge_tree_exactly() {
    let (fixture, commits) = fixture_with_merge();
    let merge_commit = commits.iter().find(|c| c.is_merge()).unwrap();

    // Record the merge commit's tree before flatten.
    let original_merge_tree =
        fixture.git(["rev-parse", &format!("{}^{{tree}}", merge_commit.id.0)]);

    let git = Git::open(fixture.path()).unwrap();
    let spec = FlattenSpec {
        merge: merge_commit.id.clone(),
        mainline_parent: merge_commit.parents[0].clone(),
        message: "Flattened merge".to_string(),
    };

    execute_flatten(&git, "main", &spec).unwrap();

    // Find the flattened commit (parent of c4's new OID).
    let new_commits = git.walk_commits("main").unwrap();
    // The flattened commit should have the custom message.
    let flattened = new_commits
        .iter()
        .find(|c| c.summary == "Flattened merge")
        .expect("should find flattened commit");

    let new_tree = fixture.git(["rev-parse", &format!("{}^{{tree}}", flattened.id.0)]);
    assert_eq!(original_merge_tree.trim(), new_tree.trim());

    // The flattened commit should have exactly one parent.
    assert_eq!(flattened.parents.len(), 1);
}

// ── Flatten: merge with previously resolved conflicts ──────────────

#[test]
fn flatten_merge_with_resolved_conflicts() {
    let mut fixture = RepoFixture::new();
    fixture.commit("c1", "init", &[("shared.txt", "original")]);
    fixture.checkout_new_branch("feature");
    fixture.commit("c2", "feature change", &[("shared.txt", "feature version")]);
    fixture.checkout("main");
    fixture.commit("c3", "main change", &[("shared.txt", "main version")]);

    // Resolve conflict manually and create merge.
    let merge_output = fixture.git_raw(["merge", "--no-ff", "feature", "-m", "Merge feature"]);
    if !merge_output.status.success() {
        // Resolve conflict: use the feature version.
        std::fs::write(fixture.path().join("shared.txt"), "resolved content").unwrap();
        fixture.git(["add", "shared.txt"]);
        fixture.git(["commit", "-m", "Merge feature"]);
    }

    let git = Git::open(fixture.path()).unwrap();
    let commits = git.walk_commits("main").unwrap();
    let merge_commit = commits.iter().find(|c| c.is_merge()).unwrap();

    let merge_tree = fixture.git(["rev-parse", &format!("{}^{{tree}}", merge_commit.id.0)]);

    let spec = FlattenSpec {
        merge: merge_commit.id.clone(),
        mainline_parent: merge_commit.parents[0].clone(),
        message: merge_commit.summary.clone(),
    };

    execute_flatten(&git, "main", &spec).unwrap();

    // The tree at the tip should match the original merge tree (conflict resolution preserved).
    let new_tree = fixture.git(["rev-parse", "HEAD^{tree}"]);
    assert_eq!(
        merge_tree.trim(),
        new_tree.trim(),
        "resolved conflict state must be preserved after flatten"
    );
}

// ── Flatten: dirty tree refused ────────────────────────────────────

#[test]
fn flatten_refuses_dirty_tree() {
    let (fixture, commits) = fixture_with_merge();
    let merge_commit = commits.iter().find(|c| c.is_merge()).unwrap();

    // Dirty the working tree.
    std::fs::write(fixture.path().join("a.txt"), "dirty").unwrap();

    let git = Git::open(fixture.path()).unwrap();
    let spec = FlattenSpec {
        merge: merge_commit.id.clone(),
        mainline_parent: merge_commit.parents[0].clone(),
        message: merge_commit.summary.clone(),
    };

    let err = execute_flatten(&git, "main", &spec).unwrap_err();
    assert!(err.to_string().contains("dirty"));
}

// ── Flatten: backup ref is created ─────────────────────────────────

#[test]
fn flatten_creates_backup_ref() {
    let (fixture, commits) = fixture_with_merge();
    let merge_commit = commits.iter().find(|c| c.is_merge()).unwrap();
    let original_tip = fixture.rev_parse("main");

    let git = Git::open(fixture.path()).unwrap();
    let spec = FlattenSpec {
        merge: merge_commit.id.clone(),
        mainline_parent: merge_commit.parents[0].clone(),
        message: merge_commit.summary.clone(),
    };

    let result = execute_flatten(&git, "main", &spec).unwrap();

    // Backup ref should point at the original tip.
    let backup_oid = fixture.rev_parse(&result.backup_ref);
    assert_eq!(backup_oid, original_tip);
}

// ── Flatten: restore from backup ───────────────────────────────────

#[test]
fn flatten_restore_from_backup() {
    let (fixture, commits) = fixture_with_merge();
    let merge_commit = commits.iter().find(|c| c.is_merge()).unwrap();
    let original_tip = fixture.rev_parse("main");
    let original_count = commits.len();

    let git = Git::open(fixture.path()).unwrap();
    let spec = FlattenSpec {
        merge: merge_commit.id.clone(),
        mainline_parent: merge_commit.parents[0].clone(),
        message: merge_commit.summary.clone(),
    };

    let result = execute_flatten(&git, "main", &spec).unwrap();

    // Restore from backup.
    restore_backup(&git, "main", &result.backup_ref).unwrap();

    let restored_tip = fixture.rev_parse("main");
    assert_eq!(restored_tip, original_tip);

    // History should be fully restored (including the merge).
    let restored_commits = git.walk_commits("main").unwrap();
    assert_eq!(restored_commits.len(), original_count);

    let has_merge = restored_commits.iter().any(|c| c.is_merge());
    assert!(has_merge, "merge commit should be restored");
}

// ── Flatten: fast-forward merge ────────────────────────────────────

#[test]
fn flatten_ff_style_merge() {
    let mut fixture = RepoFixture::new();
    fixture.commit("c1", "first", &[("a.txt", "a")]);
    // Create a feature branch with one commit.
    fixture.checkout_new_branch("feature");
    fixture.commit("c2", "feature work", &[("b.txt", "b")]);
    fixture.checkout("main");
    // Force a merge commit even though it could fast-forward.
    fixture.merge("M", "feature", "Merge feature (ff)");

    let git = Git::open(fixture.path()).unwrap();
    let commits = git.walk_commits("main").unwrap();
    let merge_commit = commits.iter().find(|c| c.is_merge()).unwrap();

    let spec = FlattenSpec {
        merge: merge_commit.id.clone(),
        mainline_parent: merge_commit.parents[0].clone(),
        message: merge_commit.summary.clone(),
    };

    execute_flatten(&git, "main", &spec).unwrap();

    let new_commits = git.walk_commits("main").unwrap();
    for c in &new_commits {
        assert!(c.parents.len() <= 1, "history should be linear");
    }

    // All files present.
    assert!(fixture.path().join("a.txt").exists());
    assert!(fixture.path().join("b.txt").exists());
}

// ── Flatten via execute_plan ───────────────────────────────────────

#[test]
fn execute_plan_handles_flatten() {
    let (fixture, commits) = fixture_with_merge();
    let merge_commit = commits.iter().find(|c| c.is_merge()).unwrap();

    let git = Git::open(fixture.path()).unwrap();

    let operations = vec![Operation::FlattenMerge {
        merge: merge_commit.id.clone(),
    }];
    let exec_plan = plan(&commits, &operations).unwrap();

    let result = execute_plan(&git, "main", &exec_plan).unwrap();

    let new_commits = git.walk_commits("main").unwrap();
    for c in &new_commits {
        assert!(c.parents.len() <= 1, "history should be linear");
    }
    assert!(result.backup_ref.starts_with("refs/gunk/backup/main/"));
}

// ── Flatten + squash composite ─────────────────────────────────────

#[test]
fn flatten_then_squash_composite() {
    let mut fixture = RepoFixture::new();
    fixture.commit("c1", "first", &[("a.txt", "a")]);
    fixture.commit("c2", "second", &[("b.txt", "b")]);
    fixture.checkout_new_branch("feature");
    fixture.commit("c3", "feature work", &[("c.txt", "c")]);
    fixture.checkout("main");
    fixture.merge("M", "feature", "Merge feature");
    fixture.commit("c4", "after merge", &[("d.txt", "d")]);

    let git = Git::open(fixture.path()).unwrap();
    let commits = git.walk_commits("main").unwrap();
    let merge_commit = commits.iter().find(|c| c.is_merge()).unwrap();

    // Step 1: flatten.
    let flatten_spec = FlattenSpec {
        merge: merge_commit.id.clone(),
        mainline_parent: merge_commit.parents[0].clone(),
        message: merge_commit.summary.clone(),
    };
    execute_flatten(&git, "main", &flatten_spec).unwrap();

    // Step 2: re-snapshot and squash.
    let new_commits = git.walk_commits("main").unwrap();
    assert!(
        new_commits.iter().all(|c| c.parents.len() <= 1),
        "should be linear after flatten"
    );

    // Squash the last two commits.
    let operations = vec![Operation::Squash {
        keep: new_commits[1].id.clone(),
        absorb: vec![new_commits[0].id.clone()],
    }];
    let exec_plan = plan(&new_commits, &operations).unwrap();
    execute_plan(&git, "main", &exec_plan).unwrap();

    let final_commits = git.walk_commits("main").unwrap();
    assert!(
        final_commits.len() < new_commits.len(),
        "squash should reduce commit count"
    );

    // All files should still exist.
    assert!(fixture.path().join("a.txt").exists());
    assert!(fixture.path().join("b.txt").exists());
    assert!(fixture.path().join("c.txt").exists());
    assert!(fixture.path().join("d.txt").exists());
}

// ── Flatten: message is preserved ──────────────────────────────────

#[test]
fn flatten_preserves_custom_message() {
    let mut fixture = RepoFixture::new();
    fixture.commit("c1", "first", &[("a.txt", "a")]);
    fixture.checkout_new_branch("feature");
    fixture.commit("c2", "feature work", &[("b.txt", "b")]);
    fixture.checkout("main");
    fixture.merge("M", "feature", "My custom merge message");

    let git = Git::open(fixture.path()).unwrap();
    let commits = git.walk_commits("main").unwrap();
    let merge_commit = commits.iter().find(|c| c.is_merge()).unwrap();

    let spec = FlattenSpec {
        merge: merge_commit.id.clone(),
        mainline_parent: merge_commit.parents[0].clone(),
        message: "Flattened: feature branch".to_string(),
    };

    execute_flatten(&git, "main", &spec).unwrap();

    let new_commits = git.walk_commits("main").unwrap();
    let has_msg = new_commits
        .iter()
        .any(|c| c.summary == "Flattened: feature branch");
    assert!(has_msg, "custom message should appear in flattened commit");
}

// ── Flatten: descendant rebase conflict is reported cleanly ────────

#[test]
fn flatten_conflict_during_descendant_rebase_leaves_branch_untouched() {
    let mut fixture = RepoFixture::new();
    // c1: base with shared file.
    fixture.commit("c1", "init", &[("shared.txt", "base")]);

    // feature branch modifies shared.txt.
    fixture.checkout_new_branch("feature");
    fixture.commit("c2", "feature change", &[("shared.txt", "feature version")]);
    fixture.checkout("main");

    // main also modifies shared.txt (will be resolved in merge).
    fixture.commit("c3", "main change", &[("shared.txt", "main version")]);

    // Merge with manual conflict resolution.
    let merge_output = fixture.git_raw(["merge", "--no-ff", "feature", "-m", "Merge feature"]);
    if !merge_output.status.success() {
        std::fs::write(fixture.path().join("shared.txt"), "resolved").unwrap();
        fixture.git(["add", "shared.txt"]);
        fixture.git(["commit", "-m", "Merge feature"]);
    }

    // Post-merge commit that also modifies shared.txt in a way that will
    // conflict when rebased onto the flattened commit (which has the merge's
    // tree, not c3's tree).
    fixture.commit(
        "c4",
        "post-merge edit",
        &[("shared.txt", "post-merge content")],
    );

    let original_tip = fixture.rev_parse("main");
    let git = Git::open(fixture.path()).unwrap();
    let commits = git.walk_commits("main").unwrap();
    let merge_commit = commits.iter().find(|c| c.is_merge()).unwrap();

    let spec = FlattenSpec {
        merge: merge_commit.id.clone(),
        mainline_parent: merge_commit.parents[0].clone(),
        message: merge_commit.summary.clone(),
    };

    // This may succeed (no conflict because tree is identical) or fail.
    // The important thing is the branch is either correctly updated or untouched.
    let result = execute_flatten(&git, "main", &spec);

    match result {
        Ok(_) => {
            // If it succeeds, history should be linear.
            let new_commits = git.walk_commits("main").unwrap();
            for c in &new_commits {
                assert!(c.parents.len() <= 1, "expected linear history");
            }
        }
        Err(e) => {
            // If it fails, real branch must be untouched.
            let current_tip = fixture.rev_parse("main");
            assert_eq!(
                current_tip, original_tip,
                "on failure, real branch should be untouched: {e}"
            );
        }
    }
}

// ── Flatten: composite remap is safe when descendants contain a merge ──
//
// Regression test. If a flattened merge has another merge above it, rebase
// reorders the commits in between, so the flatten step can't reliably tell
// which old commit became which new one. Rather than guess (and risk rewriting
// the wrong commit in a later phase), the composite must fail and roll the
// branch back untouched.
#[test]
fn composite_flatten_with_descendant_merge_fails_loudly_and_leaves_branch_untouched() {
    let mut fixture = RepoFixture::new();
    fixture.commit("c1", "first", &[("a.txt", "a")]);
    fixture.commit("c2", "second", &[("b.txt", "b")]);

    // First feature branch -> the merge M that we will flatten.
    fixture.checkout_new_branch("feature1");
    fixture.commit("c3", "feature1 work", &[("c.txt", "c")]);
    fixture.checkout("main");
    fixture.merge("M", "feature1", "Merge feature1");

    // A second feature branch merged *above* M, so M has a descendant merge.
    fixture.checkout_new_branch("feature2");
    fixture.commit("s1", "feature2 work", &[("e.txt", "e")]);
    fixture.checkout("main");
    fixture.commit("m1", "main work", &[("f.txt", "f")]);
    fixture.merge("MERGE2", "feature2", "Merge feature2");
    fixture.commit("top", "top commit", &[("g.txt", "g")]);

    let original_tip = fixture.rev_parse("main");
    let git = Git::open(fixture.path()).unwrap();
    let commits = git.walk_commits("main").unwrap();

    // The merge to flatten is the older one (M), not the intervening MERGE2.
    let merge_m = commits
        .iter()
        .find(|c| c.is_merge() && c.summary == "Merge feature1")
        .unwrap();
    // A descendant of M that the rebase phase will try to reword.
    let top = commits.iter().find(|c| c.summary == "top commit").unwrap();

    // Composite plan: flatten M, then reword a descendant of M.
    let ops = vec![
        Operation::FlattenMerge {
            merge: merge_m.id.clone(),
        },
        Operation::Reword {
            target: top.id.clone(),
            summary: "reworded top".to_string(),
            body: String::new(),
        },
    ];
    let exec_plan = plan(&commits, &ops).expect("plan should build a composite");
    assert!(
        matches!(exec_plan, ExecutionPlan::Composite(_)),
        "expected a composite (flatten + rebase) plan"
    );

    let result = execute_plan(&git, "main", &exec_plan);

    // The composite must refuse rather than silently rewrite the wrong commit.
    assert!(
        result.is_err(),
        "composite flatten+reword over a descendant merge must fail, not silently corrupt"
    );

    // And the real branch must be restored to its original tip.
    let current_tip = fixture.rev_parse("main");
    assert_eq!(
        current_tip, original_tip,
        "branch must be rolled back to its original tip on failure"
    );
    // History is intact: both merges still present.
    let restored = git.walk_commits("main").unwrap();
    assert_eq!(
        restored.iter().filter(|c| c.is_merge()).count(),
        2,
        "both merge commits should survive the rolled-back composite"
    );
}

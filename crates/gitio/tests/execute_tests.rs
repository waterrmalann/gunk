use gunk_core::{CommitId, ExecutionPlan, Operation, RebaseTodo, RebaseTodoLine, plan};
use gunk_gitio::{
    Git, check_clean, create_backup_ref, execute_rebase, format_rebase_todo, list_backup_refs,
    restore_backup, stash_pop, stash_push,
};
use gunk_testkit::RepoFixture;

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
    // The squashed commit should contain both messages.
    // (git's default squash behavior combines messages.)
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

    let _ = execute_rebase(&git, "main", &todo); // may fail

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

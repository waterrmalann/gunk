use gunk_core::ChangeStatus;
use gunk_gitio::Git;
use gunk_testkit::RepoFixture;

// ── open / basic ───────────────────────────────────────────────────

#[test]
fn open_valid_repo() {
    let fixture = RepoFixture::new();
    let git = Git::open(fixture.path());
    assert!(git.is_ok());
}

#[test]
fn open_invalid_path_fails() {
    let result = Git::open("/tmp/not_a_repo_xyz_999");
    assert!(result.is_err());
}

#[test]
fn run_rev_parse_head() {
    let mut fixture = RepoFixture::new();
    fixture.commit("c1", "initial", &[("f.txt", "hello")]);

    let git = Git::open(fixture.path()).unwrap();
    let output = git.run(["rev-parse", "HEAD"]).unwrap();
    let oid = output.stdout.trim();

    assert_eq!(oid.len(), 40);
    assert_eq!(oid, fixture.oid("c1"));
}

#[test]
fn run_log_format() {
    let mut fixture = RepoFixture::new();
    fixture.commit("c1", "first commit", &[("a.txt", "a")]);
    fixture.commit("c2", "second commit", &[("b.txt", "b")]);

    let git = Git::open(fixture.path()).unwrap();
    let output = git.run(["log", "--format=%H %s"]).unwrap();

    let lines: Vec<&str> = output.stdout.trim().lines().collect();
    assert_eq!(lines.len(), 2);
    assert!(lines[0].ends_with("second commit"));
    assert!(lines[1].ends_with("first commit"));
}

#[test]
fn run_failing_command_returns_error() {
    let mut fixture = RepoFixture::new();
    fixture.commit("c1", "init", &[("f.txt", "x")]);

    let git = Git::open(fixture.path()).unwrap();
    let result = git.run(["rev-parse", "nonexistent_ref_xyz"]);
    assert!(result.is_err());
}

#[test]
fn run_nul_separated_parses_fields() {
    let mut fixture = RepoFixture::new();
    fixture.commit("c1", "init", &[("a.txt", "a")]);

    let git = Git::open(fixture.path()).unwrap();
    // Use for-each-ref with NUL separator to list branches
    let fields = git
        .run_nul_separated([
            "for-each-ref",
            "--format=%(refname:short)%00%(objectname)",
            "refs/heads",
        ])
        .unwrap();

    // Should have at least 2 fields: branch name and oid
    assert!(fields.len() >= 2);
    assert_eq!(fields[0], "main");
    assert_eq!(fields[1], fixture.oid("c1"));
}

#[test]
fn repo_path_returns_correct_path() {
    let fixture = RepoFixture::new();
    let git = Git::open(fixture.path()).unwrap();
    assert_eq!(git.repo_path(), fixture.path());
}

// ── list_branches ──────────────────────────────────────────────────

#[test]
fn list_branches_single_branch() {
    let mut fixture = RepoFixture::new();
    fixture.commit("c1", "init", &[("f.txt", "x")]);

    let git = Git::open(fixture.path()).unwrap();
    let branches = git.list_branches().unwrap();

    assert_eq!(branches.len(), 1);
    assert_eq!(branches[0].name, "main");
    assert_eq!(branches[0].oid, fixture.oid("c1"));
    assert!(branches[0].upstream.is_none());
}

#[test]
fn list_branches_multiple() {
    let mut fixture = RepoFixture::new();
    fixture.commit("c1", "init", &[("f.txt", "x")]);
    fixture.checkout_new_branch("feature");
    fixture.commit("c2", "feature work", &[("g.txt", "y")]);
    fixture.checkout_new_branch("bugfix");
    fixture.commit("c3", "bugfix work", &[("h.txt", "z")]);

    let git = Git::open(fixture.path()).unwrap();
    let mut branches = git.list_branches().unwrap();
    branches.sort_by(|a, b| a.name.cmp(&b.name));

    assert_eq!(branches.len(), 3);
    assert_eq!(branches[0].name, "bugfix");
    assert_eq!(branches[1].name, "feature");
    assert_eq!(branches[2].name, "main");
}

// ── walk_commits ───────────────────────────────────────────────────

#[test]
fn walk_single_commit() {
    let mut fixture = RepoFixture::new();
    fixture.commit("c1", "initial commit", &[("f.txt", "hello")]);

    let git = Git::open(fixture.path()).unwrap();
    let commits = git.walk_commits("main").unwrap();

    assert_eq!(commits.len(), 1);
    assert_eq!(commits[0].id.0, fixture.oid("c1"));
    assert_eq!(commits[0].summary, "initial commit");
    assert!(commits[0].parents.is_empty()); // root commit
    assert_eq!(commits[0].author.name, "Test Author");
    assert_eq!(commits[0].author.email, "test@example.com");
    assert_eq!(commits[0].committer.name, "Test Committer");
    assert_eq!(commits[0].committer.email, "committer@example.com");
}

#[test]
fn walk_linear_history() {
    let mut fixture = RepoFixture::new();
    fixture.commit("c1", "first", &[("a.txt", "a")]);
    fixture.commit("c2", "second", &[("b.txt", "b")]);
    fixture.commit("c3", "third", &[("c.txt", "c")]);

    let git = Git::open(fixture.path()).unwrap();
    let commits = git.walk_commits("main").unwrap();

    // Newest first
    assert_eq!(commits.len(), 3);
    assert_eq!(commits[0].summary, "third");
    assert_eq!(commits[1].summary, "second");
    assert_eq!(commits[2].summary, "first");

    // Parent chain
    assert_eq!(commits[0].parents, vec![commits[1].id.clone()]);
    assert_eq!(commits[1].parents, vec![commits[2].id.clone()]);
    assert!(commits[2].parents.is_empty());
}

#[test]
fn walk_includes_merge_commits() {
    let mut fixture = RepoFixture::new();
    fixture.commit("c1", "initial", &[("a.txt", "a")]);
    fixture.checkout_new_branch("feature");
    fixture.commit("c2", "feature work", &[("b.txt", "b")]);
    fixture.checkout("main");
    fixture.merge("m1", "feature", "Merge feature");

    let git = Git::open(fixture.path()).unwrap();
    let commits = git.walk_commits("main").unwrap();

    // Should see 3 commits: merge, feature, initial
    assert_eq!(commits.len(), 3);
    assert_eq!(commits[0].summary, "Merge feature");
    assert!(commits[0].is_merge());
    assert_eq!(commits[0].parents.len(), 2);
}

#[test]
fn walk_unicode_messages() {
    let mut fixture = RepoFixture::new();
    fixture.commit("c1", "初めてのコミット 🎉", &[("f.txt", "content")]);

    let git = Git::open(fixture.path()).unwrap();
    let commits = git.walk_commits("main").unwrap();

    assert_eq!(commits.len(), 1);
    assert_eq!(commits[0].summary, "初めてのコミット 🎉");
}

#[test]
fn walk_empty_body() {
    let mut fixture = RepoFixture::new();
    fixture.commit("c1", "no body", &[("f.txt", "x")]);

    let git = Git::open(fixture.path()).unwrap();
    let commits = git.walk_commits("main").unwrap();

    assert_eq!(commits.len(), 1);
    assert_eq!(commits[0].summary, "no body");
    assert!(commits[0].body.is_empty());
}

#[test]
fn walk_multiline_body_preserves_paragraphs() {
    let mut fixture = RepoFixture::new();
    fixture.commit(
        "c1",
        "Subject line\n\nFirst body paragraph.\n\nSecond paragraph with detail.",
        &[("f.txt", "x")],
    );

    let git = Git::open(fixture.path()).unwrap();
    let commits = git.walk_commits("main").unwrap();

    assert_eq!(commits.len(), 1);
    // The summary must stop at the first line; the body must retain the
    // blank-line-separated paragraphs intact (NUL/record parsing must not
    // truncate the multi-line body).
    assert_eq!(commits[0].summary, "Subject line");
    assert!(commits[0].body.contains("First body paragraph."));
    assert!(commits[0].body.contains("Second paragraph with detail."));
    assert!(
        commits[0].body.contains("\n\n"),
        "blank line between paragraphs should survive, got: {:?}",
        commits[0].body
    );
}

#[test]
fn walk_custom_author() {
    let mut fixture = RepoFixture::new();
    fixture.commit_by("c1", "custom", &[("f.txt", "x")], "Alice", "alice@dev.io");

    let git = Git::open(fixture.path()).unwrap();
    let commits = git.walk_commits("main").unwrap();

    assert_eq!(commits[0].author.name, "Alice");
    assert_eq!(commits[0].author.email, "alice@dev.io");
}

// ── changed_paths ──────────────────────────────────────────────────

#[test]
fn changed_paths_root_commit() {
    let mut fixture = RepoFixture::new();
    fixture.commit("c1", "root", &[("a.txt", "a"), ("b.txt", "b")]);

    let git = Git::open(fixture.path()).unwrap();
    let paths = git.changed_paths(fixture.oid("c1")).unwrap();

    assert_eq!(paths.len(), 2);
    let names: Vec<&str> = paths.iter().map(|p| p.path.as_str()).collect();
    assert!(names.contains(&"a.txt"));
    assert!(names.contains(&"b.txt"));
    assert!(paths.iter().all(|p| p.status == ChangeStatus::Added));
}

#[test]
fn changed_paths_modified_file() {
    let mut fixture = RepoFixture::new();
    fixture.commit("c1", "add", &[("f.txt", "v1")]);
    fixture.commit("c2", "modify", &[("f.txt", "v2")]);

    let git = Git::open(fixture.path()).unwrap();
    let paths = git.changed_paths(fixture.oid("c2")).unwrap();

    assert_eq!(paths.len(), 1);
    assert_eq!(paths[0].path, "f.txt");
    assert_eq!(paths[0].status, ChangeStatus::Modified);
}

#[test]
fn changed_paths_deleted_file() {
    let mut fixture = RepoFixture::new();
    fixture.commit("c1", "add", &[("f.txt", "content")]);
    // Delete the file and commit
    std::fs::remove_file(fixture.path().join("f.txt")).unwrap();
    fixture.git(["add", "-A"]);
    fixture.commit("c2", "delete", &[]);

    let git = Git::open(fixture.path()).unwrap();
    let paths = git.changed_paths(fixture.oid("c2")).unwrap();

    assert_eq!(paths.len(), 1);
    assert_eq!(paths[0].path, "f.txt");
    assert_eq!(paths[0].status, ChangeStatus::Deleted);
}

#[test]
fn changed_paths_subdirectory() {
    let mut fixture = RepoFixture::new();
    fixture.commit("c1", "nested", &[("src/main.rs", "fn main() {}")]);

    let git = Git::open(fixture.path()).unwrap();
    let paths = git.changed_paths(fixture.oid("c1")).unwrap();

    assert_eq!(paths.len(), 1);
    assert_eq!(paths[0].path, "src/main.rs");
}

// ── show_diff ──────────────────────────────────────────────────────

#[test]
fn show_diff_returns_patch() {
    let mut fixture = RepoFixture::new();
    fixture.commit("c1", "add", &[("f.txt", "hello\n")]);
    fixture.commit("c2", "modify", &[("f.txt", "hello\nworld\n")]);

    let git = Git::open(fixture.path()).unwrap();
    let diff = git.show_diff(fixture.oid("c2")).unwrap();

    assert!(diff.contains("+world"));
    assert!(diff.contains("f.txt"));
}

#[test]
fn show_diff_root_commit() {
    let mut fixture = RepoFixture::new();
    fixture.commit("c1", "root", &[("f.txt", "content\n")]);

    let git = Git::open(fixture.path()).unwrap();
    let diff = git.show_diff(fixture.oid("c1")).unwrap();

    assert!(diff.contains("+content"));
}

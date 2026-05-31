use gunk_gitio::Git;
use gunk_testkit::RepoFixture;

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
    let output = git
        .run(["log", "--format=%H %s"])
        .unwrap();

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

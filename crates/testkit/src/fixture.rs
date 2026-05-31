use std::collections::HashMap;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use tempfile::TempDir;

/// A throwaway Git repository for testing.
///
/// Creates a real repo in a tempdir, scripted via a builder API.
/// Pinned author/committer dates and identities for deterministic oids.
pub struct RepoFixture {
    _tempdir: TempDir,
    path: PathBuf,
    /// Map of label → commit oid for later reference.
    commits: HashMap<String, String>,
    /// Current branch name.
    current_branch: String,
    /// Counter for deterministic timestamps.
    time_counter: u64,
}

/// The default test identity.
const DEFAULT_AUTHOR_NAME: &str = "Test Author";
const DEFAULT_AUTHOR_EMAIL: &str = "test@example.com";
const DEFAULT_COMMITTER_NAME: &str = "Test Committer";
const DEFAULT_COMMITTER_EMAIL: &str = "committer@example.com";
/// Base timestamp: 2024-01-01T00:00:00+00:00
const BASE_TIMESTAMP: u64 = 1_704_067_200;

impl Default for RepoFixture {
    fn default() -> Self {
        Self::new()
    }
}

impl RepoFixture {
    /// Create a new empty repo in a tempdir with an initial commit.
    pub fn new() -> Self {
        let tempdir = TempDir::new().expect("failed to create tempdir");
        let path = tempdir.path().to_path_buf();

        let fixture = Self {
            _tempdir: tempdir,
            path,
            commits: HashMap::new(),
            current_branch: "main".to_string(),
            time_counter: 0,
        };

        fixture.git(["init", "-b", "main"]);
        fixture.git(["config", "user.name", DEFAULT_AUTHOR_NAME]);
        fixture.git(["config", "user.email", DEFAULT_AUTHOR_EMAIL]);
        // Ensure consistent line endings across platforms
        fixture.git(["config", "core.autocrlf", "false"]);

        fixture
    }

    /// Returns the path to the repository root.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Returns the oid for a labeled commit.
    pub fn oid(&self, label: &str) -> &str {
        self.commits
            .get(label)
            .unwrap_or_else(|| panic!("no commit labeled '{label}'"))
    }

    /// Returns all stored commit labels and their oids.
    pub fn commits(&self) -> &HashMap<String, String> {
        &self.commits
    }

    /// Returns the current branch name.
    pub fn current_branch(&self) -> &str {
        &self.current_branch
    }

    /// Create a commit with given files. Files are `(relative_path, content)` pairs.
    /// The commit is labeled so it can be referenced later.
    pub fn commit(&mut self, label: &str, message: &str, files: &[(&str, &str)]) -> &str {
        self.commit_by(
            label,
            message,
            files,
            DEFAULT_AUTHOR_NAME,
            DEFAULT_AUTHOR_EMAIL,
        )
    }

    /// Create a commit with a specific author.
    pub fn commit_by(
        &mut self,
        label: &str,
        message: &str,
        files: &[(&str, &str)],
        author_name: &str,
        author_email: &str,
    ) -> &str {
        for (path, content) in files {
            let full_path = self.path.join(path);
            if let Some(parent) = full_path.parent() {
                std::fs::create_dir_all(parent).expect("failed to create parent dirs");
            }
            std::fs::write(&full_path, content).expect("failed to write file");
            self.git(["add", path]);
        }

        let timestamp = self.next_timestamp();
        let date_str = format!("{timestamp} +0000");

        let output = Command::new("git")
            .args(["commit", "--allow-empty", "-m", message])
            .current_dir(&self.path)
            .env("GIT_AUTHOR_NAME", author_name)
            .env("GIT_AUTHOR_EMAIL", author_email)
            .env("GIT_AUTHOR_DATE", &date_str)
            .env("GIT_COMMITTER_NAME", DEFAULT_COMMITTER_NAME)
            .env("GIT_COMMITTER_EMAIL", DEFAULT_COMMITTER_EMAIL)
            .env("GIT_COMMITTER_DATE", &date_str)
            .output()
            .expect("failed to run git commit");
        assert_output_success(&output, "commit");

        let oid = self.rev_parse("HEAD");
        self.commits.insert(label.to_string(), oid);
        self.commits.get(label).unwrap()
    }

    /// Create a new branch at the current HEAD.
    pub fn branch(&mut self, name: &str) {
        self.git(["branch", name]);
    }

    /// Checkout an existing branch.
    pub fn checkout(&mut self, name: &str) {
        self.git(["checkout", name]);
        self.current_branch = name.to_string();
    }

    /// Create and checkout a new branch.
    pub fn checkout_new_branch(&mut self, name: &str) {
        self.git(["checkout", "-b", name]);
        self.current_branch = name.to_string();
    }

    /// Merge another branch into the current branch (creates a merge commit).
    /// Returns the merge commit oid.
    pub fn merge(&mut self, label: &str, branch: &str, message: &str) -> &str {
        let timestamp = self.next_timestamp();
        let date_str = format!("{timestamp} +0000");

        let output = Command::new("git")
            .args(["merge", "--no-ff", branch, "-m", message])
            .current_dir(&self.path)
            .env("GIT_AUTHOR_NAME", DEFAULT_AUTHOR_NAME)
            .env("GIT_AUTHOR_EMAIL", DEFAULT_AUTHOR_EMAIL)
            .env("GIT_AUTHOR_DATE", &date_str)
            .env("GIT_COMMITTER_NAME", DEFAULT_COMMITTER_NAME)
            .env("GIT_COMMITTER_EMAIL", DEFAULT_COMMITTER_EMAIL)
            .env("GIT_COMMITTER_DATE", &date_str)
            .output()
            .expect("failed to run git merge");
        assert_output_success(&output, "merge");

        let oid = self.rev_parse("HEAD");
        self.commits.insert(label.to_string(), oid);
        self.commits.get(label).unwrap()
    }

    /// Get the commit log as a list of `(oid, summary)` pairs (newest first).
    pub fn log(&self) -> Vec<(String, String)> {
        let output = self.git(["log", "--format=%H %s"]);
        output
            .lines()
            .filter(|l| !l.is_empty())
            .map(|line| {
                let (oid, summary) = line.split_once(' ').expect("malformed log line");
                (oid.to_string(), summary.to_string())
            })
            .collect()
    }

    /// Get the parent oids of a commit.
    pub fn parents(&self, rev: &str) -> Vec<String> {
        let output = self.git(["rev-parse", &format!("{rev}^@")]);
        output
            .lines()
            .filter(|l| !l.is_empty())
            .map(|s| s.to_string())
            .collect()
    }

    /// Get the commit message (subject + body) for a revision.
    pub fn message(&self, rev: &str) -> String {
        self.git(["log", "-1", "--format=%B", rev])
            .trim()
            .to_string()
    }

    /// Get the author name for a revision.
    pub fn author_name(&self, rev: &str) -> String {
        self.git(["log", "-1", "--format=%an", rev])
            .trim()
            .to_string()
    }

    /// Get the author email for a revision.
    pub fn author_email(&self, rev: &str) -> String {
        self.git(["log", "-1", "--format=%ae", rev])
            .trim()
            .to_string()
    }

    /// Resolve a revision to its full oid.
    pub fn rev_parse(&self, rev: &str) -> String {
        self.git(["rev-parse", rev]).trim().to_string()
    }

    /// Check if the working tree is clean.
    pub fn is_clean(&self) -> bool {
        self.git(["status", "--porcelain"]).trim().is_empty()
    }

    /// Run an arbitrary git command and return stdout.
    pub fn git<I, S>(&self, args: I) -> String
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let args: Vec<_> = args.into_iter().collect();
        let output = Command::new("git")
            .args(&args)
            .current_dir(&self.path)
            .output()
            .expect("failed to run git");
        assert_output_success(&output, "git command");
        String::from_utf8(output.stdout).expect("non-utf8 git output")
    }

    /// Run a git command that may fail, returning the Output.
    pub fn git_raw<I, S>(&self, args: I) -> Output
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let args: Vec<_> = args.into_iter().collect();
        Command::new("git")
            .args(&args)
            .current_dir(&self.path)
            .output()
            .expect("failed to run git")
    }

    fn next_timestamp(&mut self) -> u64 {
        let ts = BASE_TIMESTAMP + self.time_counter;
        self.time_counter += 1;
        ts
    }
}

fn assert_output_success(output: &Output, context: &str) {
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        panic!(
            "{context} failed (status {:?}):\nstdout: {stdout}\nstderr: {stderr}",
            output.status.code()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_fixture_creates_valid_repo() {
        let fixture = RepoFixture::new();
        assert!(fixture.path().join(".git").is_dir());
    }

    #[test]
    fn commit_creates_labeled_oid() {
        let mut fixture = RepoFixture::new();
        fixture.commit("c1", "first commit", &[("file.txt", "hello")]);

        let oid = fixture.oid("c1");
        assert_eq!(oid.len(), 40, "oid should be 40 hex chars");
        assert!(oid.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn commits_appear_in_log() {
        let mut fixture = RepoFixture::new();
        fixture.commit("c1", "first", &[("a.txt", "a")]);
        fixture.commit("c2", "second", &[("b.txt", "b")]);

        let log = fixture.log();
        assert_eq!(log.len(), 2);
        assert_eq!(log[0].1, "second"); // newest first
        assert_eq!(log[1].1, "first");
    }

    #[test]
    fn commit_by_sets_custom_author() {
        let mut fixture = RepoFixture::new();
        fixture.commit_by(
            "c1",
            "custom author commit",
            &[("file.txt", "content")],
            "Custom Name",
            "custom@example.com",
        );

        let name = fixture.author_name("HEAD");
        let email = fixture.author_email("HEAD");
        assert_eq!(name, "Custom Name");
        assert_eq!(email, "custom@example.com");
    }

    #[test]
    fn branch_and_checkout_work() {
        let mut fixture = RepoFixture::new();
        fixture.commit("c1", "initial", &[("f.txt", "x")]);

        fixture.checkout_new_branch("feature");
        assert_eq!(fixture.current_branch(), "feature");

        fixture.commit("c2", "on feature", &[("g.txt", "y")]);

        fixture.checkout("main");
        assert_eq!(fixture.current_branch(), "main");

        // Feature commit should not be on main
        let log = fixture.log();
        assert_eq!(log.len(), 1);
        assert_eq!(log[0].1, "initial");
    }

    #[test]
    fn merge_creates_commit_with_two_parents() {
        let mut fixture = RepoFixture::new();
        fixture.commit("c1", "initial", &[("a.txt", "a")]);

        fixture.checkout_new_branch("feature");
        fixture.commit("c2", "feature work", &[("b.txt", "b")]);

        fixture.checkout("main");
        fixture.merge("m1", "feature", "Merge feature");

        let parents = fixture.parents("HEAD");
        assert_eq!(parents.len(), 2, "merge should have 2 parents");

        let log = fixture.log();
        assert_eq!(log[0].1, "Merge feature");
    }

    #[test]
    fn rev_parse_matches_oid() {
        let mut fixture = RepoFixture::new();
        fixture.commit("c1", "first", &[("f.txt", "x")]);

        let head_oid = fixture.rev_parse("HEAD");
        assert_eq!(head_oid, *fixture.oid("c1"));
    }

    #[test]
    fn message_returns_full_commit_message() {
        let mut fixture = RepoFixture::new();
        fixture.commit("c1", "subject line", &[("f.txt", "x")]);

        let msg = fixture.message("HEAD");
        assert_eq!(msg, "subject line");
    }

    #[test]
    fn unicode_messages_work() {
        let mut fixture = RepoFixture::new();
        fixture.commit("c1", "初めてのコミット 🎉", &[("f.txt", "content")]);

        let msg = fixture.message("HEAD");
        assert_eq!(msg, "初めてのコミット 🎉");
    }

    #[test]
    fn empty_body_commit() {
        let mut fixture = RepoFixture::new();
        fixture.commit("c1", "no body", &[]);

        let msg = fixture.message("HEAD");
        assert_eq!(msg, "no body");
    }

    #[test]
    fn is_clean_on_fresh_repo() {
        let mut fixture = RepoFixture::new();
        fixture.commit("c1", "initial", &[("f.txt", "x")]);
        assert!(fixture.is_clean());
    }

    #[test]
    fn is_not_clean_with_untracked_file() {
        let mut fixture = RepoFixture::new();
        fixture.commit("c1", "initial", &[("f.txt", "x")]);
        std::fs::write(fixture.path().join("untracked.txt"), "dirty").unwrap();
        assert!(!fixture.is_clean());
    }

    #[test]
    fn subdirectory_files() {
        let mut fixture = RepoFixture::new();
        fixture.commit(
            "c1",
            "nested files",
            &[("src/main.rs", "fn main() {}"), ("src/lib.rs", "// lib")],
        );

        assert!(fixture.path().join("src/main.rs").exists());
        assert!(fixture.path().join("src/lib.rs").exists());
    }
}

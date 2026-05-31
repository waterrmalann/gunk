use std::collections::BTreeSet;

use crate::model::Commit;

/// A search result with the matching commit index and which field matched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchHit {
    pub index: usize,
    pub fields: Vec<SearchField>,
}

/// Which field of a commit matched the search query.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchField {
    Summary,
    Body,
    AuthorName,
    AuthorEmail,
    Path,
}

/// Search across commits by message, author, or changed file paths.
///
/// The search is case-insensitive substring matching. Returns indices of
/// matching commits along with which fields matched.
pub fn search_commits(commits: &[Commit], query: &str) -> Vec<SearchHit> {
    if query.is_empty() {
        return Vec::new();
    }
    let query_lower = query.to_lowercase();

    commits
        .iter()
        .enumerate()
        .filter_map(|(i, commit)| {
            let mut fields = Vec::new();

            if commit.summary.to_lowercase().contains(&query_lower) {
                fields.push(SearchField::Summary);
            }
            if commit.body.to_lowercase().contains(&query_lower) {
                fields.push(SearchField::Body);
            }
            if commit.author.name.to_lowercase().contains(&query_lower) {
                fields.push(SearchField::AuthorName);
            }
            if commit.author.email.to_lowercase().contains(&query_lower) {
                fields.push(SearchField::AuthorEmail);
            }
            if commit
                .changed_paths
                .iter()
                .any(|p| p.path.to_lowercase().contains(&query_lower))
            {
                fields.push(SearchField::Path);
            }

            if fields.is_empty() {
                None
            } else {
                Some(SearchHit { index: i, fields })
            }
        })
        .collect()
}

/// Convenience: extract just the indices from search hits as a `BTreeSet`
/// (ready to feed into `SelectionMsg::SelectSet`).
pub fn search_hit_indices(hits: &[SearchHit]) -> BTreeSet<usize> {
    hits.iter().map(|h| h.index).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::*;
    use time::OffsetDateTime;

    fn make_identity(name: &str, email: &str) -> Identity {
        Identity {
            name: name.to_string(),
            email: email.to_string(),
            time: OffsetDateTime::UNIX_EPOCH,
        }
    }

    fn make_commit(
        idx: usize,
        summary: &str,
        body: &str,
        author_name: &str,
        author_email: &str,
        paths: &[&str],
    ) -> Commit {
        Commit {
            id: CommitId(format!("{idx:040x}")),
            parents: vec![],
            author: make_identity(author_name, author_email),
            committer: make_identity(author_name, author_email),
            summary: summary.to_string(),
            body: body.to_string(),
            changed_paths: paths
                .iter()
                .map(|p| PathChange {
                    status: ChangeStatus::Modified,
                    path: p.to_string(),
                })
                .collect(),
        }
    }

    fn sample_commits() -> Vec<Commit> {
        vec![
            make_commit(
                0,
                "Initial commit",
                "",
                "Alice",
                "alice@example.com",
                &["README.md"],
            ),
            make_commit(
                1,
                "Add authentication module",
                "Implements JWT-based auth",
                "Bob",
                "bob@example.com",
                &["src/auth.rs", "src/main.rs"],
            ),
            make_commit(
                2,
                "Fix typo in README",
                "",
                "Alice",
                "alice@example.com",
                &["README.md"],
            ),
            make_commit(
                3,
                "Add database migration",
                "PostgreSQL schema v2",
                "Charlie",
                "charlie@corp.dev",
                &["migrations/002.sql", "src/db.rs"],
            ),
            make_commit(
                4,
                "Update dependencies",
                "",
                "Bob",
                "bob@example.com",
                &["Cargo.toml", "Cargo.lock"],
            ),
        ]
    }

    // ── Empty / no-match ───────────────────────────────────────────

    #[test]
    fn empty_query_returns_no_results() {
        let commits = sample_commits();
        let hits = search_commits(&commits, "");
        assert!(hits.is_empty());
    }

    #[test]
    fn no_match_returns_empty() {
        let commits = sample_commits();
        let hits = search_commits(&commits, "zzzznonexistent");
        assert!(hits.is_empty());
    }

    // ── Summary matching ───────────────────────────────────────────

    #[test]
    fn matches_summary_case_insensitive() {
        let commits = sample_commits();
        let hits = search_commits(&commits, "initial COMMIT");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].index, 0);
        assert!(hits[0].fields.contains(&SearchField::Summary));
    }

    #[test]
    fn matches_summary_substring() {
        let commits = sample_commits();
        let hits = search_commits(&commits, "auth");
        // "Add authentication module" matches in summary
        assert!(hits.iter().any(|h| h.index == 1));
    }

    // ── Body matching ──────────────────────────────────────────────

    #[test]
    fn matches_body() {
        let commits = sample_commits();
        let hits = search_commits(&commits, "JWT");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].index, 1);
        assert!(hits[0].fields.contains(&SearchField::Body));
    }

    #[test]
    fn matches_body_case_insensitive() {
        let commits = sample_commits();
        let hits = search_commits(&commits, "postgresql");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].index, 3);
        assert!(hits[0].fields.contains(&SearchField::Body));
    }

    // ── Author matching ────────────────────────────────────────────

    #[test]
    fn matches_author_name() {
        let commits = sample_commits();
        let hits = search_commits(&commits, "alice");
        assert_eq!(hits.len(), 2); // commits 0 and 2
        let indices: BTreeSet<usize> = hits.iter().map(|h| h.index).collect();
        assert_eq!(indices, BTreeSet::from([0, 2]));
    }

    #[test]
    fn matches_author_email() {
        let commits = sample_commits();
        let hits = search_commits(&commits, "corp.dev");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].index, 3);
        assert!(hits[0].fields.contains(&SearchField::AuthorEmail));
    }

    // ── Path matching ──────────────────────────────────────────────

    #[test]
    fn matches_changed_path() {
        let commits = sample_commits();
        let hits = search_commits(&commits, "auth.rs");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].index, 1);
        assert!(hits[0].fields.contains(&SearchField::Path));
    }

    #[test]
    fn matches_path_partial() {
        let commits = sample_commits();
        let hits = search_commits(&commits, "migrations");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].index, 3);
    }

    #[test]
    fn matches_path_across_multiple_commits() {
        let commits = sample_commits();
        let hits = search_commits(&commits, "README");
        // commits 0 and 2 both touch README.md (in summary and/or path)
        let indices: BTreeSet<usize> = hits.iter().map(|h| h.index).collect();
        assert!(indices.contains(&0));
        assert!(indices.contains(&2));
    }

    // ── Multiple field matches ─────────────────────────────────────

    #[test]
    fn reports_all_matching_fields() {
        let commits = sample_commits();
        // "auth" appears in summary ("authentication") and path ("auth.rs")
        let hits = search_commits(&commits, "auth");
        let hit = hits.iter().find(|h| h.index == 1).unwrap();
        assert!(hit.fields.contains(&SearchField::Summary));
        assert!(hit.fields.contains(&SearchField::Body)); // "JWT-based auth"
        assert!(hit.fields.contains(&SearchField::Path));
    }

    // ── search_hit_indices ─────────────────────────────────────────

    #[test]
    fn hit_indices_extracts_sorted_set() {
        let commits = sample_commits();
        let hits = search_commits(&commits, "README");
        let indices = search_hit_indices(&hits);
        assert!(indices.contains(&0));
        assert!(indices.contains(&2));
    }

    #[test]
    fn hit_indices_empty_on_no_hits() {
        let hits: Vec<SearchHit> = vec![];
        assert!(search_hit_indices(&hits).is_empty());
    }

    // ── Edge cases ─────────────────────────────────────────────────

    #[test]
    fn search_on_empty_commit_list() {
        let hits = search_commits(&[], "anything");
        assert!(hits.is_empty());
    }

    #[test]
    fn search_single_char() {
        let commits = sample_commits();
        let hits = search_commits(&commits, "A");
        // Should match multiple commits (Alice, Add, auth.rs, etc.)
        assert!(!hits.is_empty());
    }
}

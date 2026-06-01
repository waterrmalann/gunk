use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

/// A full Git object ID (SHA-1 hex string).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CommitId(pub String);

impl CommitId {
    pub fn short(&self) -> &str {
        &self.0[..7.min(self.0.len())]
    }
}

impl std::fmt::Display for CommitId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Author or committer identity with timestamp.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Identity {
    pub name: String,
    pub email: String,
    #[serde(with = "time::serde::iso8601")]
    pub time: OffsetDateTime,
}

/// A single path change within a commit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PathChange {
    pub status: ChangeStatus,
    pub path: String,
}

/// The kind of change to a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChangeStatus {
    Added,
    Modified,
    Deleted,
    Renamed,
    Copied,
    TypeChange,
    Unknown,
}

/// A parsed Git commit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Commit {
    pub id: CommitId,
    pub parents: Vec<CommitId>,
    pub author: Identity,
    pub committer: Identity,
    pub summary: String,
    pub body: String,
    pub changed_paths: Vec<PathChange>,
}

impl Commit {
    /// Returns true if this is a merge commit (2+ parents).
    pub fn is_merge(&self) -> bool {
        self.parents.len() >= 2
    }
}

/// A glob-style path specification for file removal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PathSpec(pub String);

/// A co-author identity extracted from `Co-authored-by:` trailers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CoAuthor {
    pub name: String,
    pub email: String,
}

impl CoAuthor {
    /// Format as a Git trailer line.
    pub fn to_trailer(&self) -> String {
        format!("Co-authored-by: {} <{}>", self.name, self.email)
    }
}

/// The trailer key, matched case-insensitively per Git's trailer conventions.
const CO_AUTHOR_KEY: &str = "co-authored-by:";

/// If `line` begins with the (case-insensitive) `Co-authored-by:` key, return
/// the trimmed trailer value.
fn co_author_value(line: &str) -> Option<&str> {
    let trimmed = line.trim();
    if trimmed.len() < CO_AUTHOR_KEY.len() {
        return None;
    }
    let (head, rest) = trimmed.split_at(CO_AUTHOR_KEY.len());
    head.eq_ignore_ascii_case(CO_AUTHOR_KEY)
        .then(|| rest.trim())
}

/// Parse all `Co-authored-by:` trailers from a commit body (case-insensitive).
pub fn parse_co_authors(body: &str) -> Vec<CoAuthor> {
    let mut result = Vec::new();
    for line in body.lines() {
        if let Some(rest) = co_author_value(line)
            && let Some((name, email)) = parse_trailer_identity(rest)
        {
            result.push(CoAuthor { name, email });
        }
    }
    result
}

/// Strip all `Co-authored-by:` trailer lines from a body, trimming trailing whitespace.
pub fn strip_co_author_trailers(body: &str) -> String {
    let lines: Vec<&str> = body
        .lines()
        .filter(|line| co_author_value(line).is_none())
        .collect();
    lines.join("\n").trim_end().to_string()
}

/// Rebuild a commit body with the given co-authors appended as trailers.
///
/// Existing `Co-authored-by:` trailers are stripped first, then the new set is
/// appended after a blank separator line (if the body is non-empty).
pub fn set_co_authors_in_body(body: &str, co_authors: &[CoAuthor]) -> String {
    let stripped = strip_co_author_trailers(body);
    if co_authors.is_empty() {
        return stripped;
    }
    let trailers: String = co_authors
        .iter()
        .map(|ca| ca.to_trailer())
        .collect::<Vec<_>>()
        .join("\n");
    if stripped.is_empty() {
        trailers
    } else {
        format!("{stripped}\n\n{trailers}")
    }
}

/// Parse `Name <email>` from a trailer value.
fn parse_trailer_identity(s: &str) -> Option<(String, String)> {
    let s = s.trim();
    let open = s.find('<')?;
    let close = s.find('>')?;
    if close <= open + 1 {
        return None;
    }
    let name = s[..open].trim().to_string();
    let email = s[open + 1..close].trim().to_string();
    if name.is_empty() || email.is_empty() {
        return None;
    }
    Some((name, email))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ca(name: &str, email: &str) -> CoAuthor {
        CoAuthor {
            name: name.into(),
            email: email.into(),
        }
    }

    #[test]
    fn parse_co_authors_single() {
        let body = "Some body\n\nCo-authored-by: Alice <alice@example.com>";
        let result = parse_co_authors(body);
        assert_eq!(result, vec![ca("Alice", "alice@example.com")]);
    }

    #[test]
    fn parse_co_authors_multiple() {
        let body =
            "Body text\n\nCo-authored-by: Alice <alice@x.com>\nCo-authored-by: Bob <bob@x.com>";
        let result = parse_co_authors(body);
        assert_eq!(
            result,
            vec![ca("Alice", "alice@x.com"), ca("Bob", "bob@x.com")]
        );
    }

    #[test]
    fn parse_co_authors_empty_body() {
        assert!(parse_co_authors("").is_empty());
    }

    #[test]
    fn parse_co_authors_no_trailers() {
        assert!(parse_co_authors("Just a regular body").is_empty());
    }

    #[test]
    fn parse_co_authors_malformed_skipped() {
        let body = "Co-authored-by: no angle brackets\nCo-authored-by: Alice <alice@x.com>";
        let result = parse_co_authors(body);
        assert_eq!(result, vec![ca("Alice", "alice@x.com")]);
    }

    #[test]
    fn to_trailer_formats_correctly() {
        let ca = ca("Alice", "alice@example.com");
        assert_eq!(ca.to_trailer(), "Co-authored-by: Alice <alice@example.com>");
    }

    #[test]
    fn strip_co_author_trailers_removes_all() {
        let body = "Body text\n\nCo-authored-by: Alice <a@x.com>\nCo-authored-by: Bob <b@x.com>";
        assert_eq!(strip_co_author_trailers(body), "Body text");
    }

    #[test]
    fn strip_co_author_trailers_empty_body() {
        assert_eq!(strip_co_author_trailers(""), "");
    }

    #[test]
    fn strip_co_author_trailers_case_insensitive() {
        let body = "Body\n\nco-authored-by: Alice <a@x.com>";
        assert_eq!(strip_co_author_trailers(body), "Body");
    }

    #[test]
    fn set_co_authors_in_body_adds_to_empty() {
        let result = set_co_authors_in_body("", &[ca("Alice", "a@x.com")]);
        assert_eq!(result, "Co-authored-by: Alice <a@x.com>");
    }

    #[test]
    fn set_co_authors_in_body_replaces_existing() {
        let body = "Body\n\nCo-authored-by: Old <old@x.com>";
        let result = set_co_authors_in_body(body, &[ca("New", "new@x.com")]);
        assert_eq!(result, "Body\n\nCo-authored-by: New <new@x.com>");
    }

    #[test]
    fn set_co_authors_in_body_clears_when_empty() {
        let body = "Body\n\nCo-authored-by: Alice <a@x.com>";
        let result = set_co_authors_in_body(body, &[]);
        assert_eq!(result, "Body");
    }

    #[test]
    fn set_co_authors_in_body_multiple() {
        let body = "Body text";
        let result = set_co_authors_in_body(body, &[ca("Alice", "a@x.com"), ca("Bob", "b@x.com")]);
        assert_eq!(
            result,
            "Body text\n\nCo-authored-by: Alice <a@x.com>\nCo-authored-by: Bob <b@x.com>"
        );
    }
}

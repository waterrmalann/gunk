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

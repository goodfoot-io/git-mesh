//! Serde structs for `baseline.state` and `last-flush.state`.

use serde::{Deserialize, Serialize};

/// State snapshot written by `snapshot` and read by bare render.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BaselineState {
    /// Must equal `1`; future versions increment this.
    pub schema_version: u32,
    /// SHA-1 hex of the workspace tree object written to `baseline.objects/`.
    pub tree_sha: String,
    /// SHA-1 hex of the index checksum at snapshot time.
    pub index_sha: String,
    /// RFC-3339 timestamp when the snapshot was taken.
    pub captured_at: String,
}

/// State written to `last-flush.state`; structurally identical to `BaselineState`.
pub type LastFlushState = BaselineState;

/// One entry in `reads.jsonl`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadRecord {
    /// Repo-relative path.
    pub path: String,
    /// Inclusive 1-based start line, if a range was supplied.
    pub start_line: Option<u32>,
    /// Inclusive 1-based end line, if a range was supplied.
    pub end_line: Option<u32>,
    /// RFC-3339 timestamp of the read event.
    pub ts: String,
}

/// One entry in `touches.jsonl`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TouchInterval {
    /// Repo-relative path.
    pub path: String,
    /// Inclusive 1-based start line.
    pub start_line: u32,
    /// Inclusive 1-based end line.
    pub end_line: u32,
    /// RFC-3339 timestamp.
    pub ts: String,
}

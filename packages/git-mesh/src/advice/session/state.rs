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
    /// Byte offset into `reads.jsonl` consumed as of the last successful
    /// render. Persisted inside the state file so a single `last-flush.state`
    /// rename advances both the tree pointer and the read cursor atomically.
    /// Defaults to 0 for backwards-compat with state files written before
    /// this field existed.
    #[serde(default)]
    pub read_cursor: u64,
}

/// State written to `last-flush.state`; structurally identical to `BaselineState`.
pub type LastFlushState = BaselineState;

/// One entry in `reads.jsonl`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadRecord {
    /// Repo-relative path.
    pub path: String,
    /// Inclusive 1-based start line, if a anchor was supplied.
    pub start_line: Option<u32>,
    /// Inclusive 1-based end line, if a anchor was supplied.
    pub end_line: Option<u32>,
    /// RFC-3339 timestamp of the read event.
    pub ts: String,
}

/// Per-session print-gate flags persisted at `<session>/flags.state`.
///
/// These gates ensure that instruction blocks are printed at most once per
/// advice session, independent of how many times `milestone` or `stop` runs.
/// The `snapshot` verb resets this file to `SessionFlags::default()`.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct SessionFlags {
    /// True after reconciliation instructions have been printed at least once
    /// this session.
    #[serde(default)]
    pub has_printed_reconciliation_instructions: bool,
    /// True after creation instructions (for new files with related anchors)
    /// have been printed at least once this session.
    #[serde(default)]
    pub has_printed_creation_instructions: bool,
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

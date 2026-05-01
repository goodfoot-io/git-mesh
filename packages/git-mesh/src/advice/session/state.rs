//! Serde structs for the per-session JSONL streams and per-`<id>` snapshots.
//!
//! Each `mark`/`flush` pair is identified by an opaque `id` chosen by the
//! caller — the hook scripts pass `tool_use_id` as `id`, but the CLI does not
//! know what the value means. Schema fields are deliberately generic.

use serde::{Deserialize, Serialize};

/// One entry in `reads.jsonl`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadRecord {
    /// Repo-relative path.
    pub path: String,
    /// Inclusive 1-based start line, if a line-range anchor was supplied.
    pub start_line: Option<u32>,
    /// Inclusive 1-based end line, if a line-range anchor was supplied.
    pub end_line: Option<u32>,
    /// RFC-3339 timestamp of the read event.
    pub ts: String,
    /// Opaque caller-chosen id (the hook layer passes the originating
    /// `tool_use_id`). Optional — direct CLI invocations may omit it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
}

/// Kind of working-tree change attributed to a single tool call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TouchKind {
    Modified,
    Added,
    Deleted,
    ModeChange,
}

fn default_touch_kind() -> TouchKind {
    TouchKind::Modified
}

/// One entry in `touches.jsonl`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TouchInterval {
    /// Repo-relative path.
    pub path: String,
    /// Diff classification produced by `flush`.
    #[serde(default = "default_touch_kind")]
    pub kind: TouchKind,
    /// Opaque caller-chosen id that bracketed the change (the hook layer
    /// passes the originating `tool_use_id`).
    #[serde(default)]
    pub id: String,
    /// RFC-3339 timestamp.
    pub ts: String,
    /// Inclusive 1-based start line of the edited hunk, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start: Option<u32>,
    /// Inclusive 1-based end line of the edited hunk, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end: Option<u32>,
}

/// Per-session print-gate flags persisted at `<session>/flags.state`.
///
/// These gates ensure that instruction blocks are printed at most once per
/// advice session, independent of how many `flush` invocations occur.
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

/// One entry in `<session>/snapshots/<id>.untracked` — captured at `mark`,
/// consumed at `flush` to detect untracked-side changes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UntrackedSnapshotEntry {
    pub path: String,
    pub size: u64,
    pub mode: u32,
    pub mtime_ns: i128,
    pub ctime_ns: i128,
    pub ino: u64,
}

//! Data shapes for git-mesh.
//!
//! All types describe the v1 on-disk shape (see `docs/git-mesh.md` §4).
//! Every field is required; defaults are applied at creation time so
//! stored records fully self-describe their resolver behaviour.
//!
//! ## Error type
//!
//! This crate uses `thiserror` to define a library-level `Error` enum as
//! the public boundary for fallible operations. A CLI crate could reach
//! for `anyhow::Error` for brevity, but an enum-based error makes it
//! possible for downstream consumers (including the crate's own tests
//! and future library consumers) to match on variants without string
//! matching, which is the idiomatic Rust public-API choice.

use serde::{Deserialize, Serialize};

/// In-memory representation of the Range record stored at
/// `refs/ranges/v1/<rangeId>`. The id itself is the ref name suffix and
/// is not repeated in the blob.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Range {
    /// Commit this range was anchored to at creation.
    pub anchor_sha: String,
    /// ISO-8601 creation timestamp.
    pub created_at: String,
    /// File path at the anchor commit.
    pub path: String,
    /// 1-based, inclusive start line.
    pub start: u32,
    /// 1-based, inclusive end line.
    pub end: u32,
    /// Blob OID of `path` at `anchor_sha`.
    pub blob: String,
}

/// `-C` levels for `git log -L` copy detection. Stored in mesh config,
/// not in the range record. Serialized as the kebab-case variant name:
/// `off`, `same-commit`, `any-file-in-commit`, `any-file-in-repo`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CopyDetection {
    Off,
    SameCommit,
    AnyFileInCommit,
    AnyFileInRepo,
}

/// Resolver options for all ranges in a mesh.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct MeshConfig {
    pub copy_detection: CopyDetection,
    pub ignore_whitespace: bool,
}

pub const DEFAULT_COPY_DETECTION: CopyDetection = CopyDetection::SameCommit;
pub const DEFAULT_IGNORE_WHITESPACE: bool = false;

/// A Mesh is a commit whose tree contains `ranges` and `config` files
/// and whose commit message is the Mesh's message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mesh {
    /// The Mesh's name (ref suffix; the identity).
    pub name: String,
    /// Active Range ids. Canonical order: sorted by the referenced
    /// Range's `(path, start, end)` ascending.
    pub ranges: Vec<String>,
    /// The commit's message.
    pub message: String,
    /// Resolver options for all ranges in this mesh.
    pub config: MeshConfig,
}

/// Declaration order is best → worst; `Ord` derives a total order so
/// callers that want a one-line summary can reduce via `.max()`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum RangeStatus {
    /// Current bytes equal anchored bytes.
    Fresh,
    /// Bytes equal; `(path, start, end)` changed.
    Moved,
    /// Anchored bytes differ from current bytes, including complete deletion.
    Changed,
    /// `anchor_sha` is not reachable from any ref.
    Orphaned,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct RangeLocation {
    pub path: String,
    pub start: u32,
    pub end: u32,
    pub blob: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RangeResolved {
    pub range_id: String,
    pub anchor_sha: String,
    pub anchored: RangeLocation,
    pub current: Option<RangeLocation>,
    pub status: RangeStatus,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MeshResolved {
    pub name: String,
    pub message: String,
    /// One resolved entry per Range id in the Mesh, in the Mesh's
    /// stored order.
    pub ranges: Vec<RangeResolved>,
}

/// Public error boundary for the `git-mesh` library.
///
/// Variants are intentionally specific so callers (CLI, tests, future
/// library consumers) can match without string-sniffing. Each variant
/// is documented with the spec section that motivates it.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A range ref `refs/ranges/v1/<id>` does not exist (§3.1).
    #[error("range not found: {0}")]
    RangeNotFound(String),

    /// A mesh ref `refs/meshes/v1/<name>` does not exist (§3.1).
    #[error("mesh not found: {0}")]
    MeshNotFound(String),

    /// CAS conflict: the mesh ref already exists when a create-only
    /// operation expected it absent (§6.2).
    #[error("mesh already exists: {0}")]
    MeshAlreadyExists(String),

    /// Two ranges in the same mesh share `(path, start, end)` (§4.2 invariant).
    #[error("duplicate range location in mesh: {path}:{start}-{end}")]
    DuplicateRangeLocation {
        path: String,
        start: u32,
        end: u32,
    },

    /// `start` is not >= 1, or `end` < `start`, or the line range is
    /// outside the file's line count at the anchor commit (§6.1).
    #[error("invalid range: start={start} end={end}")]
    InvalidRange { start: u32, end: u32 },

    /// On-disk record could not be parsed (range blob, ranges file,
    /// config file, or staging operations file). (§4.1, §4.2, §6.3)
    #[error("parse error: {0}")]
    Parse(String),

    /// A staging operations-file line could not be parsed (§6.3).
    #[error("parse staging line: {line}")]
    ParseStaging { line: String },

    /// Mesh-ref CAS update lost a race; caller should reload and retry (§6.2).
    #[error("concurrent update: expected {expected}, found {found}")]
    ConcurrentUpdate { expected: String, found: String },

    /// Mesh name is on the §10.2 reserved list (collides with a subcommand).
    #[error("reserved mesh name: {0}")]
    ReservedName(String),

    /// Mesh name or range id violates the §3.5 ref-legal rules.
    #[error("invalid name: {0}")]
    InvalidName(String),

    /// `git mesh commit` invoked with nothing meaningful staged (§6.2).
    #[error("nothing staged for mesh: {0}")]
    StagingEmpty(String),

    /// First commit on a new mesh requires a staged message (§6.2, §10.2).
    #[error("message required for first commit on mesh: {0}")]
    MessageRequired(String),

    /// Working-tree drift detected by `git mesh status` or commit-time
    /// drift check; sidecar bytes differ from the file on disk or HEAD blob (§6.3).
    #[error("working tree drift: {path}#L{start}-L{end}")]
    WorkingTreeDrift {
        path: String,
        start: u32,
        end: u32,
        diff: String,
    },

    /// `anchor_sha` is not reachable; resolver classifies the range as
    /// `Orphaned` rather than erroring, but callers writing new ranges
    /// surface this as a hard error (§5.3, §6.8).
    #[error("anchor commit unreachable: {anchor_sha}")]
    Unreachable { anchor_sha: String },

    /// Remote does not have any `refs/{ranges,meshes}/*` refspec
    /// configured, and lazy-config refused to add it (§7.1, §6.7 doctor).
    #[error("refspec missing for remote: {remote}")]
    RefspecMissing { remote: String },

    /// `git mesh commit` aborted because the staged config value matches
    /// the committed value and no other meaningful change is staged (§6.2).
    #[error("staged config is a no-op: {key}={value}")]
    ConfigNoOp { key: String, value: String },

    /// Range address `<path>#L<start>-L<end>` could not be parsed (§10.3).
    #[error("invalid range address: {0}")]
    InvalidRangeAddress(String),

    /// Path lookup in a tree failed (§6.1 step 2).
    #[error("path not in tree: {path} at {commit}")]
    PathNotInTree { path: String, commit: String },

    /// Mesh staged operation references a `(path, start, end)` not
    /// present in the current mesh (§6.2 step 3).
    #[error("range not in mesh: {path}#L{start}-L{end}")]
    RangeNotInMesh {
        path: String,
        start: u32,
        end: u32,
    },

    /// Generic git-process / gix error.
    #[error("git: {0}")]
    Git(String),

    #[error(transparent)]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, Error>;

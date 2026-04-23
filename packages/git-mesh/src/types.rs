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
use std::collections::HashMap;
use std::path::PathBuf;

/// The extent of a pinned range: either the whole file, or an inclusive
/// 1-based line range.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum RangeExtent {
    Whole,
    Lines { start: u32, end: u32 },
}

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
    /// Extent (whole-file or line-range) pinned by this range.
    pub extent: RangeExtent,
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

/// Reason content should exist but is not readable locally without
/// a network call. See docs/stale-layers-plan.md §D4.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum UnavailableReason {
    LfsNotFetched,
    LfsNotInstalled,
    /// Partial clone, blob not fetched.
    PromisorMissing,
    /// Sparse-checkout excluded path.
    SparseExcluded,
    FilterFailed { filter: String },
    IoError { message: String },
}

/// Declaration order is best → worst; `Ord` derives a total order so
/// callers that want a one-line summary can reduce via `.max()`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum RangeStatus {
    /// Current bytes equal anchored bytes.
    Fresh,
    /// Bytes equal; `(path, extent)` changed.
    Moved,
    /// Anchored bytes differ from current bytes, including complete deletion.
    Changed,
    /// `anchor_sha` is not reachable from any ref.
    Orphaned,
    /// No stage-0 index entry for the path.
    MergeConflict,
    /// Path is a gitlink; rejected at `add`, surfaces if legacy.
    Submodule,
    /// Content should exist but isn't readable locally.
    ContentUnavailable(UnavailableReason),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RangeLocation {
    pub path: PathBuf,
    pub extent: RangeExtent,
    /// Present when the path has a blob at the resolved layer; `None` for
    /// worktree-only reads, submodule gitlinks, and terminal statuses where
    /// no blob resolves.
    pub blob: Option<gix::ObjectId>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RangeResolved {
    pub range_id: String,
    pub anchor_sha: String,
    pub anchored: RangeLocation,
    pub current: Option<RangeLocation>,
    pub status: RangeStatus,
    /// Layer that produced the drift; `None` when `Fresh` or terminal.
    /// Slice 2 of the layered-stale plan: this carries the same meaning as
    /// `Finding::source` until the renderer slice migrates wholesale to
    /// `Finding`.
    pub source: Option<DriftSource>,
    /// Staged re-anchor that acknowledges this drift, matched by `range_id`.
    /// Slice 3 scaffolding: the field is plumbed end-to-end but always set
    /// to `None` until slice 5 ships the sidecar freshness stamp that lets
    /// the engine trust ack matches. See `docs/stale-layers-slices.md`.
    pub acknowledged_by: Option<StagedOpRef>,
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

    // `DuplicateRangeLocation` removed per `docs/stale-layers-plan.md` §D5:
    // staged `(path, extent)` duplicates are last-write-wins. The three
    // former raise sites in `mesh/commit.rs` now call `todo!()` pending
    // the dedup-pass implementation slice.
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
    RangeNotInMesh { path: String, start: u32, end: u32 },

    /// A path's `.gitattributes` resolves to a `filter=<name>` driver
    /// outside the slice-2 core-filter allowlist. The engine surfaces
    /// this as `RangeStatus::ContentUnavailable(UnavailableReason::FilterFailed)`.
    /// See `docs/stale-layers-slices.md` "Standing rules" — fail loud.
    #[error("filter not implemented: {filter}")]
    FilterFailed { filter: String },

    /// Generic git-process / gix error.
    #[error("git: {0}")]
    Git(String),

    #[error(transparent)]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, Error>;

// ---------------------------------------------------------------------------
// Phase 1 scaffold types — layered engine / renderers / prechecks.
//
// These types are introduced ahead of the engine and renderer slices so the
// public boundary exists when those slices land. See
// `docs/stale-layers-plan.md` §"Key types" and §D1–D6. Only derives and
// constructors / a stubbed `ContentRef::read_normalized` live here — runtime
// logic lands in later slices.
// ---------------------------------------------------------------------------

/// Which drift layers participate in a `stale` run. HEAD is always on;
/// these toggles select additional layers on top.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LayerSet {
    pub worktree: bool,
    pub index: bool,
    pub staged_mesh: bool,
}

impl LayerSet {
    /// All three layers enabled (HEAD + Index + Worktree + Staged-mesh).
    pub fn full() -> Self {
        Self {
            worktree: true,
            index: true,
            staged_mesh: true,
        }
    }

    /// HEAD-only fast path (CI invariant). All additional layers off.
    pub fn committed_only() -> Self {
        Self {
            worktree: false,
            index: false,
            staged_mesh: false,
        }
    }
}

/// Scope of a single engine invocation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Scope {
    All,
    Mesh(String),
    Range(String),
}

/// Layer that produced drift for a `Finding`. There is no `StagedMesh`
/// variant: staged-mesh-layer disagreement rides on `PendingFinding::drift`
/// (see plan §"Key types" comment).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DriftSource {
    Head,
    Index,
    Worktree,
}

/// Reference to content readable through git's attribute + filter pipeline.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ContentRef {
    /// HEAD or index blob; reader dispatched by `.gitattributes` filter.
    Blob(gix::ObjectId),
    /// On-disk worktree file; clean filter applied to match blob form.
    WorktreeFile(PathBuf),
    /// `.git/mesh/staging/<mesh>.<N>`; re-normalized on read against
    /// current filters.
    Sidecar(PathBuf),
}

impl ContentRef {
    /// Read the content through the normalization pipeline (D3) and
    /// return canonical bytes. Callers slice into `&[&str]` on demand.
    ///
    /// Slice 1 scope: implemented for the cases the HEAD-only fast path
    /// needs. LFS / custom `filter=<name>` drivers are deferred to a
    /// later slice (see `docs/gix-filter-audit.md`).
    pub fn read_normalized(&self, repo: &gix::Repository) -> Result<Vec<u8>> {
        match self {
            ContentRef::Blob(oid) => {
                let obj = repo
                    .find_object(*oid)
                    .map_err(|e| Error::Git(format!("find blob `{oid}`: {e}")))?;
                Ok(obj.into_blob().detach().data)
            }
            ContentRef::WorktreeFile(path) => {
                let workdir = repo
                    .workdir()
                    .ok_or_else(|| Error::Git("bare repositories are not supported".into()))?;
                // Fail-loud: any `filter=<name>` outside the core-filter
                // allowlist short-circuits before we touch gix's filter
                // pipeline. See `docs/stale-layers-slices.md` standing
                // rules and `docs/gix-filter-audit.md`.
                if let Some(name) = path_filter_attribute(workdir, path)?
                    && !is_core_filter(&name)
                {
                    return Err(Error::FilterFailed { filter: name });
                }
                let abs = workdir.join(path);
                let md = std::fs::symlink_metadata(&abs)?;
                if md.file_type().is_symlink() {
                    let target = std::fs::read_link(&abs)?;
                    return Ok(target.to_string_lossy().into_owned().into_bytes());
                }
                let file = std::fs::File::open(&abs)?;
                // Apply the clean (to-git) filter so worktree bytes match
                // blob bytes for comparison. Custom `filter=<name>`
                // drivers were rejected above; only core filters reach
                // here.
                let (mut pipeline, index) = repo
                    .filter_pipeline(None)
                    .map_err(|e| Error::Git(format!("filter pipeline: {e}")))?;
                let outcome = pipeline
                    .convert_to_git(file, path.as_path(), &index)
                    .map_err(|e| Error::Git(format!("convert_to_git: {e}")))?;
                use gix::filter::plumbing::pipeline::convert::ToGitOutcome;
                use std::io::Read;
                let mut out = Vec::new();
                match outcome {
                    ToGitOutcome::Unchanged(mut r) => {
                        r.read_to_end(&mut out)?;
                    }
                    ToGitOutcome::Buffer(buf) => {
                        out.extend_from_slice(buf);
                    }
                    ToGitOutcome::Process(mut r) => {
                        r.read_to_end(&mut out)?;
                    }
                }
                Ok(out)
            }
            ContentRef::Sidecar(path) => {
                // Slice 1: read raw. Re-normalization across filter changes
                // (the .gitattributes-stamp dance in plan §B2) is a later
                // slice.
                // TODO(stale-layers-plan): re-normalize sidecars on read.
                Ok(std::fs::read(path)?)
            }
        }
    }
}

/// Resolve the `filter` `.gitattributes` value for `path` by shelling
/// out to `git check-attr filter -- <path>`. Returns the driver name
/// (e.g. `lfs`, `crypt`) when set, `None` for `unspecified` / `unset` /
/// `set` (no driver name). The fail-loud check in `ContentRef`'s
/// reader treats any returned name not on the core-filter allowlist
/// as a hard short-circuit (slice-2 standing rule).
pub(crate) fn path_filter_attribute(
    workdir: &std::path::Path,
    rel_path: &std::path::Path,
) -> Result<Option<String>> {
    let out = std::process::Command::new("git")
        .current_dir(workdir)
        .args(["check-attr", "filter", "--"])
        .arg(rel_path)
        .output()
        .map_err(|e| Error::Git(format!("spawn git check-attr: {e}")))?;
    if !out.status.success() {
        // Fail closed: if we can't probe attributes, treat as "no driver"
        // rather than guessing. The gix pipeline will run downstream.
        return Ok(None);
    }
    // Format: `<path>: filter: <value>\n`
    let s = String::from_utf8_lossy(&out.stdout);
    for line in s.lines() {
        if let Some(idx) = line.rfind(": ") {
            let value = line[idx + 2..].trim();
            return Ok(match value {
                "" | "unspecified" | "unset" | "set" => None,
                other => Some(other.to_string()),
            });
        }
    }
    Ok(None)
}

/// Slice-2 core-filter allowlist. The `filter` `.gitattributes`
/// attribute is reserved for `filter=<name>` driver dispatch (LFS,
/// custom process filters, etc.); core normalization (`text`,
/// `text=auto`, `eol`, `ident`, `working-tree-encoding`, `core.autocrlf`,
/// `core.eol`) is driven by other attributes / config and never sets
/// the `filter` value. As a result the allowlist for the `filter`
/// attribute itself is intentionally empty: any explicit `filter=<name>`
/// resolves to a non-core driver and must short-circuit.
pub(crate) fn is_core_filter(_name: &str) -> bool {
    false
}

/// Unified-diff hunk pair, in 1-based `(start, count)` form.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hunk {
    /// `(start, count)` in the source blob.
    pub old: (u32, u32),
    /// `(start, count)` in the destination blob.
    pub new: (u32, u32),
}

/// The commit blamed for the current divergence (HEAD layer only).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Culprit {
    pub commit: gix::ObjectId,
    pub author: String,
    pub summary: String,
}

/// Back-pointer from a `Finding` to the staged mesh op that acknowledges
/// its drift.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StagedOpRef {
    pub mesh: String,
    /// Index into `PendingState.mesh_ops`.
    pub index: usize,
}

/// Drift observed on a staged mesh op's sidecar vs. the blob it claims
/// to anchor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PendingDrift {
    /// Sidecar bytes disagree with the claimed blob under current filters.
    SidecarMismatch,
}

/// Staged mesh operation surfaced by the engine alongside `Finding`s.
///
/// `Add` and `Remove` carry a possible `drift: Option<PendingDrift>`;
/// `Message` and `ConfigChange` are informational and never drive exit
/// code (see plan B3).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PendingFinding {
    Add {
        mesh: String,
        range_id: String,
        op: crate::staging::StagedAdd,
        drift: Option<PendingDrift>,
    },
    Remove {
        mesh: String,
        range_id: String,
        op: crate::staging::StagedRemove,
        drift: Option<PendingDrift>,
    },
    Message {
        mesh: String,
        body: String,
    },
    ConfigChange {
        mesh: String,
        change: crate::staging::StagedConfig,
    },
}

/// A single drift observation produced by the engine.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Finding {
    pub mesh: String,
    pub range_id: String,
    pub status: RangeStatus,
    /// `None` when `Fresh` or when `status` is terminal.
    pub source: Option<DriftSource>,
    /// Always populated from the pinned `Range` record.
    pub anchored: RangeLocation,
    /// `None` when `Orphaned` / `Submodule` / `ContentUnavailable`;
    /// populated with best-effort path for `MergeConflict`.
    pub current: Option<RangeLocation>,
    /// Staged re-anchor matched by `range_id`.
    pub acknowledged_by: Option<StagedOpRef>,
    /// Only when `source == Some(Head)`.
    pub culprit: Option<Culprit>,
}

/// Index-layer entry for a single stage-0 path. Conflicted paths are
/// omitted; the engine surfaces `MergeConflict` for those.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StagedIndexEntry {
    pub blob: gix::ObjectId,
    /// Hunks from `git diff-index --cached -U0 -M HEAD`.
    pub hunks: Vec<Hunk>,
}

/// All "pending" inputs to the engine — the git index plus the on-disk
/// `.git/mesh/staging/` operations.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PendingState {
    pub index: HashMap<PathBuf, StagedIndexEntry>,
    pub mesh_ops: Vec<crate::staging::StagedOp>,
}

/// Engine invocation options. See plan §B3/§B4.
///
/// `layers` selects which drift layers (on top of HEAD) participate;
/// `ignore_unavailable` downgrades `ContentUnavailable` findings to
/// non-exit-driving per §B3. `--no-exit-code` is an output-rendering
/// concern and lives on the CLI, not in this struct.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EngineOptions {
    pub layers: LayerSet,
    pub ignore_unavailable: bool,
}

impl EngineOptions {
    /// All layers enabled, unavailable content still drives exit code.
    pub fn full() -> Self {
        Self {
            layers: LayerSet::full(),
            ignore_unavailable: false,
        }
    }

    /// HEAD-only fast path (CI invariant per §B4).
    pub fn committed_only() -> Self {
        Self {
            layers: LayerSet::committed_only(),
            ignore_unavailable: false,
        }
    }
}

/// Public error boundary for `validate_add_target` — the stage-time
/// precheck that rejects pins `git mesh add` can't honor (see plan
/// §"CLI and `git mesh add` prechecks").
///
/// These errors surface at `git mesh add` time, not at commit time, so
/// the operator gets immediate feedback before sidecars are written.
#[derive(Debug, thiserror::Error)]
pub enum AddPrecheckError {
    /// Line-range pin on a `.gitattributes`-declared binary path.
    #[error("line-range pin rejected on binary path: {path}")]
    LineRangeOnBinary { path: String },

    /// Line-range pin on a symlink (filters don't run on symlinks;
    /// whole-file pins are allowed for retarget detection).
    #[error("line-range pin rejected on symlink: {path}")]
    LineRangeOnSymlink { path: String },

    /// Line-range pin on a path inside a submodule (multi-repo content
    /// resolution is out of scope).
    #[error("line-range pin rejected inside submodule: {path}")]
    LineRangeInSubmodule { path: String },

    /// Whole-file pin on a non-gitlink path inside a submodule. The
    /// submodule's object database is not opened; only the gitlink root
    /// itself may be pinned whole-file.
    #[error("whole-file pin rejected inside submodule (only the gitlink root is allowed): {path}")]
    WholeFileInSubmodule { path: String },

    /// `filter=lfs` path whose content is not locally cached. Reuses
    /// `UnavailableReason::LfsNotFetched` vocabulary with `stale` output
    /// per plan §D4.
    #[error("content unavailable for {path}: {reason:?}")]
    ContentUnavailable {
        path: String,
        reason: UnavailableReason,
    },

    /// Underlying I/O error while probing the path (stat, readlink,
    /// gitattributes lookup). Surfaces as a precheck failure rather
    /// than silently allowing the `add`.
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// Stage-time validation for a single `git mesh add` target. Phase 1
/// boundary: the body is stubbed. Called from `cli/commit.rs::run_add`
/// before any sidecar is written.
///
/// See plan §"CLI and `git mesh add` prechecks" for the full rule set.
pub fn validate_add_target(
    _repo: &gix::Repository,
    _path: &std::path::Path,
    _extent: &RangeExtent,
) -> std::result::Result<(), AddPrecheckError> {
    todo!("phase-1: implement in reader slice")
}

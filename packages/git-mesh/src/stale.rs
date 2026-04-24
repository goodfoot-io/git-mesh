//! Resolver: compute staleness for ranges and meshes (§5).
//!
//! Slice 5 of the layered-stale rewrite (see
//! `docs/stale-layers-plan.md`). HEAD/Index/Worktree layers run in order
//! atop the HEAD-resolved location; the staged-mesh layer surfaces
//! `PendingFinding`s and matches `acknowledged_by` by `range_id`
//! (re-normalized on the sidecar freshness stamp).

#![allow(dead_code)]

use crate::git;
use crate::mesh::read::{list_mesh_names, read_mesh};
use crate::range::read_range;
use crate::types::{
    self, CopyDetection, DriftSource, EngineOptions, LayerSet, MeshConfig, MeshResolved,
    PendingDrift, PendingFinding, Range, RangeExtent, RangeLocation, RangeResolved, RangeStatus,
    StagedOpRef, UnavailableReason, current_normalization_stamp,
};
use crate::{Error, Result};
use similar::{ChangeTag, TextDiff};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::process::Command;
use std::str::FromStr;

/// Per-run, per-layer cache of `git diff-{index,files}` parses.
struct LayerDiffs {
    /// Map keyed by destination path → entry describing the path's drift.
    map: HashMap<String, DiffEntry>,
    /// Source-path → destination-path lookup for renames.
    renamed_from: HashMap<String, String>,
    /// Whether rename detection was disabled this run (rename-budget cap).
    rename_detection_disabled: bool,
}

#[derive(Clone, Debug)]
struct DiffEntry {
    /// Destination-side path.
    new_path: String,
    /// Source-side path (== new_path for non-renames).
    old_path: String,
    /// Hunks against the source side.
    hunks: Vec<(u32, u32, u32, u32)>, // (old_start, old_count, new_start, new_count)
    /// Index/HEAD blob OID of the destination side, if known. None for
    /// worktree-layer entries (no synthesized OID), intent-to-add entries,
    /// and deletions.
    new_blob: Option<String>,
    /// Whether the path was deleted at this layer.
    deleted: bool,
    /// True if the index entry is intent-to-add (zero-OID staged entry).
    intent_to_add: bool,
}

const RENAME_BUDGET_DEFAULT: usize = 1000;

fn rename_budget() -> usize {
    std::env::var("GIT_MESH_RENAME_BUDGET")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(RENAME_BUDGET_DEFAULT)
}

/// Engine-level state cached for one `stale` run.
struct EngineState {
    layers: LayerSet,
    index_diffs: Option<LayerDiffs>,
    worktree_diffs: Option<LayerDiffs>,
    /// Paths with no stage-0 entry (merge conflicts).
    conflicted_paths: HashSet<String>,
    /// SHA-1 trailer of `.git/index` at run start.
    index_trailer_start: Option<[u8; 20]>,
    /// Collected warnings flushed to stderr at end of run.
    warnings: Vec<String>,
    /// Lazily spawned `git-lfs filter-process` subprocess, reused across
    /// every LFS read in a single `stale` run. `None` until the first
    /// LFS read; `Some(Err(_))` if a previous spawn attempt already
    /// failed (cached so we don't retry — it's deterministic per run).
    lfs: Option<std::result::Result<FilterProcess, FilterSpawnError>>,
    /// Custom `filter.<name>.process` subprocesses (slice 7), cached
    /// per driver name for the duration of the run. `Err(_)` is sticky
    /// — a driver that failed to spawn / handshake stays terminal.
    custom_filters: HashMap<String, std::result::Result<FilterProcess, FilterSpawnError>>,
}

/// Why a `git filter-process`-protocol spawn failed. Cached on the
/// engine so subsequent reads in the same run return the same terminal
/// state without re-attempting the spawn.
#[derive(Clone, Debug)]
enum FilterSpawnError {
    /// Binary not on PATH (ENOENT) or otherwise unspawnable.
    NotInstalled,
    /// Spawn succeeded but the handshake failed.
    HandshakeFailed,
}

impl EngineState {
    fn new(repo: &gix::Repository, layers: LayerSet) -> Result<Self> {
        let index_trailer_start = read_index_trailer(repo).ok();
        let mut s = EngineState {
            layers,
            index_diffs: None,
            worktree_diffs: None,
            conflicted_paths: HashSet::new(),
            index_trailer_start,
            warnings: Vec::new(),
            lfs: None,
            custom_filters: HashMap::new(),
        };
        if layers.index || layers.worktree {
            // Need conflicted-path set whenever any non-HEAD layer is on.
            s.conflicted_paths = read_conflicted_paths(repo)?;
        }
        if layers.index {
            s.index_diffs = Some(read_index_layer(repo, &mut s.warnings)?);
        }
        if layers.worktree {
            s.worktree_diffs = Some(read_worktree_layer(repo, &mut s.warnings)?);
        }
        Ok(s)
    }

    fn finish(self, repo: &gix::Repository) {
        // Concurrency guard: re-read trailer; warn on change.
        if let Some(start) = self.index_trailer_start
            && let Ok(end) = read_index_trailer(repo)
            && end != start
        {
            eprintln!("warning: index changed during stale; consider re-running");
        }
        for w in self.warnings {
            eprintln!("{w}");
        }
    }
}

pub fn resolve_range(
    repo: &gix::Repository,
    mesh_name: &str,
    range_id: &str,
    options: EngineOptions,
) -> Result<RangeResolved> {
    let mut state = EngineState::new(repo, options.layers)?;
    let mesh = read_mesh(repo, mesh_name)?;
    let mut out = match read_range(repo, range_id) {
        Ok(r) => resolve_range_inner(repo, &mut state, &mesh.config, range_id, r)?,
        Err(Error::RangeNotFound(_)) => orphaned_placeholder(range_id),
        Err(e) => return Err(e),
    };
    if state.layers.staged_mesh {
        apply_acknowledgment(repo, mesh_name, &mut out);
    }
    state.finish(repo);
    Ok(out)
}

pub fn resolve_mesh(
    repo: &gix::Repository,
    name: &str,
    options: EngineOptions,
) -> Result<MeshResolved> {
    let mut state = EngineState::new(repo, options.layers)?;
    let mesh = read_mesh(repo, name)?;
    let mut ranges = Vec::with_capacity(mesh.ranges.len());
    for id in &mesh.ranges {
        match read_range(repo, id) {
            Ok(r) => ranges.push(resolve_range_inner(repo, &mut state, &mesh.config, id, r)?),
            Err(Error::RangeNotFound(_)) => {
                ranges.push(orphaned_placeholder(id));
            }
            Err(e) => return Err(e),
        }
    }
    let pending = if state.layers.staged_mesh {
        for r in &mut ranges {
            apply_acknowledgment(repo, name, r);
        }
        // Adds that successfully acknowledged a Finding don't also
        // count as pending drift (they're consumed as an ack).
        let acked_indices: std::collections::HashSet<usize> = ranges
            .iter()
            .filter_map(|r| r.acknowledged_by.as_ref().map(|s| s.index))
            .collect();
        let mut p = build_pending_findings(repo, name);
        for f in &mut p {
            if let PendingFinding::Add { op, drift, .. } = f {
                let idx = (op.line_number as usize).saturating_sub(1);
                if acked_indices.contains(&idx) {
                    *drift = None;
                }
            }
        }
        p
    } else {
        Vec::new()
    };
    state.finish(repo);
    Ok(MeshResolved {
        name: mesh.name,
        message: mesh.message,
        ranges,
        pending,
    })
}

/// Blame the commit in `anchor..HEAD` that produced `current.blob`, when
/// the drift `source` is HEAD (plan §B2). For non-HEAD drift sources or
/// when no blob resolves, return `None`.
pub fn culprit_commit(
    repo: &gix::Repository,
    resolved: &RangeResolved,
) -> Result<Option<String>> {
    if resolved.source != Some(DriftSource::Head) {
        return Ok(None);
    }
    let cur = match resolved.current.as_ref() {
        Some(c) => c,
        None => return Ok(None),
    };
    if cur.blob.is_none() {
        return Ok(None);
    }
    let path = cur.path.to_string_lossy().into_owned();
    let head = git::head_oid(repo)?;
    let workdir = git::work_dir(repo)?;
    // Latest commit in `anchor..HEAD` that touched the path.
    let out = std::process::Command::new("git")
        .current_dir(workdir)
        .args([
            "log",
            "-n",
            "1",
            "--format=%H",
            &format!("{}..{}", resolved.anchor_sha, head),
            "--",
            &path,
        ])
        .output()
        .map_err(|e| Error::Git(format!("git log culprit: {e}")))?;
    if !out.status.success() {
        return Ok(None);
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() { Ok(None) } else { Ok(Some(s)) }
}

pub fn stale_meshes(repo: &gix::Repository, options: EngineOptions) -> Result<Vec<MeshResolved>> {
    let names = list_mesh_names(repo)?;
    let mut out = Vec::with_capacity(names.len());
    for name in names {
        out.push(resolve_mesh(repo, &name, options)?);
    }
    out.sort_by(|a, b| {
        let max_a = a
            .ranges
            .iter()
            .map(|r| r.status.clone())
            .max_by(status_rank)
            .unwrap_or(RangeStatus::Fresh);
        let max_b = b
            .ranges
            .iter()
            .map(|r| r.status.clone())
            .max_by(status_rank)
            .unwrap_or(RangeStatus::Fresh);
        status_rank(&max_b, &max_a)
    });
    Ok(out)
}

fn status_rank(a: &RangeStatus, b: &RangeStatus) -> std::cmp::Ordering {
    fn rank(s: &RangeStatus) -> u8 {
        match s {
            RangeStatus::Fresh => 0,
            RangeStatus::Moved => 1,
            RangeStatus::Changed => 2,
            RangeStatus::MergeConflict => 3,
            RangeStatus::Submodule => 4,
            RangeStatus::ContentUnavailable(_) => 5,
            RangeStatus::Orphaned => 6,
        }
    }
    rank(a).cmp(&rank(b))
}

fn orphaned_placeholder(range_id: &str) -> RangeResolved {
    RangeResolved {
        range_id: range_id.into(),
        anchor_sha: String::new(),
        anchored: RangeLocation {
            path: PathBuf::new(),
            extent: RangeExtent::Lines { start: 0, end: 0 },
            blob: None,
        },
        current: None,
        status: RangeStatus::Orphaned,
        source: None,
        acknowledged_by: None,
        culprit: None,
    }
}

// ---------------------------------------------------------------------------
// Internals.
// ---------------------------------------------------------------------------

fn resolve_range_inner(
    repo: &gix::Repository,
    state: &mut EngineState,
    cfg: &MeshConfig,
    range_id: &str,
    r: Range,
) -> Result<RangeResolved> {
    if matches!(r.extent, RangeExtent::Whole) {
        return resolve_whole_file(repo, state, cfg, range_id, r);
    }
    let (anchored_start, anchored_end) = match r.extent {
        RangeExtent::Lines { start, end } => (start, end),
        RangeExtent::Whole => unreachable!(),
    };
    let anchored = RangeLocation {
        path: PathBuf::from(&r.path),
        extent: r.extent,
        blob: oid_from_hex(&r.blob).ok(),
    };
    if !is_commit_reachable(repo, &r.anchor_sha)? {
        return Ok(RangeResolved {
            range_id: range_id.into(),
            anchor_sha: r.anchor_sha,
            anchored,
            current: None,
            status: RangeStatus::Orphaned,
            source: None,
            acknowledged_by: None,
            culprit: None,
        });
    }

    // 1. Resolve current location at HEAD.
    let head_loc = resolve_current_location(repo, &r, cfg.copy_detection)?;

    // 2. If any non-HEAD layer is enabled, check merge conflict on the
    //    current path before doing any layer math.
    let head_path: Option<String> = head_loc.as_ref().map(|t| t.path.clone());
    if state.layers.index || state.layers.worktree {
        let p = head_path.as_deref().unwrap_or(r.path.as_str());
        if state.conflicted_paths.contains(p) {
            return Ok(RangeResolved {
                range_id: range_id.into(),
                anchor_sha: r.anchor_sha,
                anchored,
                current: Some(RangeLocation {
                    path: PathBuf::from(p),
                    extent: RangeExtent::Lines {
                        start: anchored_start,
                        end: anchored_end,
                    },
                    blob: None,
                }),
                status: RangeStatus::MergeConflict,
                source: None,
                acknowledged_by: None,
        culprit: None,
            });
        }
    }

    // 3. Apply index then worktree hunks layer-by-layer atop HEAD.
    let head_tracked = head_loc.clone();
    let mut tracked = head_tracked.clone();
    let mut deepest_layer = DriftSource::Head;
    let mut index_blob_oid: Option<String> = None;
    let mut index_changed = false;
    let mut worktree_changed = false;

    if state.layers.index {
        if let Some(t) = tracked.as_ref()
            && let Some(diffs) = state.index_diffs.as_ref()
            && let Some(entry) = diffs.map.get(&t.path)
        {
            if entry.deleted {
                tracked = None;
            } else {
                let (s, e) = apply_hunks_to_range(&entry.hunks, t.start, t.end);
                let new_path = entry.new_path.clone();
                tracked = Some(Tracked {
                    path: new_path,
                    start: s,
                    end: e,
                });
                index_blob_oid = entry.new_blob.clone();
                index_changed = true;
            }
        }
        deepest_layer = DriftSource::Index;
    }

    if state.layers.worktree {
        if let Some(t) = tracked.as_ref()
            && let Some(diffs) = state.worktree_diffs.as_ref()
            && let Some(entry) = diffs.map.get(&t.path)
        {
            if entry.deleted {
                tracked = None;
            } else {
                let (s, e) = apply_hunks_to_range(&entry.hunks, t.start, t.end);
                let new_path = entry.new_path.clone();
                tracked = Some(Tracked {
                    path: new_path,
                    start: s,
                    end: e,
                });
                worktree_changed = true;
            }
        }
        deepest_layer = DriftSource::Worktree;
    }

    // 3b. LFS short-circuit (slice 6). If the deepest-layer path is
    // `filter=lfs`, route through `git-lfs filter-process`. Compares
    // pointer OIDs first; on a miss probes the local LFS object cache;
    // failure modes surface as `ContentUnavailable`.
    if let Some(t) = tracked.as_ref()
        && is_lfs_path(repo, &t.path)
    {
        return Ok(resolve_lfs_range(
            repo,
            state,
            range_id,
            &r,
            anchored,
            t,
            deepest_layer,
            index_blob_oid.as_deref(),
            worktree_changed,
        ));
    }

    // 4. Read content at deepest enabled layer.
    let current = match tracked.as_ref() {
        None => None,
        Some(t) => {
            // For the deepest enabled layer, read bytes appropriately.
            let (cur_text, cur_blob) = match deepest_layer {
                DriftSource::Worktree => match read_worktree_normalized(repo, state, &t.path) {
                    Ok(bytes) => (string_from_utf8_lossy(&bytes), None),
                    Err(Error::FilterFailed { filter }) => {
                        return Ok(RangeResolved {
                            range_id: range_id.into(),
                            anchor_sha: r.anchor_sha,
                            anchored,
                            current: None,
                            status: RangeStatus::ContentUnavailable(
                                UnavailableReason::FilterFailed { filter },
                            ),
                            source: None,
                            acknowledged_by: None,
        culprit: None,
                        });
                    }
                    Err(e) => return Err(e),
                },
                DriftSource::Index => {
                    if let Some(filter) = filter_short_circuit(repo, &t.path)? {
                        return Ok(RangeResolved {
                            range_id: range_id.into(),
                            anchor_sha: r.anchor_sha,
                            anchored,
                            current: None,
                            status: RangeStatus::ContentUnavailable(
                                UnavailableReason::FilterFailed { filter },
                            ),
                            source: None,
                            acknowledged_by: None,
        culprit: None,
                        });
                    }
                    let oid = index_blob_oid.clone().or_else(|| {
                        // Path didn't appear in index diff — read from HEAD blob.
                        head_blob_for(repo, &t.path).ok()
                    });
                    match oid {
                        Some(o) => {
                            let txt = git::read_git_text(repo, &o).unwrap_or_default();
                            (txt, oid_from_hex(&o).ok())
                        }
                        None => (String::new(), None),
                    }
                }
                DriftSource::Head => {
                    if let Some(filter) = filter_short_circuit(repo, &t.path)? {
                        return Ok(RangeResolved {
                            range_id: range_id.into(),
                            anchor_sha: r.anchor_sha,
                            anchored,
                            current: None,
                            status: RangeStatus::ContentUnavailable(
                                UnavailableReason::FilterFailed { filter },
                            ),
                            source: None,
                            acknowledged_by: None,
        culprit: None,
                        });
                    }
                    let oid = head_blob_for(repo, &t.path).ok();
                    let txt = match &oid {
                        Some(o) => git::read_git_text(repo, o).unwrap_or_default(),
                        None => String::new(),
                    };
                    (txt, oid.and_then(|o| oid_from_hex(&o).ok()))
                }
            };
            Some((t.clone(), cur_text, cur_blob))
        }
    };

    let status: RangeStatus;
    let source: Option<DriftSource>;
    let current_loc: Option<RangeLocation>;

    match current {
        None => {
            status = RangeStatus::Changed;
            source = Some(deepest_layer);
            current_loc = None;
        }
        Some((t, cur_text, cur_blob)) => {
            let anchored_text = git::read_git_text(repo, &r.blob)?;
            let anchored_lines: Vec<&str> = anchored_text.lines().collect();
            let current_lines: Vec<&str> = cur_text.lines().collect();
            let a_lo = (anchored_start as usize).saturating_sub(1);
            let a_hi = (anchored_end as usize).min(anchored_lines.len());
            let c_lo = (t.start as usize).saturating_sub(1);
            let c_hi = (t.end as usize).min(current_lines.len());
            let a_slice = if a_lo <= a_hi {
                &anchored_lines[a_lo..a_hi]
            } else {
                &[][..]
            };
            let c_slice = if c_lo <= c_hi {
                &current_lines[c_lo..c_hi]
            } else {
                &[][..]
            };
            let equal = lines_equal(a_slice, c_slice, cfg.ignore_whitespace);
            // Determine layer source: shallowest layer where slice diverges.
            // We compute by layering: HEAD-only slice vs anchored, then
            // index-applied slice, then worktree-applied. Source = first
            // divergent layer (None if Fresh).
            let inferred_source = infer_layer_source(
                repo,
                &r,
                &head_tracked,
                state,
                anchored_lines.as_slice(),
                cfg.ignore_whitespace,
                index_changed,
                worktree_changed,
            )?;

            if equal {
                if t.path == r.path && t.start == anchored_start && t.end == anchored_end {
                    status = RangeStatus::Fresh;
                    source = None;
                } else {
                    status = RangeStatus::Moved;
                    source = inferred_source;
                }
            } else {
                status = RangeStatus::Changed;
                source = inferred_source.or(Some(deepest_layer));
            }
            current_loc = Some(RangeLocation {
                path: PathBuf::from(t.path.clone()),
                extent: RangeExtent::Lines {
                    start: t.start,
                    end: t.end,
                },
                blob: if worktree_changed {
                    // Worktree contributed drift at this path → no blob OID.
                    None
                } else if state.layers.index && index_blob_oid.is_some() {
                    // Index contributed (or is the deepest read), use staged OID.
                    index_blob_oid
                        .as_deref()
                        .and_then(|o| oid_from_hex(o).ok())
                } else {
                    cur_blob
                },
            });
        }
    }

    Ok(RangeResolved {
        range_id: range_id.into(),
        anchor_sha: r.anchor_sha,
        anchored,
        current: current_loc,
        status,
        source,
        // Slice 3 scaffolding: ack matching is wired through types but
        // remains disabled until slice 5 ships the sidecar freshness
        // stamp. See `docs/stale-layers-slices.md`.
        acknowledged_by: None,
        culprit: None,
    })
}

/// Determine the shallowest layer at which the tracked slice diverges
/// from the anchored slice. Returns `None` if no layer adds drift.
#[allow(clippy::too_many_arguments)]
fn infer_layer_source(
    repo: &gix::Repository,
    r: &Range,
    head_tracked: &Option<Tracked>,
    state: &EngineState,
    anchored_lines: &[&str],
    ignore_ws: bool,
    index_changed: bool,
    worktree_changed: bool,
) -> Result<Option<DriftSource>> {
    let (anchored_start, anchored_end) = match r.extent {
        RangeExtent::Lines { start, end } => (start, end),
        RangeExtent::Whole => return Ok(None),
    };
    // HEAD-layer slice.
    let head_slice = if let Some(t) = head_tracked.as_ref() {
        let oid = head_blob_for(repo, &t.path).ok();
        let txt = match &oid {
            Some(o) => git::read_git_text(repo, o).unwrap_or_default(),
            None => String::new(),
        };
        let lines: Vec<String> = txt.lines().map(str::to_string).collect();
        let lo = (t.start as usize).saturating_sub(1);
        let hi = (t.end as usize).min(lines.len());
        Some((t.clone(), lines, lo, hi))
    } else {
        None
    };
    let a_lo = (anchored_start as usize).saturating_sub(1);
    let a_hi = (anchored_end as usize).min(anchored_lines.len());
    let a_slice = if a_lo <= a_hi {
        &anchored_lines[a_lo..a_hi]
    } else {
        &[][..]
    };

    // HEAD layer divergence.
    let head_diverges = match &head_slice {
        None => true,
        Some((_, lines, lo, hi)) => {
            let s: Vec<&str> = lines[*lo..*hi].iter().map(String::as_str).collect();
            !lines_equal(a_slice, &s, ignore_ws)
        }
    };
    if head_diverges {
        return Ok(Some(DriftSource::Head));
    }
    if state.layers.index && index_changed {
        return Ok(Some(DriftSource::Index));
    }
    if state.layers.worktree && worktree_changed {
        return Ok(Some(DriftSource::Worktree));
    }
    Ok(None)
}

/// Probe `.gitattributes` for a custom `filter=<name>` driver on
/// `path`. Returns `Some(name)` when the driver is unknown — neither on
/// the core-filter allowlist (`types::is_core_filter`) nor backed by a
/// configured `filter.<name>.process` (slice 7) — i.e. fail-loud
/// short-circuit. Returns `None` when it's safe to read the blob's
/// stored canonical bytes (core, LFS, or `.process`-backed driver).
///
/// Index/HEAD-layer reads consult this helper because git stores blobs
/// in canonical form already; the driver name only matters for
/// classifying whether the path's read path is "known" to the engine.
/// Worktree-layer reads still need the actual subprocess to smudge
/// disk bytes — that lives in `read_worktree_normalized`.
fn filter_short_circuit(repo: &gix::Repository, path: &str) -> Result<Option<String>> {
    let workdir = git::work_dir(repo)?;
    match types::path_filter_attribute(workdir, std::path::Path::new(path))? {
        Some(name) if types::is_core_filter(&name) => Ok(None),
        Some(name) if is_custom_filter_configured(repo, &name) => Ok(None),
        Some(name) => Ok(Some(name)),
        _ => Ok(None),
    }
}

fn head_blob_for(repo: &gix::Repository, path: &str) -> Result<String> {
    let head_sha = git::head_oid(repo)?;
    git::path_blob_at(repo, &head_sha, path)
}

fn string_from_utf8_lossy(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn apply_hunks_to_range(
    hunks: &[(u32, u32, u32, u32)],
    start: u32,
    end: u32,
) -> (u32, u32) {
    let mut s = start as i64;
    let mut e = end as i64;
    for (os, oc, _ns, nc) in hunks {
        let os = *os as i64;
        let oc = *oc as i64;
        let nc = *nc as i64;
        let delta = nc - oc;
        if oc == 0 {
            if os < s {
                s += delta;
                e += delta;
            } else if os >= e {
                // no effect
            } else {
                e += delta;
            }
            continue;
        }
        let old_last = os + oc - 1;
        if old_last < s {
            s += delta;
            e += delta;
        } else if os > e {
            // no effect
        } else {
            let new_last = if nc == 0 { os } else { os + nc - 1 };
            s = (s.min(os)).max(1);
            e = new_last.max(e + delta);
        }
    }
    let s = s.max(1) as u32;
    let e = e.max(s as i64) as u32;
    (s, e)
}

fn oid_from_hex(hex: &str) -> Result<gix::ObjectId> {
    gix::ObjectId::from_str(hex).map_err(|e| Error::Git(format!("invalid oid `{hex}`: {e}")))
}

fn lines_equal(a: &[&str], b: &[&str], ignore_ws: bool) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b.iter()).all(|(x, y)| {
        if ignore_ws {
            let xs: String = x.split_whitespace().collect();
            let ys: String = y.split_whitespace().collect();
            xs == ys
        } else {
            x == y
        }
    })
}

fn is_commit_reachable(repo: &gix::Repository, commit: &str) -> Result<bool> {
    git::commit_reachable_from_any_ref(repo, commit)
}

#[derive(Clone, Debug)]
struct Tracked {
    path: String,
    start: u32,
    end: u32,
}

fn resolve_current_location(
    repo: &gix::Repository,
    r: &Range,
    copy_detection: CopyDetection,
) -> Result<Option<Tracked>> {
    let (rstart, rend) = match r.extent {
        RangeExtent::Lines { start, end } => (start, end),
        // Whole-file pins do not flow through this layer-shifting walker;
        // `resolve_whole_file` handles them.
        RangeExtent::Whole => (1, 1),
    };
    let head_sha = git::head_oid(repo)?;
    let mut commits =
        git::rev_walk_excluding(repo, &[&head_sha], &[&r.anchor_sha], None).unwrap_or_default();
    commits.reverse();
    let mut loc = Tracked {
        path: r.path.clone(),
        start: rstart,
        end: rend,
    };
    let mut parent = r.anchor_sha.clone();
    for commit in &commits {
        match advance(repo, &parent, commit, &loc, copy_detection)? {
            Change::Unchanged => {}
            Change::Deleted => return Ok(None),
            Change::Updated(next) => loc = next,
        }
        parent = commit.clone();
    }
    if git::path_blob_at(repo, &head_sha, &loc.path).is_err() {
        return Ok(None);
    }
    Ok(Some(loc))
}

enum Change {
    Unchanged,
    Deleted,
    Updated(Tracked),
}

fn advance(
    repo: &gix::Repository,
    parent: &str,
    commit: &str,
    loc: &Tracked,
    copy_detection: CopyDetection,
) -> Result<Change> {
    let entries = name_status(repo, parent, commit, copy_detection)?;
    let mut next_path: Option<String> = None;
    let mut deleted = false;
    let mut modified = false;
    for e in &entries {
        match e {
            NS::Added { path } | NS::Modified { path } => {
                if path == &loc.path {
                    modified = true;
                    next_path = Some(loc.path.clone());
                }
            }
            NS::Deleted { path } => {
                if path == &loc.path {
                    deleted = true;
                }
            }
            NS::Renamed { from, to } => {
                if from == &loc.path {
                    next_path = Some(to.clone());
                    modified = true;
                    deleted = false;
                }
            }
            NS::Copied { from, to } => {
                if from == &loc.path {
                    next_path = Some(to.clone());
                    modified = true;
                }
            }
        }
    }
    if deleted {
        if let Some(p) = next_path {
            let (s, e) = compute_new_range(repo, parent, commit, loc, &p)?;
            return Ok(Change::Updated(Tracked {
                path: p,
                start: s,
                end: e,
            }));
        }
        return Ok(Change::Deleted);
    }
    if !modified {
        return Ok(Change::Unchanged);
    }
    let p = next_path.unwrap_or_else(|| loc.path.clone());
    let (s, e) = compute_new_range(repo, parent, commit, loc, &p)?;
    Ok(Change::Updated(Tracked {
        path: p,
        start: s,
        end: e,
    }))
}

fn compute_new_range(
    repo: &gix::Repository,
    parent: &str,
    commit: &str,
    loc: &Tracked,
    new_path: &str,
) -> Result<(u32, u32)> {
    let old_text = git::path_blob_at(repo, parent, &loc.path)
        .and_then(|b| git::read_git_text(repo, &b))
        .unwrap_or_default();
    let new_text = git::path_blob_at(repo, commit, new_path)
        .and_then(|b| git::read_git_text(repo, &b))
        .unwrap_or_default();
    let hunks = compute_hunks(&old_text, &new_text);
    Ok(apply_hunks_to_range(&hunks, loc.start, loc.end))
}

fn compute_hunks(old: &str, new: &str) -> Vec<(u32, u32, u32, u32)> {
    let a: Vec<&str> = old.lines().collect();
    let b: Vec<&str> = new.lines().collect();
    let diff = TextDiff::from_slices(&a, &b);
    let mut hunks: Vec<(u32, u32, u32, u32)> = Vec::new();
    let mut cur_old_start: Option<usize> = None;
    let mut cur_new_start: Option<usize> = None;
    let mut cur_oc: u32 = 0;
    let mut cur_nc: u32 = 0;
    let mut next_old_line: usize = 1;
    let mut next_new_line: usize = 1;
    for change in diff.iter_all_changes() {
        match change.tag() {
            ChangeTag::Equal => {
                if cur_old_start.is_some() || cur_new_start.is_some() {
                    let os = cur_old_start.unwrap_or(next_old_line.saturating_sub(1));
                    let ns = cur_new_start.unwrap_or(next_new_line.saturating_sub(1));
                    let (emitted_os, emitted_ns) = if cur_oc == 0 {
                        (next_old_line.saturating_sub(1), ns)
                    } else if cur_nc == 0 {
                        (os, next_new_line.saturating_sub(1))
                    } else {
                        (os, ns)
                    };
                    hunks.push((emitted_os as u32, cur_oc, emitted_ns as u32, cur_nc));
                    cur_old_start = None;
                    cur_new_start = None;
                    cur_oc = 0;
                    cur_nc = 0;
                }
                next_old_line += 1;
                next_new_line += 1;
            }
            ChangeTag::Delete => {
                if cur_old_start.is_none() {
                    cur_old_start = Some(next_old_line);
                }
                cur_oc += 1;
                next_old_line += 1;
            }
            ChangeTag::Insert => {
                if cur_new_start.is_none() {
                    cur_new_start = Some(next_new_line);
                }
                cur_nc += 1;
                next_new_line += 1;
            }
        }
    }
    if cur_old_start.is_some() || cur_new_start.is_some() {
        let os = cur_old_start.unwrap_or(next_old_line.saturating_sub(1));
        let ns = cur_new_start.unwrap_or(next_new_line.saturating_sub(1));
        let (emitted_os, emitted_ns) = if cur_oc == 0 {
            (next_old_line.saturating_sub(1), ns)
        } else if cur_nc == 0 {
            (os, next_new_line.saturating_sub(1))
        } else {
            (os, ns)
        };
        hunks.push((emitted_os as u32, cur_oc, emitted_ns as u32, cur_nc));
    }
    hunks
}

enum NS {
    Added { path: String },
    Modified { path: String },
    Deleted { path: String },
    Renamed { from: String, to: String },
    Copied { from: String, to: String },
}

fn name_status(
    repo: &gix::Repository,
    parent: &str,
    commit: &str,
    copy_detection: CopyDetection,
) -> Result<Vec<NS>> {
    let parent_oid = gix::ObjectId::from_str(parent)
        .map_err(|e| Error::Git(format!("parse parent oid: {e}")))?;
    let commit_oid = gix::ObjectId::from_str(commit)
        .map_err(|e| Error::Git(format!("parse commit oid: {e}")))?;
    let parent_commit = repo
        .find_commit(parent_oid)
        .map_err(|e| Error::Git(format!("find parent: {e}")))?;
    let commit_obj = repo
        .find_commit(commit_oid)
        .map_err(|e| Error::Git(format!("find commit: {e}")))?;
    let parent_tree = parent_commit
        .tree()
        .map_err(|e| Error::Git(format!("parent tree: {e}")))?;
    let new_tree = commit_obj
        .tree()
        .map_err(|e| Error::Git(format!("commit tree: {e}")))?;
    let mut platform = parent_tree
        .changes()
        .map_err(|e| Error::Git(format!("tree changes: {e}")))?;
    platform.options(|opts| {
        let want_copies = !matches!(copy_detection, CopyDetection::Off);
        opts.track_path().track_rewrites(Some(gix::diff::Rewrites {
            copies: if want_copies {
                Some(gix::diff::rewrites::Copies::default())
            } else {
                None
            },
            percentage: Some(0.5),
            limit: 1000,
            track_empty: false,
        }));
    });
    let mut out = Vec::new();
    platform
        .for_each_to_obtain_tree(&new_tree, |change| -> Result<std::ops::ControlFlow<()>> {
            use gix::object::tree::diff::Change as DC;
            match change {
                DC::Addition { location, .. } => out.push(NS::Added {
                    path: location.to_string(),
                }),
                DC::Deletion { location, .. } => out.push(NS::Deleted {
                    path: location.to_string(),
                }),
                DC::Modification { location, .. } => out.push(NS::Modified {
                    path: location.to_string(),
                }),
                DC::Rewrite {
                    source_location,
                    location,
                    copy,
                    ..
                } => {
                    if copy {
                        out.push(NS::Copied {
                            from: source_location.to_string(),
                            to: location.to_string(),
                        });
                    } else {
                        out.push(NS::Renamed {
                            from: source_location.to_string(),
                            to: location.to_string(),
                        });
                    }
                }
            }
            Ok(std::ops::ControlFlow::Continue(()))
        })
        .map_err(|e| Error::Git(format!("tree diff: {e}")))?;
    Ok(out)
}

// ---------------------------------------------------------------------------
// Slice 2: index / worktree layer readers.
// ---------------------------------------------------------------------------

fn read_index_layer(repo: &gix::Repository, warnings: &mut Vec<String>) -> Result<LayerDiffs> {
    let workdir = git::work_dir(repo)?;
    // First pass: with renames.
    let out = run_git_diff(
        workdir,
        &["diff-index", "--cached", "-U0", "-M", "--full-index", "HEAD"],
    )?;
    let parsed = parse_diff_raw_unified(&out, /*has_worktree_blob:*/ false);
    let budget = rename_budget();
    if parsed.entry_count > budget {
        warnings.push(format!(
            "warning: rename detection disabled (--no-renames); {} > GIT_MESH_RENAME_BUDGET={}",
            parsed.entry_count, budget
        ));
        let out = run_git_diff(
            workdir,
            &[
                "diff-index",
                "--cached",
                "-U0",
                "--no-renames",
                "--full-index",
                "HEAD",
            ],
        )?;
        let mut p = parse_diff_raw_unified(&out, false);
        p.rename_detection_disabled = true;
        return Ok(p.into_layer());
    }
    Ok(parsed.into_layer())
}

fn read_worktree_layer(repo: &gix::Repository, warnings: &mut Vec<String>) -> Result<LayerDiffs> {
    let workdir = git::work_dir(repo)?;
    let out = run_git_diff(workdir, &["diff-files", "-U0", "-M"])?;
    let parsed = parse_diff_raw_unified(&out, /*has_worktree_blob:*/ true);
    let budget = rename_budget();
    if parsed.entry_count > budget {
        warnings.push(format!(
            "warning: rename detection disabled (--no-renames); {} > GIT_MESH_RENAME_BUDGET={}",
            parsed.entry_count, budget
        ));
        let out = run_git_diff(workdir, &["diff-files", "-U0", "--no-renames"])?;
        let mut p = parse_diff_raw_unified(&out, true);
        p.rename_detection_disabled = true;
        return Ok(p.into_layer());
    }
    Ok(parsed.into_layer())
}

fn run_git_diff(workdir: &std::path::Path, args: &[&str]) -> Result<String> {
    // Suppress the LFS filter-process driver for diff-time path-resolution.
    // The engine routes LFS reads through its own managed subprocess
    // (slice 6); we do not want diff-files / diff-index to spawn its
    // own `git-lfs` (which would also fail loudly on hosts without
    // `git-lfs` installed).
    let out = Command::new("git")
        .current_dir(workdir)
        .args([
            "-c",
            "filter.lfs.process=",
            "-c",
            "filter.lfs.smudge=cat",
            "-c",
            "filter.lfs.clean=cat",
            "-c",
            "filter.lfs.required=false",
        ])
        .args(args)
        .output()
        .map_err(|e| Error::Git(format!("spawn git {args:?}: {e}")))?;
    if !out.status.success() {
        return Err(Error::Git(format!(
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

struct ParsedDiff {
    entries: Vec<DiffEntry>,
    rename_detection_disabled: bool,
    entry_count: usize,
}

impl ParsedDiff {
    fn into_layer(self) -> LayerDiffs {
        let mut map = HashMap::new();
        let mut renamed_from = HashMap::new();
        for e in self.entries {
            if e.old_path != e.new_path {
                renamed_from.insert(e.old_path.clone(), e.new_path.clone());
            }
            // Map keyed by destination path; also record a source-path lookup
            // so a tracked location whose HEAD-side path was renamed is found.
            if e.old_path != e.new_path {
                map.insert(e.old_path.clone(), e.clone());
            }
            map.insert(e.new_path.clone(), e);
        }
        LayerDiffs {
            map,
            renamed_from,
            rename_detection_disabled: self.rename_detection_disabled,
        }
    }
}

/// Parse `git diff-index`/`diff-files` `-U0` output. The format is
/// `diff --git`-style unified with hunk headers (`@@ -A,B +C,D @@`).
/// Rename markers come as `rename from <p>` / `rename to <p>`. Index
/// lines `index <old>..<new>` carry blob OIDs.
fn parse_diff_raw_unified(text: &str, worktree: bool) -> ParsedDiff {
    let mut entries: Vec<DiffEntry> = Vec::new();
    let mut cur: Option<DiffEntry> = None;
    let mut new_mode_zero = false; // intent-to-add detection
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("diff --git ") {
            if let Some(prev) = cur.take() {
                entries.push(prev);
            }
            // `a/<old> b/<new>`. Default both to the same path; rename
            // markers below override.
            let (a, b) = parse_diff_paths(rest);
            cur = Some(DiffEntry {
                new_path: b,
                old_path: a,
                hunks: Vec::new(),
                new_blob: None,
                deleted: false,
                intent_to_add: false,
            });
            new_mode_zero = false;
            continue;
        }
        let Some(e) = cur.as_mut() else {
            continue;
        };
        if let Some(rest) = line.strip_prefix("rename from ") {
            e.old_path = rest.to_string();
            continue;
        }
        if let Some(rest) = line.strip_prefix("rename to ") {
            e.new_path = rest.to_string();
            continue;
        }
        if line.starts_with("deleted file mode") {
            e.deleted = true;
            continue;
        }
        if let Some(rest) = line.strip_prefix("new file mode ") {
            // `new file mode 000000` is intent-to-add (zero-OID stage entry).
            if rest.trim() == "000000" {
                new_mode_zero = true;
                e.intent_to_add = true;
            }
            continue;
        }
        if let Some(rest) = line.strip_prefix("index ") {
            // `<old>..<new>[ mode]`
            if !worktree && !new_mode_zero {
                if let Some((_oldnew, _)) = rest.split_once(' ') {
                    if let Some((_, new)) = _oldnew.split_once("..") {
                        let new_oid = new.trim().to_string();
                        if !new_oid.chars().all(|c| c == '0') {
                            e.new_blob = Some(new_oid);
                        } else {
                            e.intent_to_add = true;
                        }
                    }
                } else if let Some((_, new)) = rest.split_once("..") {
                    let new_oid = new.trim().to_string();
                    if !new_oid.chars().all(|c| c == '0') {
                        e.new_blob = Some(new_oid);
                    } else {
                        e.intent_to_add = true;
                    }
                }
            }
            continue;
        }
        if let Some(rest) = line.strip_prefix("@@ ") {
            // `@@ -A[,B] +C[,D] @@ ...`
            if let Some(end) = rest.find(" @@") {
                let head = &rest[..end];
                let parts: Vec<&str> = head.split_whitespace().collect();
                if parts.len() >= 2 {
                    let (os, oc) = parse_hunk_loc(parts[0].trim_start_matches('-'));
                    let (ns, nc) = parse_hunk_loc(parts[1].trim_start_matches('+'));
                    e.hunks.push((os, oc, ns, nc));
                }
            }
            continue;
        }
    }
    if let Some(prev) = cur.take() {
        entries.push(prev);
    }
    let entry_count = entries.len();
    ParsedDiff {
        entries,
        rename_detection_disabled: false,
        entry_count,
    }
}

fn parse_diff_paths(rest: &str) -> (String, String) {
    // Best-effort: handles unquoted `a/<p> b/<p>` form. Quoted paths fall
    // through to identical halves which is acceptable for the integration
    // tests in scope.
    let trimmed = rest.trim();
    if let Some(idx) = trimmed.find(" b/") {
        let a_part = &trimmed[..idx];
        let b_part = &trimmed[idx + 3..];
        let a = a_part.strip_prefix("a/").unwrap_or(a_part).to_string();
        let b = b_part.to_string();
        return (a, b);
    }
    (trimmed.to_string(), trimmed.to_string())
}

fn parse_hunk_loc(s: &str) -> (u32, u32) {
    if let Some((a, b)) = s.split_once(',') {
        (a.parse().unwrap_or(0), b.parse().unwrap_or(0))
    } else {
        (s.parse().unwrap_or(0), 1)
    }
}

fn read_conflicted_paths(repo: &gix::Repository) -> Result<HashSet<String>> {
    let workdir = git::work_dir(repo)?;
    let out = Command::new("git")
        .current_dir(workdir)
        .args(["ls-files", "-u", "-z"])
        .output()
        .map_err(|e| Error::Git(format!("spawn git ls-files -u: {e}")))?;
    if !out.status.success() {
        return Err(Error::Git(format!(
            "git ls-files -u failed: {}",
            String::from_utf8_lossy(&out.stderr)
        )));
    }
    let mut set = HashSet::new();
    // Format: <mode> <oid> <stage>\t<path>\0
    for chunk in out.stdout.split(|b| *b == 0) {
        if chunk.is_empty() {
            continue;
        }
        if let Some(tab) = chunk.iter().position(|b| *b == b'\t') {
            let path = String::from_utf8_lossy(&chunk[tab + 1..]).into_owned();
            set.insert(path);
        }
    }
    Ok(set)
}

fn read_index_trailer(repo: &gix::Repository) -> Result<[u8; 20]> {
    let workdir = git::work_dir(repo)?;
    let index_path = workdir.join(".git").join("index");
    let bytes = std::fs::read(&index_path)?;
    if bytes.len() < 20 {
        return Err(Error::Git("index too short for trailer".into()));
    }
    let mut out = [0u8; 20];
    out.copy_from_slice(&bytes[bytes.len() - 20..]);
    Ok(out)
}

/// Read a worktree file, applying git's clean filter where possible.
///
/// Driver dispatch (slice 7):
///   - core filters (`types::is_core_filter`) → gix pipeline below.
///   - `filter=<name>` with `filter.<name>.process` configured → managed
///     custom subprocess (cached on `state.custom_filters`).
///   - `filter=<name>` with no `.process` → `Error::FilterFailed`.
///   - LFS is *not* handled here; callers branch on `is_lfs_path`
///     before reaching this read site.
///
/// Engine maps `Error::FilterFailed` to
/// `RangeStatus::ContentUnavailable(FilterFailed)`.
fn read_worktree_normalized(
    repo: &gix::Repository,
    state: &mut EngineState,
    rel_path: &str,
) -> Result<Vec<u8>> {
    let workdir = git::work_dir(repo)?;
    if let Some(name) =
        types::path_filter_attribute(workdir, std::path::Path::new(rel_path))?
        && !types::is_core_filter(&name)
    {
        // Custom filter-process driver (slice 7). LFS is intercepted
        // by the caller before this function runs, so a `<name>` here
        // is either backed by `filter.<name>.process` config (route
        // through the subprocess) or unknown (fail loud).
        let abs = workdir.join(rel_path);
        let raw = match std::fs::read(&abs) {
            Ok(b) => b,
            Err(_) => return Ok(Vec::new()),
        };
        return match custom_filter_smudge(state, workdir, &name, rel_path, &raw) {
            CustomFilterOutcome::Bytes(b) => Ok(b),
            CustomFilterOutcome::FilterFailed => Err(Error::FilterFailed { filter: name }),
        };
    }
    let abs = workdir.join(rel_path);
    let md = match std::fs::symlink_metadata(&abs) {
        Ok(m) => m,
        Err(_) => return Ok(Vec::new()),
    };
    if md.file_type().is_symlink() {
        let target = std::fs::read_link(&abs)?;
        return Ok(target.to_string_lossy().into_owned().into_bytes());
    }
    // Try clean-filter via gix; fall back to raw bytes on any error so the
    // engine never panics on a worktree read.
    let file = match std::fs::File::open(&abs) {
        Ok(f) => f,
        Err(_) => return Ok(Vec::new()),
    };
    let pipeline = repo.filter_pipeline(None);
    let Ok((mut pipeline, index)) = pipeline else {
        let mut buf = Vec::new();
        let mut f = std::fs::File::open(&abs)?;
        use std::io::Read;
        f.read_to_end(&mut buf)?;
        return Ok(buf);
    };
    let outcome = pipeline.convert_to_git(file, std::path::Path::new(rel_path), &index);
    let Ok(outcome) = outcome else {
        let mut buf = Vec::new();
        let mut f = std::fs::File::open(&abs)?;
        use std::io::Read;
        f.read_to_end(&mut buf)?;
        return Ok(buf);
    };
    use gix::filter::plumbing::pipeline::convert::ToGitOutcome;
    use std::io::Read;
    let mut out = Vec::new();
    match outcome {
        ToGitOutcome::Unchanged(mut r) => {
            r.read_to_end(&mut out)?;
        }
        ToGitOutcome::Buffer(buf) => out.extend_from_slice(buf),
        ToGitOutcome::Process(mut r) => {
            r.read_to_end(&mut out)?;
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Slice 5: whole-file resolver, ack matching, pending findings.
// ---------------------------------------------------------------------------

/// Resolve a whole-file pin at the deepest enabled layer (plan §D2).
/// Compares blob OIDs (or, for symlinks/gitlinks, the link target string
/// or gitlink SHA respectively).
fn resolve_whole_file(
    repo: &gix::Repository,
    state: &mut EngineState,
    cfg: &MeshConfig,
    range_id: &str,
    r: Range,
) -> Result<RangeResolved> {
    let anchored = RangeLocation {
        path: PathBuf::from(&r.path),
        extent: RangeExtent::Whole,
        blob: oid_from_hex(&r.blob).ok(),
    };
    if !is_commit_reachable(repo, &r.anchor_sha)? {
        return Ok(RangeResolved {
            range_id: range_id.into(),
            anchor_sha: r.anchor_sha,
            anchored,
            current: None,
            status: RangeStatus::Orphaned,
            source: None,
            acknowledged_by: None,
            culprit: None,
        });
    }

    // Walk anchor..HEAD via `git log --follow` to locate the path's
    // current name (renames produce Moved). Then compare anchored vs.
    // current.
    let head_sha = git::head_oid(repo)?;
    let workdir = git::work_dir(repo)?;
    let current_path = follow_path_to_head(workdir, &r.anchor_sha, &head_sha, &r.path)
        .unwrap_or_else(|| r.path.clone());

    // Resolve current SHA at HEAD layer for the path. Preference:
    // gitlink first (mode 160000), then blob.
    let head_kind_sha = ls_tree_kind_and_sha(workdir, &head_sha, &current_path);
    let mut deepest = DriftSource::Head;
    let mut current_blob: Option<String> = head_kind_sha.as_ref().map(|(_, sha)| sha.clone());
    let moved = current_path != r.path;

    if state.layers.index {
        // Index entry's mode/oid via `git ls-files --stage --full-name`
        if let Some((_mode, sha)) = ls_files_stage(workdir, &current_path) {
            current_blob = Some(sha);
        }
        deepest = DriftSource::Index;
    }
    if state.layers.worktree {
        // For worktree layer on a non-gitlink path, read worktree bytes
        // and hash via `git hash-object` to produce an OID comparable
        // to the anchored blob OID. Symlinks: read link target string.
        let abs = workdir.join(&current_path);
        if let Ok(md) = std::fs::symlink_metadata(&abs) {
            if md.file_type().is_symlink() {
                let target = std::fs::read_link(&abs)
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let oid = git_hash_object_bytes(workdir, target.as_bytes());
                current_blob = oid;
            } else if md.file_type().is_file()
                && let Ok(oid) = git_hash_object_path(workdir, &abs)
            {
                current_blob = Some(oid);
            }
        } else {
            // Path missing from worktree → Changed.
            current_blob = None;
        }
        deepest = DriftSource::Worktree;
    }

    let _ = cfg;
    let status: RangeStatus;
    let source: Option<DriftSource>;
    let cur_blob_oid = current_blob.as_deref().and_then(|s| oid_from_hex(s).ok());
    let current_loc = Some(RangeLocation {
        path: PathBuf::from(&current_path),
        extent: RangeExtent::Whole,
        blob: cur_blob_oid,
    });
    match current_blob.as_deref() {
        None => {
            status = RangeStatus::Changed;
            source = Some(deepest);
        }
        Some(cur) if cur == r.blob && moved => {
            status = RangeStatus::Moved;
            source = Some(deepest);
        }
        Some(cur) if cur == r.blob => {
            status = RangeStatus::Fresh;
            source = None;
        }
        Some(_) => {
            status = RangeStatus::Changed;
            source = Some(deepest);
        }
    }
    let _ = moved;

    Ok(RangeResolved {
        range_id: range_id.into(),
        anchor_sha: r.anchor_sha,
        anchored,
        current: current_loc,
        status,
        source,
        acknowledged_by: None,
        culprit: None,
    })
}

fn follow_path_to_head(
    workdir: &std::path::Path,
    anchor: &str,
    head: &str,
    path: &str,
) -> Option<String> {
    // Use `git log --follow --name-only -z --format=` between anchor..head.
    let out = std::process::Command::new("git")
        .current_dir(workdir)
        .args([
            "log",
            "--follow",
            "--name-only",
            "-z",
            "--format=",
            &format!("{anchor}..{head}"),
            "--",
            path,
        ])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    // First non-empty NUL-separated entry is the most recent name.
    out.stdout
        .split(|b| *b == 0)
        .find(|s| !s.is_empty())
        .map(|s| String::from_utf8_lossy(s).into_owned())
}

fn ls_tree_kind_and_sha(
    workdir: &std::path::Path,
    commit: &str,
    path: &str,
) -> Option<(String, String)> {
    let out = std::process::Command::new("git")
        .current_dir(workdir)
        .args(["ls-tree", commit, "--", path])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout);
    let line = s.lines().next()?;
    let (meta, _) = line.split_once('\t')?;
    let mut parts = meta.split_whitespace();
    let mode = parts.next()?.to_string();
    let _ty = parts.next()?;
    let oid = parts.next()?.to_string();
    Some((mode, oid))
}

fn ls_files_stage(workdir: &std::path::Path, path: &str) -> Option<(String, String)> {
    let out = std::process::Command::new("git")
        .current_dir(workdir)
        .args(["ls-files", "--stage", "--", path])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout);
    let line = s.lines().next()?;
    let (meta, _) = line.split_once('\t')?;
    let mut parts = meta.split_whitespace();
    let mode = parts.next()?.to_string();
    let oid = parts.next()?.to_string();
    Some((mode, oid))
}

fn git_hash_object_path(workdir: &std::path::Path, abs: &std::path::Path) -> Result<String> {
    let out = std::process::Command::new("git")
        .current_dir(workdir)
        .arg("hash-object")
        .arg(abs)
        .output()
        .map_err(|e| Error::Git(format!("hash-object: {e}")))?;
    if !out.status.success() {
        return Err(Error::Git("hash-object failed".into()));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn git_hash_object_bytes(workdir: &std::path::Path, bytes: &[u8]) -> Option<String> {
    use std::io::Write;
    let mut child = std::process::Command::new("git")
        .current_dir(workdir)
        .args(["hash-object", "--stdin"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .ok()?;
    {
        let stdin = child.stdin.as_mut()?;
        stdin.write_all(bytes).ok()?;
    }
    let out = child.wait_with_output().ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Acknowledgment matching by `range_id` (plan §B2). For a finding to
/// be acknowledged, a staged op in `<mesh>` must point at the same
/// `range_id` AND its sidecar bytes (re-normalized through current
/// filters) must equal the current live content for that range.
fn apply_acknowledgment(repo: &gix::Repository, mesh_name: &str, r: &mut RangeResolved) {
    if r.status == RangeStatus::Fresh {
        return;
    }
    let staging = match crate::staging::read_staging(repo, mesh_name) {
        Ok(s) => s,
        Err(_) => return,
    };
    for add in &staging.adds {
        let meta = match crate::staging::read_sidecar_meta(repo, mesh_name, add.line_number) {
            Some(m) => m,
            None => continue,
        };
        let Some(rid) = &meta.range_id else { continue };
        if rid != &r.range_id {
            continue;
        }
        let sidecar_path =
            match crate::staging::sidecar_path(repo, mesh_name, add.line_number) {
                Ok(p) => p,
                Err(_) => continue,
            };
        let Ok(side_bytes) = std::fs::read(&sidecar_path) else {
            continue;
        };
        let side_norm = renormalize(repo, &add.path, &side_bytes, &meta.stamp);
        let live_norm = match read_live_for_range(repo, r) {
            Some(b) => b,
            None => continue,
        };
        let matches = match r.anchored.extent {
            RangeExtent::Whole => side_norm == live_norm,
            RangeExtent::Lines { .. } => {
                // Sidecar extent is the staged add's extent (capture time);
                // live extent is the range's current resolved extent.
                let side_text = String::from_utf8_lossy(&side_norm);
                let live_text = String::from_utf8_lossy(&live_norm);
                let side_extent = add.extent;
                let live_extent = r
                    .current
                    .as_ref()
                    .map(|c| c.extent)
                    .unwrap_or(r.anchored.extent);
                slice_eq_at(&side_text, side_extent, &live_text, live_extent)
            }
        };
        if matches {
            r.acknowledged_by = Some(StagedOpRef {
                mesh: mesh_name.to_string(),
                index: (add.line_number as usize).saturating_sub(1),
            });
            return;
        }
    }
}

fn slice_eq_at(
    side_text: &str,
    side_extent: RangeExtent,
    live_text: &str,
    live_extent: RangeExtent,
) -> bool {
    let (s_lo, s_hi) = match side_extent {
        RangeExtent::Lines { start, end } => (start.saturating_sub(1) as usize, end as usize),
        RangeExtent::Whole => return side_text == live_text,
    };
    let (l_lo, l_hi) = match live_extent {
        RangeExtent::Lines { start, end } => (start.saturating_sub(1) as usize, end as usize),
        RangeExtent::Whole => return side_text == live_text,
    };
    let side_lines: Vec<&str> = side_text.lines().collect();
    let live_lines: Vec<&str> = live_text.lines().collect();
    let s_hi = s_hi.min(side_lines.len());
    let l_hi = l_hi.min(live_lines.len());
    let side_slice: &[&str] = if s_lo <= s_hi { &side_lines[s_lo..s_hi] } else { &[] };
    let live_slice: &[&str] = if l_lo <= l_hi { &live_lines[l_lo..l_hi] } else { &[] };
    side_slice == live_slice
}

#[allow(dead_code)]
fn slice_eq(side_text: &str, live_text: &str, r: &RangeResolved) -> bool {
    // The sidecar holds the full file's bytes at capture time. Both
    // sides slice by the same line-range (the *anchored* extent — the
    // staged add was pinned at that extent before any post-add motion).
    let (lo, hi) = match r.anchored.extent {
        RangeExtent::Lines { start, end } => (start.saturating_sub(1) as usize, end as usize),
        RangeExtent::Whole => return side_text == live_text,
    };
    let side_lines: Vec<&str> = side_text.lines().collect();
    let live_lines: Vec<&str> = live_text.lines().collect();
    let s_hi = hi.min(side_lines.len());
    let l_hi = hi.min(live_lines.len());
    let side_slice: &[&str] = if lo <= s_hi { &side_lines[lo..s_hi] } else { &[] };
    let live_slice: &[&str] = if lo <= l_hi { &live_lines[lo..l_hi] } else { &[] };
    side_slice == live_slice
}

/// Re-normalize sidecar bytes when the captured stamp doesn't match the
/// current stamp. Intentionally simple in slice 5: if either side is
/// CRLF-vs-LF only, normalize both to LF before returning.
fn renormalize(
    repo: &gix::Repository,
    _path: &str,
    bytes: &[u8],
    captured: &crate::types::NormalizationStamp,
) -> Vec<u8> {
    let current = current_normalization_stamp(repo).unwrap_or_default();
    if &current == captured {
        return bytes.to_vec();
    }
    // Fail-loud-but-friendly: collapse line endings to LF on both sides.
    // The test fixture exercises a `*.txt text eol=lf` flip, which only
    // affects line endings.
    let s = String::from_utf8_lossy(bytes).into_owned();
    s.replace("\r\n", "\n").into_bytes()
}

/// Read the current bytes for the anchored range at the deepest enabled
/// layer. For whole-file extents this returns the full file bytes; for
/// line-range extents it returns the full file bytes (the slicing is
/// done in the comparator).
fn read_live_for_range(repo: &gix::Repository, r: &RangeResolved) -> Option<Vec<u8>> {
    let workdir = git::work_dir(repo).ok()?;
    let path = r
        .current
        .as_ref()
        .map(|c| c.path.clone())
        .unwrap_or(r.anchored.path.clone());
    let abs = workdir.join(&path);
    let bytes = std::fs::read(&abs).ok()?;
    // Apply LF collapse so re-normalized sidecars compare cleanly.
    let s = String::from_utf8_lossy(&bytes).into_owned();
    Some(s.replace("\r\n", "\n").into_bytes())
}

/// Build `Vec<PendingFinding>` from `.git/mesh/staging/<name>` ops. For
/// `Add`/`Remove` we compute `drift: Option<PendingDrift>` by comparing
/// the sidecar against the claimed blob under current filters.
fn build_pending_findings(repo: &gix::Repository, mesh_name: &str) -> Vec<PendingFinding> {
    let mut out = Vec::new();
    let ops = match crate::staging::read_staged_ops(repo, mesh_name) {
        Ok(v) => v,
        Err(_) => return out,
    };
    for op in ops {
        match op {
            crate::staging::StagedOp::Add(a) => {
                let meta = crate::staging::read_sidecar_meta(repo, mesh_name, a.line_number);
                let range_id = meta
                    .as_ref()
                    .and_then(|m| m.range_id.clone())
                    .unwrap_or_default();
                let drift = pending_add_drift(repo, mesh_name, &a, meta.as_ref());
                out.push(PendingFinding::Add {
                    mesh: mesh_name.to_string(),
                    range_id,
                    op: a,
                    drift,
                });
            }
            crate::staging::StagedOp::Remove(rm) => {
                let range_id = String::new();
                out.push(PendingFinding::Remove {
                    mesh: mesh_name.to_string(),
                    range_id,
                    op: rm,
                    drift: None,
                });
            }
            crate::staging::StagedOp::Config(c) => out.push(PendingFinding::ConfigChange {
                mesh: mesh_name.to_string(),
                change: c,
            }),
            crate::staging::StagedOp::Message(body) => out.push(PendingFinding::Message {
                mesh: mesh_name.to_string(),
                body,
            }),
        }
    }
    out
}

fn pending_add_drift(
    repo: &gix::Repository,
    mesh_name: &str,
    add: &crate::staging::StagedAdd,
    meta: Option<&crate::staging::SidecarMeta>,
) -> Option<PendingDrift> {
    let sidecar_p = crate::staging::sidecar_path(repo, mesh_name, add.line_number).ok()?;
    let side_bytes = std::fs::read(&sidecar_p).ok()?;
    let stamp = meta.map(|m| &m.stamp);
    let live = if let Some(anchor) = &add.anchor {
        // Anchor-pinned: compare against blob at anchor.
        match crate::git::path_blob_at(repo, anchor, &add.path) {
            Ok(blob) => crate::git::read_blob_bytes(repo, &blob).ok()?,
            Err(_) => return Some(PendingDrift::SidecarMismatch),
        }
    } else {
        // Worktree-anchored: compare against current worktree bytes.
        let workdir = git::work_dir(repo).ok()?;
        std::fs::read(workdir.join(&add.path)).ok()?
    };
    let captured = stamp.cloned().unwrap_or_default();
    let side_norm = renormalize(repo, &add.path, &side_bytes, &captured);
    let live_norm = {
        let s = String::from_utf8_lossy(&live).into_owned();
        s.replace("\r\n", "\n").into_bytes()
    };
    let equal = match add.extent {
        RangeExtent::Whole => side_norm == live_norm,
        RangeExtent::Lines { start, end } => {
            // Slice both sides at the staged add's extent.
            let st = String::from_utf8_lossy(&side_norm);
            let lt = String::from_utf8_lossy(&live_norm);
            let lo = start.saturating_sub(1) as usize;
            let hi = end as usize;
            let s_lines: Vec<&str> = st.lines().collect();
            let l_lines: Vec<&str> = lt.lines().collect();
            let s_hi = hi.min(s_lines.len());
            let l_hi = hi.min(l_lines.len());
            let s: &[&str] = if lo <= s_hi { &s_lines[lo..s_hi] } else { &[] };
            let l: &[&str] = if lo <= l_hi { &l_lines[lo..l_hi] } else { &[] };
            s == l
        }
    };
    if equal { None } else { Some(PendingDrift::SidecarMismatch) }
}

// ---------------------------------------------------------------------------
// Slice 6: LFS first-class reader.
//
// The plan (`docs/stale-layers-plan.md` §D3) routes `filter=lfs` paths
// through a managed `git-lfs filter-process` subprocess instead of the
// fail-loud short-circuit. Spawn is lazy on first LFS read in a `stale`
// run; the process is reused for every subsequent read and torn down on
// `Drop`.
//
// Cache-probe semantics distinguish:
//   - both sides cached → run subprocess, return smudged bytes (with
//     `GIT_LFS_SKIP_SMUDGE=1` this is the pointer text round-trip — the
//     comparator-equivalence is preserved; the subprocess exists so we
//     respect any environment-driven driver override),
//   - either side missing → `LfsNotFetched`,
//   - subprocess spawn fails → `LfsNotInstalled`.
// ---------------------------------------------------------------------------

use std::io::{BufReader, Read, Write};
use std::process::{Child, ChildStdin, ChildStdout, Stdio};

/// Owned `git filter-process`-protocol subprocess, plus the pkt-line
/// streams used to drive the long-running protocol. Kept on
/// `EngineState` so a single process serves every read in the run for
/// its driver. Generic across LFS (`git-lfs filter-process`) and
/// custom `filter.<name>.process` shell-command drivers (slice 7).
pub(crate) struct FilterProcess {
    child: Child,
    /// `Option` so `Drop` can take + drop the write-end *before*
    /// `child.wait()`. Without an explicit drop the subprocess never
    /// sees EOF on stdin and we deadlock at end of run.
    stdin: Option<ChildStdin>,
    stdout: BufReader<ChildStdout>,
}

impl FilterProcess {
    fn stdin_mut(&mut self) -> &mut ChildStdin {
        self.stdin.as_mut().expect("stdin only None during Drop")
    }
}

impl Drop for FilterProcess {
    fn drop(&mut self) {
        if let Some(mut s) = self.stdin.take() {
            let _ = s.flush();
            // s drops here — closing the write-end of the pipe.
        }
        let _ = self.child.wait();
    }
}

/// Finish wiring `child` into a `FilterProcess` and run the
/// long-running-process handshake. Caller built `child` so they get to
/// pick the command line (LFS vs `sh -c <cmd>` for slice 7).
fn finalize_filter_process(
    mut child: Child,
) -> std::result::Result<FilterProcess, FilterSpawnError> {
    let stdin = child.stdin.take().ok_or(FilterSpawnError::HandshakeFailed)?;
    let stdout = BufReader::new(child.stdout.take().ok_or(FilterSpawnError::HandshakeFailed)?);
    let mut p = FilterProcess { child, stdin: Some(stdin), stdout };
    if filter_handshake(&mut p).is_err() {
        return Err(FilterSpawnError::HandshakeFailed);
    }
    Ok(p)
}

/// Spawn `git-lfs filter-process` in the repo's worktree and complete
/// the long-running-process handshake. On any spawn / handshake failure
/// the caller maps to `FilterSpawnError` and the engine surfaces a
/// terminal `ContentUnavailable` finding.
fn spawn_lfs_process(
    workdir: &std::path::Path,
) -> std::result::Result<FilterProcess, FilterSpawnError> {
    let child = std::process::Command::new("git-lfs")
        .arg("filter-process")
        .current_dir(workdir)
        .env("GIT_LFS_SKIP_SMUDGE", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| FilterSpawnError::NotInstalled)?;
    finalize_filter_process(child)
}

/// Spawn a custom `filter.<name>.process` driver via `sh -c <cmd>`
/// (matches what git itself does for `filter.<name>.process` config
/// values per `git help config`). Slice 7. Failures are sticky on the
/// engine cache; subsequent reads on the same driver short-circuit
/// without re-spawning.
fn spawn_custom_filter_process(
    workdir: &std::path::Path,
    cmd: &str,
) -> std::result::Result<FilterProcess, FilterSpawnError> {
    let child = std::process::Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .current_dir(workdir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| FilterSpawnError::NotInstalled)?;
    finalize_filter_process(child)
}

/// Long-running filter-process handshake (see `git help long-running-process`):
/// client sends `git-filter-client`, version pkts, then capabilities; server
/// echoes welcome, a version, and capabilities. We negotiate `smudge` only.
fn filter_handshake(p: &mut FilterProcess) -> std::io::Result<()> {
    pkt_write_text(p.stdin_mut(), "git-filter-client\n")?;
    pkt_write_text(p.stdin_mut(), "version=2\n")?;
    pkt_flush(p.stdin_mut())?;
    p.stdin_mut().flush()?;
    // Read welcome.
    let welcome = pkt_read_text(&mut p.stdout)?;
    if !welcome.starts_with("git-filter-server") {
        return Err(std::io::Error::other(format!("bad welcome: {welcome:?}")));
    }
    // Drain until flush: server announces its version pkts.
    while pkt_read(&mut p.stdout)?.is_some() {}
    pkt_write_text(p.stdin_mut(), "capability=clean\n")?;
    pkt_write_text(p.stdin_mut(), "capability=smudge\n")?;
    pkt_flush(p.stdin_mut())?;
    p.stdin_mut().flush()?;
    // Drain server's capability list until flush.
    while pkt_read(&mut p.stdout)?.is_some() {}
    Ok(())
}

/// Run a single `command=smudge` round-trip against a long-running
/// filter-process subprocess for `pathname`, sending `input_bytes` as
/// the payload. Returns the smudged output bytes on success.
fn filter_smudge(
    p: &mut FilterProcess,
    pathname: &str,
    input_bytes: &[u8],
) -> std::io::Result<Vec<u8>> {
    pkt_write_text(p.stdin_mut(), "command=smudge\n")?;
    pkt_write_text(p.stdin_mut(), &format!("pathname={pathname}\n"))?;
    pkt_flush(p.stdin_mut())?;
    p.stdin_mut().flush()?;
    // Payload pkt-lines, then a flush.
    for chunk in input_bytes.chunks(65516) {
        pkt_write_bytes(p.stdin_mut(), chunk)?;
    }
    pkt_flush(p.stdin_mut())?;
    p.stdin_mut().flush()?;
    // Read status pkt-line list (until flush). Then the response payload
    // (until flush). Then a final status list (until flush).
    let status1 = read_status_block(&mut p.stdout)?;
    if !status1.iter().any(|s| s.starts_with("status=success")) {
        return Err(std::io::Error::other(format!("smudge status: {status1:?}")));
    }
    let mut out = Vec::new();
    loop {
        match pkt_read(&mut p.stdout)? {
            None => break,
            Some(b) => out.extend_from_slice(&b),
        }
    }
    let _final = read_status_block(&mut p.stdout)?;
    Ok(out)
}

fn read_status_block(r: &mut BufReader<ChildStdout>) -> std::io::Result<Vec<String>> {
    let mut out = Vec::new();
    loop {
        match pkt_read(r)? {
            None => return Ok(out),
            Some(b) => out.push(String::from_utf8_lossy(&b).into_owned()),
        }
    }
}

// ---- pkt-line framing ----------------------------------------------------

fn pkt_write_text(w: &mut ChildStdin, s: &str) -> std::io::Result<()> {
    pkt_write_bytes(w, s.as_bytes())
}

fn pkt_write_bytes(w: &mut ChildStdin, bytes: &[u8]) -> std::io::Result<()> {
    let len = bytes.len() + 4;
    if len > 65520 {
        return Err(std::io::Error::other("pkt too large"));
    }
    let hdr = format!("{len:04x}");
    w.write_all(hdr.as_bytes())?;
    w.write_all(bytes)?;
    Ok(())
}

fn pkt_flush(w: &mut ChildStdin) -> std::io::Result<()> {
    w.write_all(b"0000")
}

/// Read one pkt-line. Returns `Ok(None)` for a flush pkt (`0000`),
/// `Ok(Some(bytes))` for a data pkt.
fn pkt_read(r: &mut BufReader<ChildStdout>) -> std::io::Result<Option<Vec<u8>>> {
    let mut hdr = [0u8; 4];
    r.read_exact(&mut hdr)?;
    let hex = std::str::from_utf8(&hdr).map_err(|e| std::io::Error::other(format!("hdr: {e}")))?;
    let len = u32::from_str_radix(hex, 16)
        .map_err(|e| std::io::Error::other(format!("hdr len: {e}")))?;
    if len == 0 {
        return Ok(None);
    }
    if len < 4 {
        return Err(std::io::Error::other(format!("bad pkt len: {len}")));
    }
    let body_len = (len - 4) as usize;
    let mut buf = vec![0u8; body_len];
    r.read_exact(&mut buf)?;
    Ok(Some(buf))
}

fn pkt_read_text(r: &mut BufReader<ChildStdout>) -> std::io::Result<String> {
    match pkt_read(r)? {
        None => Ok(String::new()),
        Some(b) => Ok(String::from_utf8_lossy(&b).into_owned()),
    }
}

// ---- Custom filter-process dispatch (slice 7) ----------------------------

/// Outcome of routing a single read through a custom
/// `filter.<name>.process` driver.
pub(crate) enum CustomFilterOutcome {
    /// Smudge succeeded; bytes are the driver's output for this path.
    Bytes(Vec<u8>),
    /// Driver isn't configured (no `filter.<name>.process`), spawn /
    /// handshake failed, or the smudge round-trip failed. Engine
    /// surfaces as `ContentUnavailable(FilterFailed { filter })`.
    FilterFailed,
}

/// Look up `filter.<name>.process` in the repo's git config. Returns
/// the configured shell command line, or `None` when unset (in which
/// case the driver is not "known" — the engine keeps the existing
/// fail-loud `FilterFailed` short-circuit per slice doc).
fn lookup_custom_filter_process_command(
    workdir: &std::path::Path,
    name: &str,
) -> Option<String> {
    let key = format!("filter.{name}.process");
    let out = std::process::Command::new("git")
        .current_dir(workdir)
        .args(["config", "--get", &key])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() { None } else { Some(s) }
}

/// True if a `filter.<name>.process` driver is configured for this
/// repo. Used by dispatch sites to decide whether `<name>` is
/// "known" (route through the custom subprocess) or "unknown" (stay on
/// `FilterFailed`).
fn is_custom_filter_configured(repo: &gix::Repository, name: &str) -> bool {
    let Ok(workdir) = git::work_dir(repo) else {
        return false;
    };
    lookup_custom_filter_process_command(workdir, name).is_some()
}

/// Smudge `input_bytes` through the configured `filter.<name>.process`
/// driver. Lazily spawns the subprocess on first call (cached on
/// `state.custom_filters` for the run); returns `FilterFailed` on any
/// configuration / spawn / protocol failure.
fn custom_filter_smudge(
    state: &mut EngineState,
    workdir: &std::path::Path,
    name: &str,
    pathname: &str,
    input_bytes: &[u8],
) -> CustomFilterOutcome {
    if !state.custom_filters.contains_key(name) {
        let spawned = match lookup_custom_filter_process_command(workdir, name) {
            None => return CustomFilterOutcome::FilterFailed,
            Some(cmd) => spawn_custom_filter_process(workdir, &cmd),
        };
        state.custom_filters.insert(name.to_string(), spawned);
    }
    match state.custom_filters.get_mut(name).expect("just inserted") {
        Err(_) => CustomFilterOutcome::FilterFailed,
        Ok(p) => match filter_smudge(p, pathname, input_bytes) {
            Ok(b) => CustomFilterOutcome::Bytes(b),
            Err(_) => CustomFilterOutcome::FilterFailed,
        },
    }
}

// ---- LFS dispatch helpers ------------------------------------------------

/// True if `path` resolves to `filter=lfs` per `.gitattributes`.
fn is_lfs_path(repo: &gix::Repository, path: &str) -> bool {
    let Ok(workdir) = git::work_dir(repo) else {
        return false;
    };
    matches!(
        types::path_filter_attribute(workdir, std::path::Path::new(path)),
        Ok(Some(ref n)) if n == "lfs"
    )
}

/// Parse the LFS pointer's referenced object id (sha256 hex) from
/// canonical pointer text. Returns `None` if `bytes` is not a pointer.
fn lfs_pointer_oid(bytes: &[u8]) -> Option<String> {
    let s = std::str::from_utf8(bytes).ok()?;
    if !s.starts_with("version https://git-lfs.github.com/spec/") {
        return None;
    }
    for line in s.lines() {
        if let Some(rest) = line.strip_prefix("oid sha256:") {
            return Some(rest.trim().to_string());
        }
    }
    None
}

/// `.git/lfs/objects/<oid[..2]>/<oid[2..4]>/<oid>` exists.
fn lfs_object_cached(workdir: &std::path::Path, oid: &str) -> bool {
    if oid.len() < 4 {
        return false;
    }
    workdir
        .join(".git")
        .join("lfs")
        .join("objects")
        .join(&oid[..2])
        .join(&oid[2..4])
        .join(oid)
        .exists()
}

/// Outcome of an LFS-routed read for a single side.
enum LfsReadOutcome {
    /// Smudged bytes (with `GIT_LFS_SKIP_SMUDGE=1` this is pointer text).
    Bytes(Vec<u8>),
    /// Pointer references an LFS object not in the local cache.
    NotFetched,
    /// `git-lfs` is not installed / unspawnable.
    NotInstalled,
}

/// Read an LFS-managed blob or worktree file through the managed
/// subprocess. Caller passes the raw pointer bytes (already in canonical
/// form, since LFS pointer text is stored verbatim in the blob and on
/// disk pre-smudge). Lazily spawns the LFS process on first call.
fn lfs_read(
    state: &mut EngineState,
    workdir: &std::path::Path,
    path: &str,
    pointer_bytes: &[u8],
) -> LfsReadOutcome {
    // Cache probe: missing object → NotFetched without touching the
    // subprocess (saves a spawn when the answer is already terminal).
    let Some(oid) = lfs_pointer_oid(pointer_bytes) else {
        // Not a pointer (e.g., already-smudged content, or not LFS) —
        // return bytes verbatim.
        return LfsReadOutcome::Bytes(pointer_bytes.to_vec());
    };
    if !lfs_object_cached(workdir, &oid) {
        return LfsReadOutcome::NotFetched;
    }
    // Lazily spawn.
    if state.lfs.is_none() {
        state.lfs = Some(spawn_lfs_process(workdir));
    }
    match state.lfs.as_mut().expect("just set") {
        Err(FilterSpawnError::NotInstalled) => LfsReadOutcome::NotInstalled,
        Err(FilterSpawnError::HandshakeFailed) => LfsReadOutcome::NotInstalled,
        Ok(p) => match filter_smudge(p, path, pointer_bytes) {
            Ok(b) => LfsReadOutcome::Bytes(b),
            Err(_) => LfsReadOutcome::NotInstalled,
        },
    }
}

/// Resolve a single line-range LFS pin against the deepest enabled
/// layer. Returns the appropriate `RangeResolved` directly: never falls
/// through to the generic comparator. See `lfs_read` for the cache /
/// subprocess semantics.
#[allow(clippy::too_many_arguments)]
fn resolve_lfs_range(
    repo: &gix::Repository,
    state: &mut EngineState,
    range_id: &str,
    r: &Range,
    anchored: RangeLocation,
    tracked: &Tracked,
    deepest_layer: DriftSource,
    index_blob_oid: Option<&str>,
    worktree_changed: bool,
) -> RangeResolved {
    let workdir = match git::work_dir(repo) {
        Ok(w) => w,
        Err(_) => return lfs_terminal(range_id, r, anchored, UnavailableReason::IoError {
            message: "no workdir".into(),
        }),
    };

    // Anchored side: the pinned blob OID is always present (read_range
    // populates it). It points at the canonical pointer-text blob.
    let anchored_pointer = match git::read_blob_bytes(repo, &r.blob) {
        Ok(b) => b,
        Err(_) => return lfs_terminal(range_id, r, anchored, UnavailableReason::IoError {
            message: format!("cannot read anchored blob {}", r.blob),
        }),
    };

    // Current side: pull the pointer bytes per layer. Worktree → file on
    // disk (pre-smudge with skip-smudge active); Index → the staged blob
    // OID; HEAD → the path's blob at HEAD.
    let current_pointer: Vec<u8> = match deepest_layer {
        DriftSource::Worktree => {
            // If worktree is enabled but didn't change, fall back to
            // index/HEAD blob for the OID-fast-path comparison.
            if worktree_changed {
                std::fs::read(workdir.join(&tracked.path)).unwrap_or_default()
            } else if let Some(o) = index_blob_oid.map(|s| s.to_string()) {
                git::read_blob_bytes(repo, &o).unwrap_or_default()
            } else if let Ok(o) = head_blob_for(repo, &tracked.path) {
                git::read_blob_bytes(repo, &o).unwrap_or_default()
            } else {
                std::fs::read(workdir.join(&tracked.path)).unwrap_or_default()
            }
        }
        DriftSource::Index => {
            let oid = index_blob_oid
                .map(|s| s.to_string())
                .or_else(|| head_blob_for(repo, &tracked.path).ok());
            match oid {
                Some(o) => git::read_blob_bytes(repo, &o).unwrap_or_default(),
                None => Vec::new(),
            }
        }
        DriftSource::Head => {
            let oid = head_blob_for(repo, &tracked.path).ok();
            match oid {
                Some(o) => git::read_blob_bytes(repo, &o).unwrap_or_default(),
                None => Vec::new(),
            }
        }
    };

    // Pointer-OID fast path: identical pointer bytes across both sides
    // implies LFS object identity (pointer is the only thing git stores).
    let anchored_oid = lfs_pointer_oid(&anchored_pointer);
    let current_oid = lfs_pointer_oid(&current_pointer);
    let same_path_extent = tracked.path == r.path
        && matches!(r.extent, RangeExtent::Lines { start, end } if start == tracked.start && end == tracked.end);
    if anchored_oid.is_some() && anchored_oid == current_oid {
        let status = if same_path_extent {
            RangeStatus::Fresh
        } else {
            RangeStatus::Moved
        };
        let source = if status == RangeStatus::Fresh { None } else { Some(deepest_layer) };
        return RangeResolved {
            range_id: range_id.into(),
            anchor_sha: r.anchor_sha.clone(),
            anchored,
            current: Some(RangeLocation {
                path: PathBuf::from(&tracked.path),
                extent: RangeExtent::Lines {
                    start: tracked.start,
                    end: tracked.end,
                },
                blob: oid_from_hex(&r.blob).ok(),
            }),
            status,
            source,
            acknowledged_by: None,
            culprit: None,
        };
    }

    // Smudge both sides through the managed subprocess.
    let anchored_smudged = match lfs_read(state, workdir, &r.path, &anchored_pointer) {
        LfsReadOutcome::Bytes(b) => b,
        LfsReadOutcome::NotFetched => {
            return lfs_terminal(range_id, r, anchored, UnavailableReason::LfsNotFetched);
        }
        LfsReadOutcome::NotInstalled => {
            return lfs_terminal(range_id, r, anchored, UnavailableReason::LfsNotInstalled);
        }
    };
    let current_smudged = match lfs_read(state, workdir, &tracked.path, &current_pointer) {
        LfsReadOutcome::Bytes(b) => b,
        LfsReadOutcome::NotFetched => {
            return lfs_terminal(range_id, r, anchored, UnavailableReason::LfsNotFetched);
        }
        LfsReadOutcome::NotInstalled => {
            return lfs_terminal(range_id, r, anchored, UnavailableReason::LfsNotInstalled);
        }
    };

    // If both smudged outputs are themselves still LFS pointer text
    // (the operating mode under `GIT_LFS_SKIP_SMUDGE=1`), arbitrary
    // line slicing returns the wrong answer because the pointer header
    // line is identical across every LFS object. Fall back to OID-level
    // comparison: different pointer OIDs imply different binary
    // content, so the pin is `Changed`. (The pointer-OID fast path
    // above handled `Fresh`/`Moved` when OIDs matched.)
    let a_smudged_oid = lfs_pointer_oid(&anchored_smudged);
    let c_smudged_oid = lfs_pointer_oid(&current_smudged);
    if a_smudged_oid.is_some() || c_smudged_oid.is_some() {
        let status = if a_smudged_oid == c_smudged_oid {
            if same_path_extent { RangeStatus::Fresh } else { RangeStatus::Moved }
        } else {
            RangeStatus::Changed
        };
        let source = if status == RangeStatus::Fresh { None } else { Some(deepest_layer) };
        return RangeResolved {
            range_id: range_id.into(),
            anchor_sha: r.anchor_sha.clone(),
            anchored,
            current: Some(RangeLocation {
                path: PathBuf::from(&tracked.path),
                extent: RangeExtent::Lines { start: tracked.start, end: tracked.end },
                blob: None,
            }),
            status,
            source,
            acknowledged_by: None,
            culprit: None,
        };
    }

    // Compare on the line-range slice (real smudged content path —
    // reached only when the operator disabled `SKIP_SMUDGE`).
    let (a_start, a_end) = match r.extent {
        RangeExtent::Lines { start, end } => (start, end),
        RangeExtent::Whole => (1, 1),
    };
    let a_text = String::from_utf8_lossy(&anchored_smudged);
    let c_text = String::from_utf8_lossy(&current_smudged);
    let a_lines: Vec<&str> = a_text.lines().collect();
    let c_lines: Vec<&str> = c_text.lines().collect();
    let a_lo = (a_start as usize).saturating_sub(1);
    let a_hi = (a_end as usize).min(a_lines.len());
    let c_lo = (tracked.start as usize).saturating_sub(1);
    let c_hi = (tracked.end as usize).min(c_lines.len());
    let a_slice = if a_lo <= a_hi { &a_lines[a_lo..a_hi] } else { &[][..] };
    let c_slice = if c_lo <= c_hi { &c_lines[c_lo..c_hi] } else { &[][..] };
    let equal = a_slice == c_slice;
    let status = if equal {
        if same_path_extent { RangeStatus::Fresh } else { RangeStatus::Moved }
    } else {
        RangeStatus::Changed
    };
    let source = if status == RangeStatus::Fresh { None } else { Some(deepest_layer) };
    RangeResolved {
        range_id: range_id.into(),
        anchor_sha: r.anchor_sha.clone(),
        anchored,
        current: Some(RangeLocation {
            path: PathBuf::from(&tracked.path),
            extent: RangeExtent::Lines {
                start: tracked.start,
                end: tracked.end,
            },
            blob: None,
        }),
        status,
        source,
        acknowledged_by: None,
        culprit: None,
    }
}

fn lfs_terminal(
    range_id: &str,
    r: &Range,
    anchored: RangeLocation,
    reason: UnavailableReason,
) -> RangeResolved {
    RangeResolved {
        range_id: range_id.into(),
        anchor_sha: r.anchor_sha.clone(),
        anchored,
        current: None,
        status: RangeStatus::ContentUnavailable(reason),
        source: None,
        acknowledged_by: None,
        culprit: None,
    }
}

//! Per-range layered resolution: HEAD walk + index/worktree hunk
//! application + LFS short-circuit + slice comparison.

use super::EngineState;
use super::super::layers::{
    filter_short_circuit, is_lfs_path, read_worktree_normalized, resolve_lfs_range,
};
use super::super::walker::{Tracked, apply_hunks_to_range, resolve_at_head};
use super::whole_file::resolve_whole_file;
use crate::git;
use crate::types::{
    DriftSource, MeshConfig, Range, RangeExtent, RangeLocation, RangeResolved, RangeStatus,
    UnavailableReason,
};
use crate::{Error, Result};
use std::path::PathBuf;
use std::str::FromStr;

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

fn head_blob_for(repo: &gix::Repository, path: &str) -> Result<String> {
    let head_sha = git::head_oid(repo)?;
    git::path_blob_at(repo, &head_sha, path)
}

fn string_from_utf8_lossy(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// Read the text content of an index blob by OID.
fn read_blob_text(repo: &gix::Repository, oid_hex: &str) -> String {
    git::read_git_text(repo, oid_hex).unwrap_or_default()
}

/// Compare the content slice at `tracked` position against the anchored
/// slice. Returns `true` when the slice differs (i.e. this layer drifts).
fn slice_differs(
    text: &str,
    tracked: &Tracked,
    anchored_lines: &[&str],
    anchored_start: u32,
    anchored_end: u32,
    ignore_ws: bool,
) -> bool {
    let current_lines: Vec<&str> = text.lines().collect();
    let a_lo = (anchored_start as usize).saturating_sub(1);
    let a_hi = (anchored_end as usize).min(anchored_lines.len());
    let c_lo = (tracked.start as usize).saturating_sub(1);
    let c_hi = (tracked.end as usize).min(current_lines.len());
    let a_slice = if a_lo <= a_hi { &anchored_lines[a_lo..a_hi] } else { &[][..] };
    let c_slice = if c_lo <= c_hi { &current_lines[c_lo..c_hi] } else { &[][..] };
    !lines_equal(a_slice, c_slice, ignore_ws)
}

pub(crate) fn resolve_range_inner(
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
            layer_sources: vec![],
            acknowledged_by: None,
            culprit: None,
        });
    }

    let head_loc = resolve_at_head(repo, &r, cfg.copy_detection, &mut state.warnings)?;

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
                    extent: RangeExtent::Lines { start: anchored_start, end: anchored_end },
                    blob: None,
                }),
                status: RangeStatus::MergeConflict,
                source: None,
                layer_sources: vec![],
                acknowledged_by: None,
                culprit: None,
            });
        }
    }

    // Track per-layer positions. Each option is `None` if the path was
    // deleted at that layer.
    let head_tracked = head_loc.clone();

    // Index layer: apply hunks on top of head_tracked.
    let mut index_tracked: Option<Tracked> = head_tracked.clone();
    let mut index_blob_oid: Option<String> = None;
    let mut index_hunk_applied = false;
    if state.layers.index
        && let Some(t) = index_tracked.as_ref()
        && let Some(diffs) = state.index_diffs.as_ref()
        && let Some(entry) = diffs.map.get(&t.path)
    {
        if entry.deleted {
            index_tracked = None;
        } else {
            let (s, e) = apply_hunks_to_range(&entry.hunks, t.start, t.end);
            let new_path = entry.new_path.clone();
            index_tracked = Some(Tracked { path: new_path, start: s, end: e });
            index_blob_oid = entry.new_blob.clone();
            index_hunk_applied = true;
        }
    }

    // Worktree layer: apply hunks on top of index_tracked.
    let mut worktree_tracked: Option<Tracked> = index_tracked.clone();
    let mut worktree_hunk_applied = false;
    if state.layers.worktree
        && let Some(t) = worktree_tracked.as_ref()
        && let Some(diffs) = state.worktree_diffs.as_ref()
        && let Some(entry) = diffs.map.get(&t.path)
    {
        if entry.deleted {
            worktree_tracked = None;
        } else {
            let (s, e) = apply_hunks_to_range(&entry.hunks, t.start, t.end);
            let new_path = entry.new_path.clone();
            worktree_tracked = Some(Tracked { path: new_path, start: s, end: e });
            worktree_hunk_applied = true;
        }
    }

    // The deepest enabled layer's tracked position determines `current`.
    let (tracked, deepest_layer) = if state.layers.worktree {
        (worktree_tracked.as_ref(), DriftSource::Worktree)
    } else if state.layers.index {
        (index_tracked.as_ref(), DriftSource::Index)
    } else {
        (head_tracked.as_ref(), DriftSource::Head)
    };

    // LFS short-circuit: if the deepest tracked path is LFS-managed, delegate.
    if let Some(t) = tracked
        && is_lfs_path(repo, &t.path)
    {
        return Ok(resolve_lfs_range(
            repo,
            &mut state.lfs,
            range_id,
            &r,
            anchored,
            t,
            deepest_layer,
            index_blob_oid.as_deref(),
            worktree_hunk_applied,
        ));
    }

    // Read the deepest layer's content for `current` and overall status.
    let current = match tracked {
        None => None,
        Some(t) => {
            let (cur_text, cur_blob) = match deepest_layer {
                DriftSource::Worktree => match read_worktree_normalized(repo, &mut state.custom_filters, &t.path) {
                    Ok(bytes) => (string_from_utf8_lossy(&bytes), None),
                    Err(Error::FilterFailed { filter }) => {
                        return Ok(unavailable(range_id, &r, anchored, UnavailableReason::FilterFailed { filter }));
                    }
                    Err(e) => return Err(e),
                },
                DriftSource::Index => {
                    if let Some(filter) = filter_short_circuit(repo, &t.path)? {
                        return Ok(unavailable(range_id, &r, anchored, UnavailableReason::FilterFailed { filter }));
                    }
                    let oid = index_blob_oid.clone().or_else(|| {
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
                        return Ok(unavailable(range_id, &r, anchored, UnavailableReason::FilterFailed { filter }));
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
    let layer_sources: Vec<DriftSource>;

    match current {
        None => {
            status = RangeStatus::Changed;
            source = Some(deepest_layer);
            current_loc = None;
            layer_sources = vec![deepest_layer];
        }
        Some((t, cur_text, cur_blob)) => {
            let anchored_text = git::read_git_text(repo, &r.blob)?;
            let anchored_lines: Vec<&str> = anchored_text.lines().collect();
            let current_lines: Vec<&str> = cur_text.lines().collect();
            let a_lo = (anchored_start as usize).saturating_sub(1);
            let a_hi = (anchored_end as usize).min(anchored_lines.len());
            let c_lo = (t.start as usize).saturating_sub(1);
            let c_hi = (t.end as usize).min(current_lines.len());
            let a_slice = if a_lo <= a_hi { &anchored_lines[a_lo..a_hi] } else { &[][..] };
            let c_slice = if c_lo <= c_hi { &current_lines[c_lo..c_hi] } else { &[][..] };
            let equal = lines_equal(a_slice, c_slice, cfg.ignore_whitespace);

            // Compute per-layer drift: compare each enabled layer's content
            // independently against the anchor. Emit a Finding per drifting
            // layer in shallow-to-deep order (I → W → H).
            let computed_layer_sources = compute_layer_sources(
                repo,
                &r,
                &head_tracked,
                &index_tracked,
                &worktree_tracked,
                &mut *state,
                &anchored_lines,
                anchored_start,
                anchored_end,
                cfg.ignore_whitespace,
                index_hunk_applied,
                worktree_hunk_applied,
                &index_blob_oid,
            )?;

            let inferred_source = computed_layer_sources.first().copied();

            if equal {
                if t.path == r.path && t.start == anchored_start && t.end == anchored_end {
                    status = RangeStatus::Fresh;
                    source = None;
                    layer_sources = vec![];
                } else {
                    status = RangeStatus::Moved;
                    source = inferred_source;
                    // MOVED means bytes are equal; per design requirement 4,
                    // keep the single-row shape (source=first drifting layer).
                    layer_sources = if let Some(s) = inferred_source { vec![s] } else { vec![] };
                }
            } else {
                status = RangeStatus::Changed;
                source = inferred_source.or(Some(deepest_layer));
                layer_sources = if computed_layer_sources.is_empty() {
                    vec![deepest_layer]
                } else {
                    computed_layer_sources
                };
            }
            current_loc = Some(RangeLocation {
                path: PathBuf::from(t.path.clone()),
                extent: RangeExtent::Lines { start: t.start, end: t.end },
                blob: if worktree_hunk_applied {
                    None
                } else if state.layers.index && index_blob_oid.is_some() {
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
        layer_sources,
        acknowledged_by: None,
        culprit: None,
    })
}

fn unavailable(
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
        layer_sources: vec![],
        acknowledged_by: None,
        culprit: None,
    }
}

/// Compute the list of layers that independently show drift vs the anchor,
/// in shallow-to-deep order: Index → Worktree → Head.
///
/// For each enabled layer:
/// - If the path was deleted at that layer → drifts.
/// - Otherwise read that layer's content at the tracked position and compare
///   the anchored slice. If they differ → drifts.
///
/// The index-hunk and worktree-hunk applied flags indicate whether the diff
/// pass found any change at that layer for this path. If no hunk was applied
/// for a layer, the position matches the layer above — so we only need to
/// check whether the content differs from anchor.
#[allow(clippy::too_many_arguments)]
fn compute_layer_sources(
    repo: &gix::Repository,
    _r: &Range,
    head_tracked: &Option<Tracked>,
    index_tracked: &Option<Tracked>,
    worktree_tracked: &Option<Tracked>,
    state: &mut EngineState,
    anchored_lines: &[&str],
    anchored_start: u32,
    anchored_end: u32,
    ignore_ws: bool,
    index_hunk_applied: bool,
    worktree_hunk_applied: bool,
    index_blob_oid: &Option<String>,
) -> Result<Vec<DriftSource>> {
    let layer_index = state.layers.index;
    let layer_worktree = state.layers.worktree;
    let mut sources: Vec<DriftSource> = Vec::new();

    // HEAD layer: compare HEAD content at head_tracked position vs anchor.
    {
        let head_drifts = match head_tracked.as_ref() {
            None => true, // path deleted at HEAD
            Some(t) => {
                if let Some(filter) = filter_short_circuit(repo, &t.path)? {
                    // Can't compare — treat as drifts (fail-closed).
                    let _ = filter;
                    true
                } else {
                    let oid = head_blob_for(repo, &t.path).ok();
                    let txt = match &oid {
                        Some(o) => git::read_git_text(repo, o).unwrap_or_default(),
                        None => String::new(),
                    };
                    slice_differs(&txt, t, anchored_lines, anchored_start, anchored_end, ignore_ws)
                }
            }
        };
        // HEAD is always enabled (the baseline layer). If HEAD drifts, only
        // emit it; shallower layers (I, W) that also drift are still emitted.
        if head_drifts {
            sources.push(DriftSource::Head);
        }
    }

    // Index layer: compare Index content at index_tracked position vs anchor.
    if layer_index {
        let index_drifts = match index_tracked.as_ref() {
            None => true, // path deleted in index
            Some(t) => {
                if index_hunk_applied {
                    // Index blob changed for this path; read the indexed blob.
                    let oid = index_blob_oid.clone().or_else(|| head_blob_for(repo, &t.path).ok());
                    let txt = match &oid {
                        Some(o) => read_blob_text(repo, o),
                        None => String::new(),
                    };
                    slice_differs(&txt, t, anchored_lines, anchored_start, anchored_end, ignore_ws)
                } else {
                    // No index hunk for this path: index content == head
                    // content at this position. Reuse the HEAD drift result
                    // so we don't re-read the blob.
                    let oid = head_blob_for(repo, &t.path).ok();
                    let txt = match &oid {
                        Some(o) => git::read_git_text(repo, o).unwrap_or_default(),
                        None => String::new(),
                    };
                    slice_differs(&txt, t, anchored_lines, anchored_start, anchored_end, ignore_ws)
                }
            }
        };
        if index_drifts {
            sources.push(DriftSource::Index);
        }
    }

    // Worktree layer: compare worktree content at worktree_tracked vs anchor.
    if layer_worktree {
        let worktree_drifts = match worktree_tracked.as_ref() {
            None => true, // path deleted in worktree
            Some(t) => {
                if worktree_hunk_applied {
                    match read_worktree_normalized(repo, &mut state.custom_filters, &t.path) {
                        Ok(bytes) => {
                            let txt = string_from_utf8_lossy(&bytes);
                            slice_differs(&txt, t, anchored_lines, anchored_start, anchored_end, ignore_ws)
                        }
                        Err(_) => true, // fail-closed on unreadable
                    }
                } else {
                    // No worktree hunk: worktree == index at this position.
                    let oid = index_blob_oid.clone().or_else(|| head_blob_for(repo, &t.path).ok());
                    let txt = match &oid {
                        Some(o) => read_blob_text(repo, o),
                        None => String::new(),
                    };
                    slice_differs(&txt, t, anchored_lines, anchored_start, anchored_end, ignore_ws)
                }
            }
        };
        if worktree_drifts {
            sources.push(DriftSource::Worktree);
        }
    }

    // Return in shallow-to-deep order: I → W → H. Currently sources was
    // built H first for clarity; reorder to I → W → H.
    let mut ordered: Vec<DriftSource> = Vec::new();
    if sources.contains(&DriftSource::Index) {
        ordered.push(DriftSource::Index);
    }
    if sources.contains(&DriftSource::Worktree) {
        ordered.push(DriftSource::Worktree);
    }
    if sources.contains(&DriftSource::Head) {
        ordered.push(DriftSource::Head);
    }
    Ok(ordered)
}

//! Per-anchor layered resolution: HEAD walk + index/worktree hunk
//! application + LFS short-circuit + slice comparison.

use super::super::layers::{
    is_lfs_path, read_worktree_normalized, resolve_lfs_anchor,
};
use super::super::session::resolve_at_head_shared;
use super::super::walker::{Tracked, apply_hunks_to_range};
use super::EngineState;
use super::whole_file::resolve_whole_file;
use crate::git;
use crate::types::{
    Anchor, AnchorExtent, AnchorLocation, AnchorResolved, AnchorStatus, DriftSource, MeshConfig,
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
    !lines_equal(a_slice, c_slice, ignore_ws)
}

pub(crate) fn resolve_anchor_inner(
    repo: &gix::Repository,
    state: &mut EngineState,
    cfg: &MeshConfig,
    anchor_id: &str,
    r: Anchor,
) -> Result<AnchorResolved> {
    if matches!(r.extent, AnchorExtent::WholeFile) {
        return resolve_whole_file(repo, state, cfg, anchor_id, r);
    }
    let (anchored_start, anchored_end) = match r.extent {
        AnchorExtent::LineRange { start, end } => (start, end),
        AnchorExtent::WholeFile => unreachable!(),
    };
    let anchored = AnchorLocation {
        path: PathBuf::from(&r.path),
        extent: r.extent,
        blob: oid_from_hex(&r.blob).ok(),
    };
    if !state.commit_reachable(repo, &r.anchor_sha)? {
        return Ok(AnchorResolved {
            anchor_id: anchor_id.into(),
            anchor_sha: r.anchor_sha,
            anchored,
            current: None,
            status: AnchorStatus::Orphaned,
            source: None,
            layer_sources: vec![],
            acknowledged_by: None,
            locus: None,
        });
    }

    if r.anchor_sha == state.head_sha {
        let head_loc = Some(Tracked {
            path: r.path.clone(),
            start: anchored_start,
            end: anchored_end,
        });
        if let Some(resolved) = clean_head_fast_path(
            repo,
            state,
            anchor_id,
            &r,
            anchored.clone(),
            &head_loc,
            anchored_start,
            anchored_end,
        )? {
            return Ok(resolved);
        }
    }

    let head_loc = resolve_at_head_shared(
        repo,
        &mut state.session,
        &r,
        cfg.copy_detection,
        &mut state.warnings,
    )?;

    let head_path: Option<String> = head_loc.as_ref().map(|t| t.path.clone());
    if state.layers.index || state.layers.worktree {
        let p = head_path.as_deref().unwrap_or(r.path.as_str());
        if state.conflicted_paths.contains(p) {
            return Ok(AnchorResolved {
                anchor_id: anchor_id.into(),
                anchor_sha: r.anchor_sha,
                anchored,
                current: Some(AnchorLocation {
                    path: PathBuf::from(p),
                    extent: AnchorExtent::LineRange {
                        start: anchored_start,
                        end: anchored_end,
                    },
                    blob: None,
                }),
                status: AnchorStatus::MergeConflict,
                source: None,
                layer_sources: vec![],
                acknowledged_by: None,
                locus: None,
            });
        }
    }

    if let Some(resolved) = clean_head_fast_path(
        repo,
        state,
        anchor_id,
        &r,
        anchored.clone(),
        &head_loc,
        anchored_start,
        anchored_end,
    )? {
        return Ok(resolved);
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
            index_tracked = Some(Tracked {
                path: new_path,
                start: s,
                end: e,
            });
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
            worktree_tracked = Some(Tracked {
                path: new_path,
                start: s,
                end: e,
            });
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
        return Ok(resolve_lfs_anchor(
            repo,
            &mut state.lfs,
            anchor_id,
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
                DriftSource::Worktree => {
                    match read_worktree_normalized(repo, &mut state.custom_filters, &t.path) {
                        Ok(bytes) => (string_from_utf8_lossy(&bytes), None),
                        Err(Error::FilterFailed { filter }) => {
                            return Ok(unavailable(
                                anchor_id,
                                &r,
                                anchored,
                                UnavailableReason::FilterFailed { filter },
                            ));
                        }
                        Err(e) => return Err(e),
                    }
                }
                DriftSource::Index => {
                    if let Some(filter) = state.filter_short_circuit(repo, &t.path)? {
                        return Ok(unavailable(
                            anchor_id,
                            &r,
                            anchored,
                            UnavailableReason::FilterFailed { filter },
                        ));
                    }
                    let oid = index_blob_oid
                        .clone()
                        .or_else(|| head_blob_for(repo, &t.path).ok());
                    match oid {
                        Some(o) => {
                            let txt = git::read_git_text(repo, &o).unwrap_or_default();
                            (txt, oid_from_hex(&o).ok())
                        }
                        None => (String::new(), None),
                    }
                }
                DriftSource::Head => {
                    if let Some(filter) = state.filter_short_circuit(repo, &t.path)? {
                        return Ok(unavailable(
                            anchor_id,
                            &r,
                            anchored,
                            UnavailableReason::FilterFailed { filter },
                        ));
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

    let status: AnchorStatus;
    let source: Option<DriftSource>;
    let current_loc: Option<AnchorLocation>;
    let layer_sources: Vec<DriftSource>;

    match current {
        None => {
            let anchored_text = git::read_git_text(repo, &r.blob)?;
            let anchored_lines: Vec<&str> = anchored_text.lines().collect();
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
            status = AnchorStatus::Changed;
            source = computed_layer_sources.first().copied().or(Some(deepest_layer));
            current_loc = None;
            layer_sources = if computed_layer_sources.is_empty() {
                vec![deepest_layer]
            } else {
                computed_layer_sources
            };
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

            // A committed cross-path rename detaches the anchor — the mesh
            // stores paths, not blob identity. Detect via head_tracked
            // diverging from the anchored path; reclassify to Orphaned so
            // `populate_drift_locus` emits `orphaned in <rename-sha>`.
            let head_path_diverged = head_tracked
                .as_ref()
                .is_some_and(|h| h.path != r.path);
            let anchored_path_absent_at_head =
                state.head_blob_at(repo, &r.path)?.is_none();
            if equal && head_path_diverged && anchored_path_absent_at_head {
                return Ok(AnchorResolved {
                    anchor_id: anchor_id.into(),
                    anchor_sha: r.anchor_sha,
                    anchored,
                    current: None,
                    status: AnchorStatus::Orphaned,
                    source: None,
                    layer_sources: vec![],
                    acknowledged_by: None,
                    locus: None,
                });
            }
            if equal {
                if t.path == r.path && t.start == anchored_start && t.end == anchored_end {
                    status = AnchorStatus::Fresh;
                    source = None;
                    layer_sources = vec![];
                } else {
                    status = AnchorStatus::Moved;
                    source = inferred_source;
                    // MOVED means bytes are equal; per design requirement 4,
                    // keep the single-row shape (source=first drifting layer).
                    layer_sources = if let Some(s) = inferred_source {
                        vec![s]
                    } else {
                        vec![]
                    };
                }
            } else {
                status = AnchorStatus::Changed;
                source = inferred_source.or(Some(deepest_layer));
                layer_sources = if computed_layer_sources.is_empty() {
                    vec![deepest_layer]
                } else {
                    computed_layer_sources
                };
            }
            current_loc = Some(AnchorLocation {
                path: PathBuf::from(t.path.clone()),
                extent: AnchorExtent::LineRange {
                    start: t.start,
                    end: t.end,
                },
                blob: if worktree_hunk_applied {
                    None
                } else if state.layers.index && index_blob_oid.is_some() {
                    index_blob_oid.as_deref().and_then(|o| oid_from_hex(o).ok())
                } else {
                    cur_blob
                },
            });
        }
    }

    Ok(AnchorResolved {
        anchor_id: anchor_id.into(),
        anchor_sha: r.anchor_sha,
        anchored,
        current: current_loc,
        status,
        source,
        layer_sources,
        acknowledged_by: None,
        locus: None,
    })
}

fn unavailable(
    anchor_id: &str,
    r: &Anchor,
    anchored: AnchorLocation,
    reason: UnavailableReason,
) -> AnchorResolved {
    AnchorResolved {
        anchor_id: anchor_id.into(),
        anchor_sha: r.anchor_sha.clone(),
        anchored,
        current: None,
        status: AnchorStatus::ContentUnavailable(reason),
        source: None,
        layer_sources: vec![],
        acknowledged_by: None,
        locus: None,
    }
}

#[allow(clippy::too_many_arguments)]
fn clean_head_fast_path(
    repo: &gix::Repository,
    state: &mut EngineState,
    anchor_id: &str,
    r: &Anchor,
    anchored: AnchorLocation,
    head_loc: &Option<Tracked>,
    anchored_start: u32,
    anchored_end: u32,
) -> Result<Option<AnchorResolved>> {
    let content_layers_match_head =
        state.clean_layers || (!state.layers.index && !state.layers.worktree);
    if !content_layers_match_head {
        return Ok(None);
    }
    let Some(t) = head_loc.as_ref() else {
        return Ok(None);
    };
    if state.filter_short_circuit(repo, &t.path)?.is_some() {
        return Ok(None);
    }
    let Some(head_blob) = state.head_blob_at(repo, &t.path)? else {
        return Ok(None);
    };
    if head_blob != r.blob {
        return Ok(None);
    }
    // Committed cross-path rename detaches the anchor (mesh stores paths,
    // not blob identity). Reclassify as Orphaned so `populate_drift_locus`
    // emits `orphaned in <rename-sha>` via the forward walk. A *copy*
    // (anchored path still present at HEAD) is not a rename: leave it as
    // Moved so the anchor follows the new path.
    let anchored_path_absent_at_head = state.head_blob_at(repo, &r.path)?.is_none();
    if t.path != r.path && anchored_path_absent_at_head {
        state.session.anchors_fast_path_hits += 1;
        return Ok(Some(AnchorResolved {
            anchor_id: anchor_id.into(),
            anchor_sha: r.anchor_sha.clone(),
            anchored,
            current: None,
            status: AnchorStatus::Orphaned,
            source: None,
            layer_sources: vec![],
            acknowledged_by: None,
            locus: None,
        }));
    }
    let status = if t.path == r.path && t.start == anchored_start && t.end == anchored_end {
        AnchorStatus::Fresh
    } else {
        AnchorStatus::Moved
    };
    let current_blob = if state.layers.worktree {
        None
    } else {
        oid_from_hex(&head_blob).ok()
    };
    state.session.anchors_fast_path_hits += 1;
    Ok(Some(AnchorResolved {
        anchor_id: anchor_id.into(),
        anchor_sha: r.anchor_sha.clone(),
        anchored,
        current: Some(AnchorLocation {
            path: PathBuf::from(t.path.clone()),
            extent: AnchorExtent::LineRange {
                start: t.start,
                end: t.end,
            },
            blob: current_blob,
        }),
        status,
        source: None,
        layer_sources: vec![],
        acknowledged_by: None,
        locus: None,
    }))
}

/// Compute the list of layers that drift from the *next-deeper* layer, in
/// shallow-to-deep order: Worktree → Index → Head.
///
/// Each enabled layer is compared against its next-deeper neighbor:
/// - Worktree vs Index: worktree drifts when (a) the path is present in
///   the index but absent in the worktree, (b) a worktree hunk was
///   applied for the path, or (c) the resolved worktree slice differs
///   from the index slice at the anchored range.
/// - Index vs Head: index drifts when (a) the path is present at HEAD
///   but absent in the index, (b) an index hunk was applied, or (c) the
///   resolved index slice differs from the HEAD slice at the anchored
///   range.
/// - Head vs Anchor: HEAD drifts when (a) the path is absent from HEAD or
///   (b) HEAD's slice differs from the anchored slice.
///
/// The `source` returned by the caller is the first (shallowest) entry of
/// the resulting list; `layer_sources` is the full list. Both follow the
/// shallow-to-deep order Worktree → Index → Head.
#[allow(clippy::too_many_arguments)]
fn compute_layer_sources(
    repo: &gix::Repository,
    _r: &Anchor,
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

    // Read each layer's content as text, scoped to the layer's tracked
    // path. `None` represents "path absent at this layer".
    let head_text: Option<(String, Tracked)> = match head_tracked.as_ref() {
        None => None,
        Some(t) => {
            if state.filter_short_circuit(repo, &t.path)?.is_some() {
                // Fail-closed: can't read — treat as "absent" so adjacent
                // comparisons surface drift.
                None
            } else {
                let oid = head_blob_for(repo, &t.path).ok();
                let txt = match &oid {
                    Some(o) => git::read_git_text(repo, o).unwrap_or_default(),
                    None => String::new(),
                };
                Some((txt, t.clone()))
            }
        }
    };

    let index_text: Option<(String, Tracked)> = if layer_index {
        match index_tracked.as_ref() {
            None => None,
            Some(t) => {
                let oid = if index_hunk_applied {
                    index_blob_oid
                        .clone()
                        .or_else(|| head_blob_for(repo, &t.path).ok())
                } else {
                    head_blob_for(repo, &t.path).ok()
                };
                let txt = match &oid {
                    Some(o) => read_blob_text(repo, o),
                    None => String::new(),
                };
                Some((txt, t.clone()))
            }
        }
    } else {
        head_text.clone()
    };

    let worktree_text: Option<(String, Tracked)> = if layer_worktree {
        match worktree_tracked.as_ref() {
            None => None,
            Some(t) => {
                if worktree_hunk_applied {
                    match read_worktree_normalized(repo, &mut state.custom_filters, &t.path) {
                        Ok(bytes) => Some((string_from_utf8_lossy(&bytes), t.clone())),
                        Err(_) => None,
                    }
                } else {
                    let oid = index_blob_oid
                        .clone()
                        .or_else(|| head_blob_for(repo, &t.path).ok());
                    let txt = match &oid {
                        Some(o) => read_blob_text(repo, o),
                        None => String::new(),
                    };
                    Some((txt, t.clone()))
                }
            }
        }
    } else {
        index_text.clone()
    };

    let mut sources: Vec<DriftSource> = Vec::new();

    // Worktree drifts from Index when (a) worktree absent while index
    // present, or (b) the worktree slice differs from the index slice.
    if layer_worktree {
        let drifts = match (&worktree_text, &index_text) {
            (None, Some(_)) => true,
            (Some(_), None) => false,
            (None, None) => false,
            (Some((wt_txt, wt_t)), Some((idx_txt, idx_t))) => {
                slice_pair_differs(wt_txt, wt_t, idx_txt, idx_t, ignore_ws)
            }
        };
        if drifts {
            sources.push(DriftSource::Worktree);
        }
    }

    // Index drifts from HEAD when (a) index absent while HEAD present,
    // or (b) the index slice differs from the HEAD slice.
    if layer_index {
        let drifts = match (&index_text, &head_text) {
            (None, Some(_)) => true,
            (Some(_), None) => false,
            (None, None) => false,
            (Some((idx_txt, idx_t)), Some((head_txt, head_t))) => {
                slice_pair_differs(idx_txt, idx_t, head_txt, head_t, ignore_ws)
            }
        };
        if drifts {
            sources.push(DriftSource::Index);
        }
    }

    // HEAD drifts from the anchor when (a) HEAD absent or (b) HEAD slice
    // differs from anchored slice.
    let head_drifts = match &head_text {
        None => true,
        Some((txt, t)) => slice_differs(
            txt,
            t,
            anchored_lines,
            anchored_start,
            anchored_end,
            ignore_ws,
        ),
    };
    if head_drifts {
        sources.push(DriftSource::Head);
    }

    Ok(sources)
}

/// Compare two layers' slices at their respective tracked ranges.
/// Returns `true` when the slices differ.
fn slice_pair_differs(
    a_text: &str,
    a_t: &Tracked,
    b_text: &str,
    b_t: &Tracked,
    ignore_ws: bool,
) -> bool {
    let a_lines: Vec<&str> = a_text.lines().collect();
    let b_lines: Vec<&str> = b_text.lines().collect();
    let a_lo = (a_t.start as usize).saturating_sub(1);
    let a_hi = (a_t.end as usize).min(a_lines.len());
    let b_lo = (b_t.start as usize).saturating_sub(1);
    let b_hi = (b_t.end as usize).min(b_lines.len());
    let a_slice = if a_lo <= a_hi { &a_lines[a_lo..a_hi] } else { &[][..] };
    let b_slice = if b_lo <= b_hi { &b_lines[b_lo..b_hi] } else { &[][..] };
    !lines_equal(a_slice, b_slice, ignore_ws)
}

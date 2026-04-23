//! Resolver: compute staleness for ranges and meshes (§5).
//!
//! Slice 1 of the layered-stale rewrite (see
//! `docs/stale-layers-plan.md`) implements the HEAD-only fast path
//! against the new `Finding`-equivalent shapes
//! (`RangeResolved` / `MeshResolved`). Layered modes (worktree / index /
//! staged-mesh) are intentionally `todo!()` — they land in subsequent
//! slices.

#![allow(dead_code)]

use crate::git;
use crate::mesh::read::{list_mesh_names, read_mesh};
use crate::range::read_range;
use crate::types::{
    CopyDetection, EngineOptions, LayerSet, MeshConfig, MeshResolved, Range, RangeExtent,
    RangeLocation, RangeResolved, RangeStatus,
};
use crate::{Error, Result};
use similar::{ChangeTag, TextDiff};
use std::path::PathBuf;
use std::str::FromStr;

pub fn resolve_range(
    repo: &gix::Repository,
    mesh_name: &str,
    range_id: &str,
    options: EngineOptions,
) -> Result<RangeResolved> {
    require_committed_only(options.layers)?;
    let mesh = read_mesh(repo, mesh_name)?;
    match read_range(repo, range_id) {
        Ok(r) => resolve_range_inner(repo, &mesh.config, range_id, r),
        Err(Error::RangeNotFound(_)) => Ok(orphaned_placeholder(range_id)),
        Err(e) => Err(e),
    }
}

pub fn resolve_mesh(
    repo: &gix::Repository,
    name: &str,
    options: EngineOptions,
) -> Result<MeshResolved> {
    require_committed_only(options.layers)?;
    let mesh = read_mesh(repo, name)?;
    let mut ranges = Vec::with_capacity(mesh.ranges.len());
    for id in &mesh.ranges {
        match read_range(repo, id) {
            Ok(r) => ranges.push(resolve_range_inner(repo, &mesh.config, id, r)?),
            Err(Error::RangeNotFound(_)) => {
                ranges.push(orphaned_placeholder(id));
            }
            Err(e) => return Err(e),
        }
    }
    Ok(MeshResolved {
        name: mesh.name,
        message: mesh.message,
        ranges,
    })
}

pub fn culprit_commit(
    _repo: &gix::Repository,
    _resolved: &RangeResolved,
) -> Result<Option<String>> {
    todo!("culprit_commit lands in a later slice")
}

pub fn stale_meshes(repo: &gix::Repository, options: EngineOptions) -> Result<Vec<MeshResolved>> {
    require_committed_only(options.layers)?;
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

fn require_committed_only(layers: LayerSet) -> Result<()> {
    if layers.worktree || layers.index || layers.staged_mesh {
        todo!("layered (worktree / index / staged-mesh) modes pending later slice");
    }
    Ok(())
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
    }
}

// ---------------------------------------------------------------------------
// Internals.
// ---------------------------------------------------------------------------

fn resolve_range_inner(
    repo: &gix::Repository,
    cfg: &MeshConfig,
    range_id: &str,
    r: Range,
) -> Result<RangeResolved> {
    let (anchored_start, anchored_end) = match r.extent {
        RangeExtent::Lines { start, end } => (start, end),
        RangeExtent::Whole => todo!("whole-file extent pending later slice"),
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
        });
    }
    let current = resolve_current_location(repo, &r, cfg.copy_detection)?;
    let status = match &current {
        None => RangeStatus::Changed,
        Some(loc) => {
            let (cs, ce) = match loc.extent {
                RangeExtent::Lines { start, end } => (start, end),
                RangeExtent::Whole => todo!("whole-file extent pending later slice"),
            };
            let anchored_text = git::read_git_text(repo, &r.blob)?;
            let cur_blob_hex = match &loc.blob {
                Some(b) => b.to_string(),
                None => return Ok(RangeResolved {
                    range_id: range_id.into(),
                    anchor_sha: r.anchor_sha,
                    anchored,
                    current: Some(loc.clone()),
                    status: RangeStatus::Changed,
                }),
            };
            let current_text = git::read_git_text(repo, &cur_blob_hex)?;
            let anchored_lines: Vec<&str> = anchored_text.lines().collect();
            let current_lines: Vec<&str> = current_text.lines().collect();
            let a_lo = (anchored_start as usize).saturating_sub(1);
            let a_hi = (anchored_end as usize).min(anchored_lines.len());
            let c_lo = (cs as usize).saturating_sub(1);
            let c_hi = (ce as usize).min(current_lines.len());
            let a_slice = &anchored_lines[a_lo..a_hi];
            let c_slice = &current_lines[c_lo..c_hi];
            if lines_equal(a_slice, c_slice, cfg.ignore_whitespace) {
                if loc.path.as_path() == std::path::Path::new(&r.path)
                    && cs == anchored_start
                    && ce == anchored_end
                {
                    RangeStatus::Fresh
                } else {
                    RangeStatus::Moved
                }
            } else {
                RangeStatus::Changed
            }
        }
    };
    Ok(RangeResolved {
        range_id: range_id.into(),
        anchor_sha: r.anchor_sha,
        anchored,
        current,
        status,
    })
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
) -> Result<Option<RangeLocation>> {
    let (rstart, rend) = match r.extent {
        RangeExtent::Lines { start, end } => (start, end),
        RangeExtent::Whole => todo!("whole-file extent pending later slice"),
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
    let blob_hex = match git::path_blob_at(repo, &head_sha, &loc.path) {
        Ok(b) => b,
        Err(_) => return Ok(None),
    };
    let blob = oid_from_hex(&blob_hex).ok();
    Ok(Some(RangeLocation {
        path: PathBuf::from(loc.path),
        extent: RangeExtent::Lines {
            start: loc.start,
            end: loc.end,
        },
        blob,
    }))
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
    let mut start = loc.start as i64;
    let mut end = loc.end as i64;
    for (os, oc, _ns, nc) in hunks {
        let os = os as i64;
        let oc = oc as i64;
        let nc = nc as i64;
        let delta = nc - oc;
        let old_last = if oc == 0 { os } else { os + oc - 1 };
        if oc == 0 {
            if os < start {
                start += delta;
                end += delta;
            } else if os >= end {
                // no effect
            } else {
                end += delta;
            }
            continue;
        }
        if old_last < start {
            start += delta;
            end += delta;
        } else if os > end {
            // no effect
        } else {
            // overlap: collapse around the change
            let head_len = (os - start).max(0);
            let new_last = if nc == 0 { os } else { os + nc - 1 };
            start = (start.min(os)).max(1);
            let _ = head_len;
            end = new_last.max(end + delta);
        }
    }
    let s = start.max(1) as u32;
    let e = end.max(start) as u32;
    Ok((s, e))
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

//! Resolver: compute staleness for ranges and meshes (§5).

use crate::git::{self, work_dir};
use crate::mesh::read::{list_mesh_names, read_mesh};
use crate::range::read_range;
use crate::types::{
    CopyDetection, Mesh, MeshConfig, MeshResolved, Range, RangeLocation, RangeResolved, RangeStatus,
};
use crate::{Error, Result};
use similar::{ChangeTag, TextDiff};

pub fn resolve_range(
    repo: &gix::Repository,
    mesh_name: &str,
    range_id: &str,
) -> Result<RangeResolved> {
    let mesh = read_mesh(repo, mesh_name)?;
    match read_range(repo, range_id) {
        Ok(r) => resolve_range_inner(repo, &mesh.config, range_id, r),
        Err(Error::RangeNotFound(_)) => Ok(orphaned_placeholder(range_id)),
        Err(e) => Err(e),
    }
}

pub fn resolve_mesh(repo: &gix::Repository, name: &str) -> Result<MeshResolved> {
    let mesh = read_mesh(repo, name)?;
    let mut ranges = Vec::with_capacity(mesh.ranges.len());
    for id in &mesh.ranges {
        match read_range(repo, id) {
            Ok(r) => ranges.push(resolve_range_inner(repo, &mesh.config, id, r)?),
            Err(Error::RangeNotFound(_)) => {
                // The mesh references a range whose blob ref is gone. Surface
                // this as an ORPHANED finding rather than aborting the whole
                // command; hard errors are reserved for corruption that
                // prevents reading the mesh itself (§5.3).
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

fn orphaned_placeholder(range_id: &str) -> RangeResolved {
    RangeResolved {
        range_id: range_id.into(),
        anchor_sha: String::new(),
        anchored: RangeLocation {
            path: String::new(),
            start: 0,
            end: 0,
            blob: String::new(),
        },
        current: None,
        status: RangeStatus::Orphaned,
    }
}

pub fn culprit_commit(repo: &gix::Repository, resolved: &RangeResolved) -> Result<Option<String>> {
    if resolved.status != RangeStatus::Changed {
        return Ok(None);
    }
    let _wd = work_dir(repo)?;
    let current = match &resolved.current {
        Some(c) => c,
        None => return Ok(None),
    };
    let anchored_text = git::read_git_text(repo, &resolved.anchored.blob)?;
    let anchored_lines: Vec<&str> = anchored_text.lines().collect();
    let a_lo = (resolved.anchored.start as usize).saturating_sub(1);
    let a_hi = (resolved.anchored.end as usize).min(anchored_lines.len());
    let anchored_slice: Vec<&str> = anchored_lines[a_lo..a_hi].to_vec();
    let current_text = git::read_git_text(repo, &current.blob)?;
    let current_lines: Vec<&str> = current_text.lines().collect();
    let c_lo = (current.start as usize).saturating_sub(1);
    let c_hi = (current.end as usize).min(current_lines.len());
    let current_slice: Vec<&str> = current_lines[c_lo..c_hi].to_vec();

    blame_culprit(
        repo,
        &current.path,
        current.start,
        &anchored_slice,
        &current_slice,
        false,
    )
}

pub fn stale_meshes(repo: &gix::Repository) -> Result<Vec<MeshResolved>> {
    let names = list_mesh_names(repo)?;
    let mut out = Vec::with_capacity(names.len());
    for name in names {
        out.push(resolve_mesh(repo, &name)?);
    }
    // Worst-first: highest status among ranges, descending.
    out.sort_by(|a, b| {
        let max_a = a
            .ranges
            .iter()
            .map(|r| r.status)
            .max()
            .unwrap_or(RangeStatus::Fresh);
        let max_b = b
            .ranges
            .iter()
            .map(|r| r.status)
            .max()
            .unwrap_or(RangeStatus::Fresh);
        max_b.cmp(&max_a)
    });
    Ok(out)
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
    let _wd = work_dir(repo)?;
    let anchored = RangeLocation {
        path: r.path.clone(),
        start: r.start,
        end: r.end,
        blob: r.blob.clone(),
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
            let anchored_text = git::read_git_text(repo, &r.blob)?;
            let current_text = git::read_git_text(repo, &loc.blob)?;
            let anchored_lines: Vec<&str> = anchored_text.lines().collect();
            let current_lines: Vec<&str> = current_text.lines().collect();
            let a_lo = (r.start as usize).saturating_sub(1);
            let a_hi = (r.end as usize).min(anchored_lines.len());
            let c_lo = (loc.start as usize).saturating_sub(1);
            let c_hi = (loc.end as usize).min(current_lines.len());
            let a_slice = &anchored_lines[a_lo..a_hi];
            let c_slice = &current_lines[c_lo..c_hi];
            if lines_equal(a_slice, c_slice, cfg.ignore_whitespace) {
                if loc.path == r.path && loc.start == r.start && loc.end == r.end {
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
    let head_sha = git::head_oid(repo)?;
    // Walk anchor..HEAD in chronological order (oldest first).
    let mut commits =
        git::rev_walk_excluding(repo, &[&head_sha], &[&r.anchor_sha], None).unwrap_or_default();
    commits.reverse();
    let mut loc = Tracked {
        path: r.path.clone(),
        start: r.start,
        end: r.end,
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
    let blob = match git::path_blob_at(repo, &head_sha, &loc.path) {
        Ok(b) => b,
        Err(_) => return Ok(None),
    };
    Ok(Some(RangeLocation {
        path: loc.path,
        start: loc.start,
        end: loc.end,
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
    // Obtain old/new blob contents via tree lookups.
    let old_text = git::path_blob_at(repo, parent, &loc.path)
        .and_then(|b| git::read_git_text(repo, &b))
        .unwrap_or_default();
    let new_text = git::path_blob_at(repo, commit, new_path)
        .and_then(|b| git::read_git_text(repo, &b))
        .unwrap_or_default();
    let hunks = compute_hunks(&old_text, &new_text);
    let mut start = loc.start as i64;
    let mut end = loc.end as i64;
    for (os, oc, ns, nc) in hunks {
        let os = os as i64;
        let oc = oc as i64;
        let ns = ns as i64;
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
            let tail_len = (end - old_last).max(0);
            let head_len = (os - start).max(0);
            start = ns - head_len;
            if start < 1 {
                start = 1;
            }
            let new_last = if nc == 0 { ns } else { ns + nc - 1 };
            end = new_last + tail_len;
        }
    }
    let s = start.max(1) as u32;
    let e = end.max(start) as u32;
    Ok((s, e))
}

/// Compute `diff -U0`-style hunks between two blob texts, returning
/// `(old_start, old_count, new_start, new_count)` in 1-based line numbers.
/// Pure `(0,0)`-origin hunks follow git's convention: an insertion before
/// line 1 has `old_start=0, old_count=0`; a deletion tail at line N (old)
/// with no new content has `new_start=N-1, new_count=0` — matching the
/// hunk-apply logic in [`compute_new_range`].
fn compute_hunks(old: &str, new: &str) -> Vec<(u32, u32, u32, u32)> {
    let a: Vec<&str> = old.lines().collect();
    let b: Vec<&str> = new.lines().collect();
    let diff = TextDiff::from_slices(&a, &b);
    let mut hunks: Vec<(u32, u32, u32, u32)> = Vec::new();
    // Walk changes and bundle contiguous Delete/Insert runs into a hunk.
    let mut cur_old_start: Option<usize> = None;
    let mut cur_new_start: Option<usize> = None;
    let mut cur_oc: u32 = 0;
    let mut cur_nc: u32 = 0;
    // Track 1-based positions for the next expected delete/insert.
    let mut next_old_line: usize = 1;
    let mut next_new_line: usize = 1;
    for change in diff.iter_all_changes() {
        match change.tag() {
            ChangeTag::Equal => {
                if cur_old_start.is_some() || cur_new_start.is_some() {
                    let os = cur_old_start.unwrap_or(next_old_line.saturating_sub(1));
                    let ns = cur_new_start.unwrap_or(next_new_line.saturating_sub(1));
                    // Emulate git's convention: pure inserts use old_start = insertion_point - 1
                    // (the old line AFTER which the insertion occurs).
                    let (emitted_os, emitted_ns) = if cur_oc == 0 {
                        // pure insert: old_start = line before insertion (0 if at top)
                        (next_old_line.saturating_sub(1), ns)
                    } else if cur_nc == 0 {
                        // pure delete: new_start = line before deletion (0 if at top)
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
    use std::str::FromStr;
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
    // Enable rename tracking; copy detection is controlled per-config.
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

fn blame_culprit(
    repo: &gix::Repository,
    path: &str,
    start: u32,
    anchored: &[&str],
    current: &[&str],
    ignore_ws: bool,
) -> Result<Option<String>> {
    let lines = differing_lines(start, anchored, current, ignore_ws);
    let head = match repo.head_id() {
        Ok(h) => h.detach(),
        Err(_) => return Ok(None),
    };
    // Compute one blame pass and cover all requested lines from its entries.
    let path_bstr: &gix::bstr::BStr = path.as_bytes().into();
    let outcome = match repo.blame_file(path_bstr, head, Default::default()) {
        Ok(o) => o,
        Err(_) => return Ok(None),
    };
    let mut newest: Option<(i64, String)> = None;
    for ln in lines {
        // gix blame entries use 0-based line ranges.
        let target = ln.saturating_sub(1);
        let Some(entry) = outcome.entries.iter().find(|e| {
            target >= e.start_in_blamed_file && target < e.start_in_blamed_file + e.len.get()
        }) else {
            continue;
        };
        let oid = entry.commit_id.to_string();
        let ts = match repo.find_commit(entry.commit_id) {
            Ok(c) => c
                .decode()
                .ok()
                .and_then(|d| d.committer().ok().and_then(|s| s.time().ok()))
                .map(|t| t.seconds)
                .unwrap_or(0),
            Err(_) => 0,
        };
        match &newest {
            Some((t, _)) if *t >= ts => {}
            _ => newest = Some((ts, oid)),
        }
    }
    Ok(newest.map(|(_, oid)| oid))
}

fn differing_lines(start: u32, a: &[&str], b: &[&str], ignore_ws: bool) -> Vec<u32> {
    let an: Vec<String> = a.iter().map(|s| normalize(s, ignore_ws)).collect();
    let bn: Vec<String> = b.iter().map(|s| normalize(s, ignore_ws)).collect();
    let ar: Vec<&str> = an.iter().map(String::as_str).collect();
    let br: Vec<&str> = bn.iter().map(String::as_str).collect();
    let diff = TextDiff::from_slices(&ar, &br);
    let mut lines = Vec::new();
    for change in diff.iter_all_changes() {
        if change.tag() == ChangeTag::Insert
            && let Some(idx) = change.new_index()
        {
            lines.push(start + idx as u32);
        }
    }
    if lines.is_empty() {
        lines.push(start);
    }
    lines
}

fn normalize(s: &str, ignore_ws: bool) -> String {
    if ignore_ws {
        s.split_whitespace().collect()
    } else {
        s.to_string()
    }
}

#[allow(dead_code)]
fn _kept(_: &Mesh) {}

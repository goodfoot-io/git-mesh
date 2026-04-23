//! Resolver: compute staleness for ranges and meshes (§5).

use crate::git::{git_stdout, work_dir};
use crate::mesh::read::{list_mesh_names, read_mesh};
use crate::range::read_range;
use crate::types::{
    CopyDetection, Mesh, MeshConfig, MeshResolved, Range, RangeLocation, RangeResolved, RangeStatus,
};
use crate::{Error, Result};
use similar::{ChangeTag, TextDiff};
use std::path::Path;

pub fn resolve_range(
    repo: &gix::Repository,
    mesh_name: &str,
    range_id: &str,
) -> Result<RangeResolved> {
    let mesh = read_mesh(repo, mesh_name)?;
    let r = read_range(repo, range_id)?;
    resolve_range_inner(repo, &mesh.config, range_id, r)
}

pub fn resolve_mesh(repo: &gix::Repository, name: &str) -> Result<MeshResolved> {
    let mesh = read_mesh(repo, name)?;
    let mut ranges = Vec::with_capacity(mesh.ranges.len());
    for id in &mesh.ranges {
        let r = read_range(repo, id)?;
        ranges.push(resolve_range_inner(repo, &mesh.config, id, r)?);
    }
    Ok(MeshResolved {
        name: mesh.name,
        message: mesh.message,
        ranges,
    })
}

pub fn culprit_commit(
    repo: &gix::Repository,
    resolved: &RangeResolved,
) -> Result<Option<String>> {
    if resolved.status != RangeStatus::Changed {
        return Ok(None);
    }
    let wd = work_dir(repo)?;
    let current = match &resolved.current {
        Some(c) => c,
        None => return Ok(None),
    };
    let anchored_text = git_stdout(wd, ["cat-file", "-p", &resolved.anchored.blob])?;
    let anchored_lines: Vec<&str> = anchored_text.lines().collect();
    let a_lo = (resolved.anchored.start as usize).saturating_sub(1);
    let a_hi = (resolved.anchored.end as usize).min(anchored_lines.len());
    let anchored_slice: Vec<&str> = anchored_lines[a_lo..a_hi].to_vec();
    let current_text = git_stdout(wd, ["cat-file", "-p", &current.blob])?;
    let current_lines: Vec<&str> = current_text.lines().collect();
    let c_lo = (current.start as usize).saturating_sub(1);
    let c_hi = (current.end as usize).min(current_lines.len());
    let current_slice: Vec<&str> = current_lines[c_lo..c_hi].to_vec();

    blame_culprit(
        wd,
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
        let max_a = a.ranges.iter().map(|r| r.status).max().unwrap_or(RangeStatus::Fresh);
        let max_b = b.ranges.iter().map(|r| r.status).max().unwrap_or(RangeStatus::Fresh);
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
    let wd = work_dir(repo)?;
    let anchored = RangeLocation {
        path: r.path.clone(),
        start: r.start,
        end: r.end,
        blob: r.blob.clone(),
    };
    if !is_commit_reachable(wd, &r.anchor_sha)? {
        return Ok(RangeResolved {
            range_id: range_id.into(),
            anchor_sha: r.anchor_sha,
            anchored,
            current: None,
            status: RangeStatus::Orphaned,
        });
    }
    let current = resolve_current_location(wd, &r, cfg.copy_detection)?;
    let status = match &current {
        None => RangeStatus::Changed,
        Some(loc) => {
            let anchored_text = git_stdout(wd, ["cat-file", "-p", &r.blob])?;
            let current_text = git_stdout(wd, ["cat-file", "-p", &loc.blob])?;
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

fn is_commit_reachable(work_dir: &Path, commit: &str) -> Result<bool> {
    let output = git_stdout(
        work_dir,
        [
            "for-each-ref",
            "--format=%(refname)",
            "--contains",
            commit,
            "refs",
        ],
    );
    Ok(output.map(|o| o.lines().any(|l| !l.is_empty())).unwrap_or(false))
}

#[derive(Clone, Debug)]
struct Tracked {
    path: String,
    start: u32,
    end: u32,
}

fn resolve_current_location(
    wd: &Path,
    r: &Range,
    copy_detection: CopyDetection,
) -> Result<Option<RangeLocation>> {
    let head_sha = git_stdout(wd, ["rev-parse", "HEAD"])?;
    let commits = git_stdout(
        wd,
        [
            "rev-list",
            "--ancestry-path",
            "--reverse",
            &format!("{}..{head_sha}", r.anchor_sha),
        ],
    )
    .unwrap_or_default();
    let mut loc = Tracked {
        path: r.path.clone(),
        start: r.start,
        end: r.end,
    };
    let mut parent = r.anchor_sha.clone();
    for commit in commits.lines().filter(|l| !l.is_empty()) {
        match advance(wd, &parent, commit, &loc, copy_detection)? {
            Change::Unchanged => {}
            Change::Deleted => return Ok(None),
            Change::Updated(next) => loc = next,
        }
        parent = commit.into();
    }
    let blob = match git_stdout(wd, ["rev-parse", &format!("HEAD:{}", loc.path)]) {
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
    wd: &Path,
    parent: &str,
    commit: &str,
    loc: &Tracked,
    copy_detection: CopyDetection,
) -> Result<Change> {
    let entries = name_status(wd, parent, commit, copy_detection)?;
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
            let (s, e) = compute_new_range(wd, parent, commit, loc, &p)?;
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
    let (s, e) = compute_new_range(wd, parent, commit, loc, &p)?;
    Ok(Change::Updated(Tracked {
        path: p,
        start: s,
        end: e,
    }))
}

fn compute_new_range(
    wd: &Path,
    parent: &str,
    commit: &str,
    loc: &Tracked,
    new_path: &str,
) -> Result<(u32, u32)> {
    let output = if new_path == loc.path {
        git_stdout(wd, ["diff", "-U0", parent, commit, "--", &loc.path]).unwrap_or_default()
    } else {
        git_stdout(
            wd,
            [
                "diff",
                "-U0",
                "--find-renames",
                "--find-copies",
                parent,
                commit,
                "--",
                &loc.path,
                new_path,
            ],
        )
        .unwrap_or_default()
    };
    let mut start = loc.start as i64;
    let mut end = loc.end as i64;
    for line in output.lines() {
        let Some(rest) = line.strip_prefix("@@ -") else {
            continue;
        };
        let Some((old, new_rest)) = rest.split_once(" +") else {
            continue;
        };
        let Some((new, _)) = new_rest.split_once(" @@") else {
            continue;
        };
        let (os, oc) = parse_hunk(old)?;
        let (ns, nc) = parse_hunk(new)?;
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

fn parse_hunk(text: &str) -> Result<(u32, u32)> {
    let (start, count) = match text.split_once(',') {
        Some((s, c)) => (
            s.parse().map_err(|_| Error::Parse("bad hunk start".into()))?,
            c.parse().map_err(|_| Error::Parse("bad hunk count".into()))?,
        ),
        None => (
            text.parse().map_err(|_| Error::Parse("bad hunk".into()))?,
            1,
        ),
    };
    Ok((start, count))
}

enum NS {
    Added { path: String },
    Modified { path: String },
    Deleted { path: String },
    Renamed { from: String, to: String },
    Copied { from: String, to: String },
}

fn name_status(
    wd: &Path,
    parent: &str,
    commit: &str,
    copy_detection: CopyDetection,
) -> Result<Vec<NS>> {
    let mut args = vec![
        "diff-tree".to_string(),
        "--no-commit-id".to_string(),
        "--name-status".to_string(),
        "-r".to_string(),
        "-M".to_string(),
    ];
    for a in copy_detection_args(copy_detection) {
        args.push(a.to_string());
    }
    args.push(parent.into());
    args.push(commit.into());
    let output = git_stdout(wd, args.iter().map(String::as_str))?;
    let mut out = Vec::new();
    for line in output.lines().filter(|l| !l.is_empty()) {
        let mut parts = line.split('\t');
        let status = parts.next().unwrap_or_default();
        match status.chars().next() {
            Some('A') => {
                if let Some(p) = parts.next() {
                    out.push(NS::Added { path: p.into() });
                }
            }
            Some('M') => {
                if let Some(p) = parts.next() {
                    out.push(NS::Modified { path: p.into() });
                }
            }
            Some('D') => {
                if let Some(p) = parts.next() {
                    out.push(NS::Deleted { path: p.into() });
                }
            }
            Some('R') => {
                let from = parts.next().unwrap_or_default().into();
                let to = parts.next().unwrap_or_default().into();
                out.push(NS::Renamed { from, to });
            }
            Some('C') => {
                let from = parts.next().unwrap_or_default().into();
                let to = parts.next().unwrap_or_default().into();
                out.push(NS::Copied { from, to });
            }
            _ => {}
        }
    }
    Ok(out)
}

fn copy_detection_args(cd: CopyDetection) -> Vec<&'static str> {
    match cd {
        CopyDetection::Off => Vec::new(),
        CopyDetection::SameCommit => vec!["-C"],
        CopyDetection::AnyFileInCommit => vec!["-C", "-C"],
        CopyDetection::AnyFileInRepo => vec!["-C", "-C", "-C"],
    }
}

fn blame_culprit(
    wd: &Path,
    path: &str,
    start: u32,
    anchored: &[&str],
    current: &[&str],
    ignore_ws: bool,
) -> Result<Option<String>> {
    let lines = differing_lines(start, anchored, current, ignore_ws);
    let mut newest: Option<(i64, String)> = None;
    for ln in lines {
        let output = git_stdout(
            wd,
            [
                "blame",
                "--porcelain",
                "-L",
                &format!("{ln},{ln}"),
                "HEAD",
                "--",
                path,
            ],
        );
        let Ok(output) = output else { continue };
        if let Some((ts, oid)) = parse_blame(&output) {
            match &newest {
                Some((t, _)) if *t >= ts => {}
                _ => newest = Some((ts, oid)),
            }
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

fn parse_blame(output: &str) -> Option<(i64, String)> {
    let mut lines = output.lines();
    let oid = lines.next()?.split_whitespace().next()?.to_string();
    let mut ts: Option<i64> = None;
    for line in lines {
        if let Some(v) = line.strip_prefix("committer-time ") {
            ts = v.parse().ok();
        }
        if line.starts_with('\t') {
            break;
        }
    }
    Some((ts?, oid))
}

#[allow(dead_code)]
fn _kept(_: &Mesh) {}

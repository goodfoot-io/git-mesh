//! HEAD-history walker. Translates an anchored `(path, line-range)` from
//! its anchor commit forward through `anchor..HEAD` by replaying each
//! commit's name-status and hunk diffs against the tracked location.

use crate::git;
use crate::range::read_range as _read_range;
use crate::types::{CopyDetection, Range, RangeExtent};
use crate::{Error, Result};
use similar::{ChangeTag, TextDiff};
use std::str::FromStr;

#[derive(Clone, Debug)]
pub(crate) struct Tracked {
    pub(crate) path: String,
    pub(crate) start: u32,
    pub(crate) end: u32,
}

pub(crate) enum Change {
    Unchanged,
    Deleted,
    Updated(Tracked),
}

pub(crate) const RENAME_BUDGET_DEFAULT: usize = 1000;

pub(crate) fn rename_budget() -> usize {
    std::env::var("GIT_MESH_RENAME_BUDGET")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(RENAME_BUDGET_DEFAULT)
}

pub(crate) fn resolve_at_head(
    repo: &gix::Repository,
    r: &Range,
    copy_detection: CopyDetection,
    warnings: &mut Vec<String>,
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
        match advance(repo, &parent, commit, &loc, copy_detection, warnings)? {
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

pub(crate) fn advance(
    repo: &gix::Repository,
    parent: &str,
    commit: &str,
    loc: &Tracked,
    copy_detection: CopyDetection,
    warnings: &mut Vec<String>,
) -> Result<Change> {
    let entries = name_status(repo, parent, commit, copy_detection, warnings)?;
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
            return Ok(Change::Updated(Tracked { path: p, start: s, end: e }));
        }
        return Ok(Change::Deleted);
    }
    if !modified {
        return Ok(Change::Unchanged);
    }
    let p = next_path.unwrap_or_else(|| loc.path.clone());
    let (s, e) = compute_new_range(repo, parent, commit, loc, &p)?;
    Ok(Change::Updated(Tracked { path: p, start: s, end: e }))
}

pub(crate) fn compute_new_range(
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

pub(crate) fn compute_hunks(old: &str, new: &str) -> Vec<(u32, u32, u32, u32)> {
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

pub(crate) fn apply_hunks_to_range(
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

pub(crate) enum NS {
    Added { path: String },
    Modified { path: String },
    Deleted { path: String },
    Renamed { from: String, to: String },
    Copied { from: String, to: String },
}

pub(crate) fn name_status(
    repo: &gix::Repository,
    parent: &str,
    commit: &str,
    copy_detection: CopyDetection,
    warnings: &mut Vec<String>,
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
    let budget = rename_budget();
    let want_rewrites = true;

    // First pass: cheap, no rewrite pairing.
    let raw = collect_changes(&parent_tree, &new_tree, copy_detection, false)?;
    if raw.len() > budget && want_rewrites {
        warnings.push(format!(
            "warning: rename detection disabled (--no-renames) for HEAD walk {}..{}; {} > GIT_MESH_RENAME_BUDGET={}",
            &parent[..parent.len().min(8)],
            &commit[..commit.len().min(8)],
            raw.len(),
            budget,
        ));
        return Ok(raw);
    }
    collect_changes(&parent_tree, &new_tree, copy_detection, true)
}

fn collect_changes<'a>(
    parent_tree: &gix::Tree<'a>,
    new_tree: &gix::Tree<'a>,
    copy_detection: CopyDetection,
    track_rewrites: bool,
) -> Result<Vec<NS>> {
    let mut platform = parent_tree
        .changes()
        .map_err(|e| Error::Git(format!("tree changes: {e}")))?;
    platform.options(|opts| {
        let want_copies = !matches!(copy_detection, CopyDetection::Off);
        if track_rewrites {
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
        } else {
            opts.track_path().track_rewrites(None);
        }
    });
    let mut out = Vec::new();
    platform
        .for_each_to_obtain_tree(new_tree, |change| -> Result<std::ops::ControlFlow<()>> {
            use gix::object::tree::diff::Change as DC;
            match change {
                DC::Addition { location, .. } => out.push(NS::Added { path: location.to_string() }),
                DC::Deletion { location, .. } => out.push(NS::Deleted { path: location.to_string() }),
                DC::Modification { location, .. } => out.push(NS::Modified { path: location.to_string() }),
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

// Silence unused-import warning until engine module wires this through.
#[allow(dead_code)]
fn _keep(_: fn(&gix::Repository, &str) -> Result<crate::types::Range>) {
    let _ = _read_range;
}

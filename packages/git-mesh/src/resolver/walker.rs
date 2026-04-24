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

#[derive(Clone)]
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

    // Second pass: rewrite-tracked diff-pair results.
    let mut entries = collect_changes(&parent_tree, &new_tree, copy_detection, true)?;

    // For AnyFileInCommit and AnyFileInRepo, run a widened similarity
    // search for added paths that were not already paired by the first pass.
    // Clone entries snapshot for use as first_pass so the mutable borrow is
    // separate from the snapshot borrow.
    match copy_detection {
        CopyDetection::Off | CopyDetection::SameCommit => {}
        CopyDetection::AnyFileInCommit => {
            let snapshot = entries.clone();
            widen_copies_in_commit(repo, &new_tree, &snapshot, &mut entries, commit, parent)?;
        }
        CopyDetection::AnyFileInRepo => {
            let snapshot = entries.clone();
            let _ = widen_copies_any_ref(
                repo,
                &new_tree,
                &snapshot,
                &mut entries,
                commit,
                parent,
                warnings,
            )?;
        }
    }

    Ok(entries)
}

/// Result of the AnyFileInRepo widening attempt.
enum WidenResult {
    Done,
    FellBack,
}

/// Collect all blob paths from `tree` into a `Vec<(path, blob_oid_string)>`.
fn tree_blob_paths(tree: &gix::Tree<'_>) -> Result<Vec<(String, String)>> {
    let mut out = Vec::new();
    tree.traverse()
        .breadthfirst
        .files()
        .map_err(|e| Error::Git(format!("tree traverse: {e}")))?
        .into_iter()
        .for_each(|entry| {
            let path = entry.filepath.to_string();
            let oid = entry.oid.to_string();
            out.push((path, oid));
        });
    Ok(out)
}

/// Collect all blob paths from every ref's tree, deduped by blob OID.
/// Returns `(path, blob_oid)` pairs with unique blob OIDs.
fn all_ref_blob_paths(repo: &gix::Repository) -> Result<Vec<(String, String)>> {
    use std::collections::HashSet;
    let mut seen_oids: HashSet<String> = HashSet::new();
    let mut out: Vec<(String, String)> = Vec::new();

    let refs = repo
        .references()
        .map_err(|e| Error::Git(format!("refs: {e}")))?;
    let all = refs
        .all()
        .map_err(|e| Error::Git(format!("refs all: {e}")))?;

    for r in all {
        let mut r = match r {
            Ok(r) => r,
            Err(_) => continue,
        };
        let tip_id = match r.peel_to_id() {
            Ok(id) => id.detach(),
            Err(_) => continue,
        };
        // Try to peel to a tree (works for commit refs, skips tags to blobs etc.)
        let obj = match repo.find_object(tip_id) {
            Ok(o) => o,
            Err(_) => continue,
        };
        let tree = match obj.peel_to_tree() {
            Ok(t) => t,
            Err(_) => continue,
        };
        let entries = tree_blob_paths(&tree).unwrap_or_default();
        for (path, oid) in entries {
            if seen_oids.insert(oid.clone()) {
                out.push((path, oid));
            }
        }
    }
    Ok(out)
}

/// Compute line-based Jaccard similarity between two text blobs.
/// Returns a value in [0.0, 1.0]. Uses multiset intersection/union of lines.
fn line_similarity(a: &str, b: &str) -> f64 {
    use std::collections::HashMap;
    let mut counts_a: HashMap<&str, i32> = HashMap::new();
    let mut counts_b: HashMap<&str, i32> = HashMap::new();
    for line in a.lines() {
        *counts_a.entry(line).or_default() += 1;
    }
    for line in b.lines() {
        *counts_b.entry(line).or_default() += 1;
    }
    let mut intersection = 0i32;
    let mut union = 0i32;
    for (line, &ca) in &counts_a {
        let cb = counts_b.get(line).copied().unwrap_or(0);
        intersection += ca.min(cb);
        union += ca.max(cb);
    }
    for (line, &cb) in &counts_b {
        if !counts_a.contains_key(line) {
            union += cb;
        }
    }
    if union == 0 {
        return 0.0;
    }
    intersection as f64 / union as f64
}

/// Read a blob OID as text, returning empty string on failure.
fn blob_text(repo: &gix::Repository, blob_oid: &str) -> String {
    use std::str::FromStr;
    let Ok(oid) = gix::ObjectId::from_str(blob_oid) else {
        return String::new();
    };
    let Ok(obj) = repo.find_object(oid) else {
        return String::new();
    };
    String::from_utf8(obj.into_blob().detach().data).unwrap_or_default()
}

/// Collect paths that are "added" in `entries` but not paired as a copy/rename source.
fn unpaired_added_paths(entries: &[NS]) -> Vec<String> {
    let mut paired: std::collections::HashSet<String> = std::collections::HashSet::new();
    for e in entries {
        match e {
            NS::Renamed { to, .. } | NS::Copied { to, .. } => {
                paired.insert(to.clone());
            }
            _ => {}
        }
    }
    entries
        .iter()
        .filter_map(|e| match e {
            NS::Added { path } if !paired.contains(path) => Some(path.clone()),
            _ => None,
        })
        .collect()
}

/// For each unpaired added path in the diff, search `candidates` for a source
/// blob with similarity >= 50%. If found, push a `NS::Copied` entry.
fn match_copies_from_pool(
    repo: &gix::Repository,
    commit: &str,
    added_paths: &[String],
    candidates: &[(String, String)], // (path, blob_oid)
    out: &mut Vec<NS>,
) -> Result<()> {
    // Build the blob OIDs for each added path in the new commit.
    let added_blobs: Vec<(String, String)> = added_paths
        .iter()
        .filter_map(|p| {
            git::path_blob_at(repo, commit, p).ok().map(|oid| (p.clone(), oid))
        })
        .collect();

    // For each candidate, read text once; then compare against each added.
    // Greedy: first candidate that beats threshold wins (stable ordering from pool).
    for (added_path, added_blob_oid) in &added_blobs {
        let added_text = blob_text(repo, added_blob_oid);
        if added_text.is_empty() {
            continue;
        }
        let mut best_sim = 0.0f64;
        let mut best_src: Option<String> = None;
        for (cand_path, cand_blob_oid) in candidates {
            // Skip if candidate is the same path as the added file.
            // (Identical blob OIDs from different paths are valid copy sources.)
            if cand_path == added_path {
                continue;
            }
            let cand_text = blob_text(repo, cand_blob_oid);
            if cand_text.is_empty() {
                continue;
            }
            let sim = line_similarity(&cand_text, &added_text);
            if sim >= 0.5 && sim > best_sim {
                best_sim = sim;
                best_src = Some(cand_path.clone());
            }
        }
        if let Some(src) = best_src {
            out.push(NS::Copied {
                from: src,
                to: added_path.clone(),
            });
        }
    }
    Ok(())
}

/// Widen copy detection to every path in `new_tree` (AnyFileInCommit).
fn widen_copies_in_commit(
    repo: &gix::Repository,
    new_tree: &gix::Tree<'_>,
    first_pass: &[NS],
    out: &mut Vec<NS>,
    commit: &str,
    _parent: &str,
) -> Result<()> {
    let added_paths = unpaired_added_paths(first_pass);
    if added_paths.is_empty() {
        return Ok(());
    }
    // Pool: all blobs in new_tree not already in the added set.
    let added_set: std::collections::HashSet<&str> =
        added_paths.iter().map(|s| s.as_str()).collect();
    let pool: Vec<(String, String)> = tree_blob_paths(new_tree)?
        .into_iter()
        .filter(|(path, _)| !added_set.contains(path.as_str()))
        .collect();
    match_copies_from_pool(repo, commit, &added_paths, &pool, out)
}

/// Widen copy detection to every blob in any ref (AnyFileInRepo).
/// Falls back to AnyFileInCommit if the pool exceeds the budget.
fn widen_copies_any_ref(
    repo: &gix::Repository,
    new_tree: &gix::Tree<'_>,
    first_pass: &[NS],
    out: &mut Vec<NS>,
    commit: &str,
    parent: &str,
    warnings: &mut Vec<String>,
) -> Result<WidenResult> {
    let added_paths = unpaired_added_paths(first_pass);
    if added_paths.is_empty() {
        return Ok(WidenResult::Done);
    }
    let budget = rename_budget();
    let pool = all_ref_blob_paths(repo)?;
    if pool.len() > budget {
        warnings.push(format!(
            "warning: AnyFileInRepo copy pool ({} blobs) exceeds GIT_MESH_RENAME_BUDGET={}; falling back to AnyFileInCommit for HEAD walk {}..{}",
            pool.len(),
            budget,
            &parent[..parent.len().min(8)],
            &commit[..commit.len().min(8)],
        ));
        widen_copies_in_commit(repo, new_tree, first_pass, out, commit, parent)?;
        return Ok(WidenResult::FellBack);
    }
    let added_set: std::collections::HashSet<&str> =
        added_paths.iter().map(|s| s.as_str()).collect();
    let pool_filtered: Vec<(String, String)> = pool
        .into_iter()
        .filter(|(path, _)| !added_set.contains(path.as_str()))
        .collect();
    match_copies_from_pool(repo, commit, &added_paths, &pool_filtered, out)?;
    Ok(WidenResult::Done)
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

#[cfg(test)]
mod scope_tests {
    use super::*;
    use std::process::Command;
    use tempfile::tempdir;

    fn run_git(dir: &std::path::Path, args: &[&str]) {
        let out = Command::new("git").current_dir(dir).args(args).output().unwrap();
        assert!(out.status.success(), "git {:?} failed: {}", args, String::from_utf8_lossy(&out.stderr));
    }

    /// Unit test: verifies that `name_status` with `AnyFileInCommit` produces a
    /// `NS::Copied` entry when b.ts copies content from an unmodified a.ts.
    #[test]
    fn widen_any_file_in_commit_produces_copied_entry() {
        let td = tempdir().unwrap();
        let dir = td.path();
        run_git(dir, &["init", "--initial-branch=main"]);
        run_git(dir, &["config", "user.email", "t@t"]);
        run_git(dir, &["config", "user.name", "t"]);
        run_git(dir, &["config", "commit.gpgsign", "false"]);
        
        let content: String = (1..=20).map(|i| format!("content_line_{i}\n")).collect();
        std::fs::write(dir.join("a.ts"), &content).unwrap();
        run_git(dir, &["add", "."]);
        run_git(dir, &["commit", "-m", "init"]);
        let parent = String::from_utf8(
            Command::new("git").current_dir(dir).args(["rev-parse", "HEAD"]).output().unwrap().stdout
        ).unwrap().trim().to_string();
        
        std::fs::write(dir.join("b.ts"), &content).unwrap();
        run_git(dir, &["add", "."]);
        run_git(dir, &["commit", "-m", "add b.ts"]);
        let commit = String::from_utf8(
            Command::new("git").current_dir(dir).args(["rev-parse", "HEAD"]).output().unwrap().stdout
        ).unwrap().trim().to_string();
        
        let repo = gix::open(dir).unwrap();
        let mut warnings = Vec::new();
        let entries = name_status(
            &repo,
            &parent,
            &commit,
            crate::types::CopyDetection::AnyFileInCommit,
            &mut warnings,
        )
        .unwrap();

        let has_copied = entries
            .iter()
            .any(|e| matches!(e, NS::Copied { from, to } if from == "a.ts" && to == "b.ts"));
        assert!(
            has_copied,
            "Expected Copied{{from=a.ts,to=b.ts}}; entries count={}, warnings={:?}",
            entries.len(),
            warnings
        );
    }
}

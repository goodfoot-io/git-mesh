//! Per-layer `git diff-{index,files}` parsing into `LayerDiffs`.

use super::super::walker::rename_budget;
use crate::git;
use crate::{Error, Result};
use std::collections::{HashMap, HashSet};
use std::process::Command;

/// Per-run, per-layer cache of `git diff-{index,files}` parses.
pub(crate) struct LayerDiffs {
    pub(crate) map: HashMap<String, DiffEntry>,
    pub(crate) renamed_from: HashMap<String, String>,
    pub(crate) rename_detection_disabled: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct DiffEntry {
    pub(crate) new_path: String,
    pub(crate) old_path: String,
    pub(crate) hunks: Vec<(u32, u32, u32, u32)>,
    pub(crate) new_blob: Option<String>,
    pub(crate) deleted: bool,
    pub(crate) intent_to_add: bool,
}

pub(crate) fn read_index_layer(
    repo: &gix::Repository,
    warnings: &mut Vec<String>,
) -> Result<LayerDiffs> {
    let workdir = git::work_dir(repo)?;
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
            &["diff-index", "--cached", "-U0", "--no-renames", "--full-index", "HEAD"],
        )?;
        let mut p = parse_diff_raw_unified(&out, false);
        p.rename_detection_disabled = true;
        return Ok(p.into_layer());
    }
    Ok(parsed.into_layer())
}

pub(crate) fn read_worktree_layer(
    repo: &gix::Repository,
    warnings: &mut Vec<String>,
) -> Result<LayerDiffs> {
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
    let out = Command::new("git")
        .current_dir(workdir)
        .args([
            "-c", "filter.lfs.process=",
            "-c", "filter.lfs.smudge=cat",
            "-c", "filter.lfs.clean=cat",
            "-c", "filter.lfs.required=false",
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
                map.insert(e.old_path.clone(), e.clone());
            }
            map.insert(e.new_path.clone(), e);
        }
        LayerDiffs { map, renamed_from, rename_detection_disabled: self.rename_detection_disabled }
    }
}

fn parse_diff_raw_unified(text: &str, worktree: bool) -> ParsedDiff {
    let mut entries: Vec<DiffEntry> = Vec::new();
    let mut cur: Option<DiffEntry> = None;
    let mut new_mode_zero = false;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("diff --git ") {
            if let Some(prev) = cur.take() {
                entries.push(prev);
            }
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
        let Some(e) = cur.as_mut() else { continue };
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
            if rest.trim() == "000000" {
                new_mode_zero = true;
                e.intent_to_add = true;
            }
            continue;
        }
        if let Some(rest) = line.strip_prefix("index ") {
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
    ParsedDiff { entries, rename_detection_disabled: false, entry_count }
}

fn parse_diff_paths(rest: &str) -> (String, String) {
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

pub(crate) fn read_conflicted_paths(repo: &gix::Repository) -> Result<HashSet<String>> {
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

pub(crate) fn read_index_trailer(repo: &gix::Repository) -> Result<[u8; 20]> {
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

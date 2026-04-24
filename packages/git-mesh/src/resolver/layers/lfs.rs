//! LFS first-class reader. Routes `filter=lfs` paths through a managed
//! `git-lfs filter-process` subprocess (lazy spawn, reused per run).

use super::filter_process::{
    FilterProcess, FilterSpawnError, filter_smudge, spawn_lfs_process,
};
use crate::git;
use crate::types::{
    self, DriftSource, Range, RangeExtent, RangeLocation, RangeResolved, RangeStatus,
    UnavailableReason,
};
use std::path::PathBuf;
use std::str::FromStr;

use super::super::walker::Tracked;

pub(crate) type LfsState = Option<std::result::Result<FilterProcess, FilterSpawnError>>;

pub(crate) fn is_lfs_path(repo: &gix::Repository, path: &str) -> bool {
    let Ok(workdir) = git::work_dir(repo) else {
        return false;
    };
    matches!(
        types::path_filter_attribute(workdir, std::path::Path::new(path)),
        Ok(Some(ref n)) if n == "lfs"
    )
}

pub(crate) fn lfs_pointer_oid(bytes: &[u8]) -> Option<String> {
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

enum LfsReadOutcome {
    Bytes(Vec<u8>),
    NotFetched,
    NotInstalled,
}

fn lfs_read(
    lfs: &mut LfsState,
    workdir: &std::path::Path,
    path: &str,
    pointer_bytes: &[u8],
) -> LfsReadOutcome {
    let Some(oid) = lfs_pointer_oid(pointer_bytes) else {
        return LfsReadOutcome::Bytes(pointer_bytes.to_vec());
    };
    if !lfs_object_cached(workdir, &oid) {
        return LfsReadOutcome::NotFetched;
    }
    if lfs.is_none() {
        *lfs = Some(spawn_lfs_process(workdir));
    }
    match lfs.as_mut().expect("just set") {
        Err(FilterSpawnError::NotInstalled) => LfsReadOutcome::NotInstalled,
        Err(FilterSpawnError::HandshakeFailed) => LfsReadOutcome::NotInstalled,
        Ok(p) => match filter_smudge(p, path, pointer_bytes) {
            Ok(b) => LfsReadOutcome::Bytes(b),
            Err(_) => LfsReadOutcome::NotInstalled,
        },
    }
}

fn oid_from_hex(hex: &str) -> Option<gix::ObjectId> {
    gix::ObjectId::from_str(hex).ok()
}

fn head_blob_for(repo: &gix::Repository, path: &str) -> Option<String> {
    let head_sha = git::head_oid(repo).ok()?;
    git::path_blob_at(repo, &head_sha, path).ok()
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn resolve_lfs_range(
    repo: &gix::Repository,
    lfs: &mut LfsState,
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
        Err(_) => {
            return lfs_terminal(range_id, r, anchored, UnavailableReason::IoError {
                message: "no workdir".into(),
            });
        }
    };

    let anchored_pointer = match git::read_blob_bytes(repo, &r.blob) {
        Ok(b) => b,
        Err(_) => {
            return lfs_terminal(range_id, r, anchored, UnavailableReason::IoError {
                message: format!("cannot read anchored blob {}", r.blob),
            });
        }
    };

    let current_pointer: Vec<u8> = match deepest_layer {
        DriftSource::Worktree => {
            if worktree_changed {
                std::fs::read(workdir.join(&tracked.path)).unwrap_or_default()
            } else if let Some(o) = index_blob_oid.map(|s| s.to_string()) {
                git::read_blob_bytes(repo, &o).unwrap_or_default()
            } else if let Some(o) = head_blob_for(repo, &tracked.path) {
                git::read_blob_bytes(repo, &o).unwrap_or_default()
            } else {
                std::fs::read(workdir.join(&tracked.path)).unwrap_or_default()
            }
        }
        DriftSource::Index => {
            let oid = index_blob_oid
                .map(|s| s.to_string())
                .or_else(|| head_blob_for(repo, &tracked.path));
            match oid {
                Some(o) => git::read_blob_bytes(repo, &o).unwrap_or_default(),
                None => Vec::new(),
            }
        }
        DriftSource::Head => match head_blob_for(repo, &tracked.path) {
            Some(o) => git::read_blob_bytes(repo, &o).unwrap_or_default(),
            None => Vec::new(),
        },
    };

    let anchored_oid = lfs_pointer_oid(&anchored_pointer);
    let current_oid = lfs_pointer_oid(&current_pointer);
    let same_path_extent = tracked.path == r.path
        && matches!(r.extent, RangeExtent::Lines { start, end } if start == tracked.start && end == tracked.end);
    if anchored_oid.is_some() && anchored_oid == current_oid {
        let status = if same_path_extent { RangeStatus::Fresh } else { RangeStatus::Moved };
        let source = if status == RangeStatus::Fresh { None } else { Some(deepest_layer) };
        let layer_sources = if source.is_some() { vec![deepest_layer] } else { vec![] };
        return RangeResolved {
            range_id: range_id.into(),
            anchor_sha: r.anchor_sha.clone(),
            anchored,
            current: Some(RangeLocation {
                path: PathBuf::from(&tracked.path),
                extent: RangeExtent::Lines { start: tracked.start, end: tracked.end },
                blob: oid_from_hex(&r.blob),
            }),
            status,
            source,
            layer_sources,
            acknowledged_by: None,
            culprit: None,
        };
    }

    let anchored_smudged = match lfs_read(lfs, workdir, &r.path, &anchored_pointer) {
        LfsReadOutcome::Bytes(b) => b,
        LfsReadOutcome::NotFetched => {
            return lfs_terminal(range_id, r, anchored, UnavailableReason::LfsNotFetched);
        }
        LfsReadOutcome::NotInstalled => {
            return lfs_terminal(range_id, r, anchored, UnavailableReason::LfsNotInstalled);
        }
    };
    let current_smudged = match lfs_read(lfs, workdir, &tracked.path, &current_pointer) {
        LfsReadOutcome::Bytes(b) => b,
        LfsReadOutcome::NotFetched => {
            return lfs_terminal(range_id, r, anchored, UnavailableReason::LfsNotFetched);
        }
        LfsReadOutcome::NotInstalled => {
            return lfs_terminal(range_id, r, anchored, UnavailableReason::LfsNotInstalled);
        }
    };

    let a_smudged_oid = lfs_pointer_oid(&anchored_smudged);
    let c_smudged_oid = lfs_pointer_oid(&current_smudged);
    if a_smudged_oid.is_some() || c_smudged_oid.is_some() {
        let status = if a_smudged_oid == c_smudged_oid {
            if same_path_extent { RangeStatus::Fresh } else { RangeStatus::Moved }
        } else {
            RangeStatus::Changed
        };
        let source = if status == RangeStatus::Fresh { None } else { Some(deepest_layer) };
        let layer_sources = if source.is_some() { vec![deepest_layer] } else { vec![] };
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
            layer_sources,
            acknowledged_by: None,
            culprit: None,
        };
    }

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
    let layer_sources = if source.is_some() { vec![deepest_layer] } else { vec![] };
    RangeResolved {
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
        layer_sources,
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
        layer_sources: vec![],
        acknowledged_by: None,
        culprit: None,
    }
}

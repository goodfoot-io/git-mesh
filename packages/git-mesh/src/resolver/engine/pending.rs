//! Acknowledgment matching + pending-finding builder.

use crate::git;
use crate::types::{
    PendingDrift, PendingFinding, RangeExtent, RangeResolved, RangeStatus, StagedOpRef,
    current_normalization_stamp,
};

/// Acknowledgment matching by `range_id` (plan §B2).
pub(crate) fn apply_acknowledgment(
    repo: &gix::Repository,
    mesh_name: &str,
    r: &mut RangeResolved,
) {
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
    let s = String::from_utf8_lossy(bytes).into_owned();
    s.replace("\r\n", "\n").into_bytes()
}

fn read_live_for_range(repo: &gix::Repository, r: &RangeResolved) -> Option<Vec<u8>> {
    let workdir = git::work_dir(repo).ok()?;
    let path = r
        .current
        .as_ref()
        .map(|c| c.path.clone())
        .unwrap_or(r.anchored.path.clone());
    let abs = workdir.join(&path);
    let bytes = std::fs::read(&abs).ok()?;
    let s = String::from_utf8_lossy(&bytes).into_owned();
    Some(s.replace("\r\n", "\n").into_bytes())
}

pub(crate) fn build_pending_findings(
    repo: &gix::Repository,
    mesh_name: &str,
) -> Vec<PendingFinding> {
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
                out.push(PendingFinding::Remove {
                    mesh: mesh_name.to_string(),
                    range_id: String::new(),
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
        match crate::git::path_blob_at(repo, anchor, &add.path) {
            Ok(blob) => crate::git::read_blob_bytes(repo, &blob).ok()?,
            Err(_) => return Some(PendingDrift::SidecarMismatch),
        }
    } else {
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

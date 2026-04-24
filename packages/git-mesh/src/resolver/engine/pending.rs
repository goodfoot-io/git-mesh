//! Acknowledgment matching + pending-finding builder.

use crate::git;
use crate::staging::{SidecarVerifyError, read_sidecar_verified};
use crate::types::{
    PendingDrift, PendingFinding, RangeExtent, RangeResolved, RangeStatus, StagedOpRef,
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
        // Slice 4: integrity-check the sidecar bytes before consuming.
        // A tampered sidecar must NOT acknowledge anything; the pending
        // finding renderer will surface the tamper separately.
        let side_bytes = match read_sidecar_verified(repo, mesh_name, add.line_number) {
            Ok(b) => b,
            Err(_) => continue,
        };
        let side_norm = normalize_for_compare(repo, &add.path, &side_bytes, &meta.stamp);
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

/// Return bytes ready for whole-file or sliced byte-equal comparison.
///
/// Slice 2 of the review plan. The previous implementation ran
/// `String::from_utf8_lossy(...).replace("\r\n", "\n")` on the raw
/// bytes — destroying any byte ≥ 0x80 that wasn't a valid UTF-8 lead
/// (PNG headers, gzip frames, every binary asset) by mapping it to
/// U+FFFD. We instead:
///
/// 1. Probe the path's `.gitattributes` `binary` attribute. If the
///    path is declared binary, return raw bytes — no normalization at
///    all. (Binary paths cannot meaningfully have CRLF rewriting
///    applied.)
/// 2. Otherwise, if the captured normalization stamp matches the
///    current stamp, return raw bytes (the sidecar bytes already
///    reflect today's filter rules).
/// 3. Otherwise, replace `\r\n` with `\n` at the byte level — no
///    UTF-8 round-trip — symmetrically for sidecar and live readers.
fn normalize_for_compare(
    repo: &gix::Repository,
    path: &str,
    bytes: &[u8],
    _captured: &crate::types::NormalizationStamp,
) -> Vec<u8> {
    if path_is_binary(repo, path) {
        return bytes.to_vec();
    }
    // Text paths: always CRLF→LF at the byte level. This is symmetric
    // with `read_live_for_range`. We deliberately do NOT short-circuit
    // when the captured stamp matches the current stamp — the previous
    // implementation did, and that asymmetry caused PNG-with-`\r\n`
    // bytes (sidecar untouched, live rewritten) to compare unequal even
    // though the underlying content was identical. `crlf_to_lf` is
    // idempotent on already-normalized bytes, so applying it
    // unconditionally costs at most one linear scan.
    crlf_to_lf(bytes)
}

/// Byte-level `\r\n` → `\n` rewriter. Preserves every other byte
/// exactly, including bytes that are not valid UTF-8 starts.
fn crlf_to_lf(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if i + 1 < bytes.len() && bytes[i] == b'\r' && bytes[i + 1] == b'\n' {
            out.push(b'\n');
            i += 2;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    out
}

/// Resolve the `.gitattributes` `binary` macro for `path`. Defaults to
/// `false` on any plumbing error — fail open here would be wrong, but
/// in practice an attribute lookup failure means we genuinely don't
/// know, and treating "unknown" as "text" preserves the prior comparison
/// shape for non-binary paths. Binary paths universally have the
/// attribute set (either via `*.png binary` or by the `binary` macro
/// expanding `-text -diff`).
fn path_is_binary(repo: &gix::Repository, path: &str) -> bool {
    let p = std::path::Path::new(path);
    match git::attr_for(repo, p, "binary") {
        Ok(Some(v)) => v.as_slice() == b"set" || v.as_slice() == b"true",
        _ => false,
    }
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
    // Mirror `normalize_for_compare`: byte-safe CRLF rewrite for text
    // paths, raw for binary. We have no captured stamp to compare
    // against here — treat the live read as "current rules" and apply
    // the same shape the sidecar side sees once it's been normalized.
    let path_str = path.to_string_lossy().into_owned();
    if path_is_binary(repo, &path_str) {
        Some(bytes)
    } else {
        Some(crlf_to_lf(&bytes))
    }
}

pub fn build_pending_findings(
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
            crate::staging::StagedOp::Why(body) => out.push(PendingFinding::Why {
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
    // Slice 4: integrity check first. A tampered sidecar surfaces as a
    // distinct `SidecarTampered` drift so renderers can tell external
    // corruption apart from a legitimate live-content divergence.
    let side_bytes = match read_sidecar_verified(repo, mesh_name, add.line_number) {
        Ok(b) => b,
        Err(SidecarVerifyError::Tampered) => return Some(PendingDrift::SidecarTampered),
        Err(SidecarVerifyError::Missing) => return None,
    };
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
    let side_norm = normalize_for_compare(repo, &add.path, &side_bytes, &captured);
    // The live-side byte stream comes either from the worktree (raw
    // bytes, possibly CRLF) or from a git blob (already in canonical
    // form). Apply the same byte-safe normalization the sidecar got.
    let live_norm = if path_is_binary(repo, &add.path) {
        live
    } else {
        crlf_to_lf(&live)
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

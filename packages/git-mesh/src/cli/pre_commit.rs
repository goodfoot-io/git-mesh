//! `git mesh pre-commit-check` — Phase 4 of `docs/stale-layers-plan.md`.
//!
//! Runs the resolver in pre-commit mode (HEAD + Index + Staged-mesh; no
//! worktree), filters findings to those touching the in-flight commit's
//! staged path set, and fails iff:
//!
//! - any filtered finding has `source == Some(Index)` and is not
//!   acknowledged by a staged re-anchor, OR
//! - any pending `Add`/`Remove` whose path intersects the staged paths
//!   has `drift: Some(SidecarMismatch)`.
//!
//! Worktree drift is **not** a pre-commit failure (per plan §"Phase 4").
//! `Message` and `ConfigChange` pending ops never drive exit code (per
//! plan §B3).

use crate::mesh::read::list_mesh_names;
use crate::resolver::{build_pending_findings, resolve_mesh};
use crate::types::{
    DriftSource, EngineOptions, Finding, LayerSet, MeshResolved, PendingDrift, PendingFinding,
    RangeStatus,
};
use crate::Error;
use anyhow::Result;
use std::collections::HashSet;
use std::path::PathBuf;

pub fn run_pre_commit_check(repo: &gix::Repository) -> Result<i32> {
    let options = EngineOptions {
        layers: LayerSet {
            worktree: false,
            index: true,
            staged_mesh: true,
        },
        ignore_unavailable: false,
        since: None,
    };
    // Union of committed mesh names and names with a staging directory:
    // a brand-new mesh (no commit yet) only exists on disk via its
    // staging file but its pending Add/Remove ops still need to be
    // surfaced to the hook. `stale_meshes` walks committed refs only.
    let names = mesh_names_with_staging(repo)?;
    let mut meshes: Vec<MeshResolved> = Vec::with_capacity(names.len());
    for name in &names {
        match resolve_mesh(repo, name, options) {
            Ok(m) => meshes.push(m),
            Err(Error::MeshNotFound(_)) => {
                // Staging-only mesh (no commit yet). Surface its
                // pending Add/Remove via the synthetic shape so the
                // hook can still catch a SidecarMismatch on a brand-new
                // mesh's pending op.
                meshes.push(MeshResolved {
                    name: name.clone(),
                    message: String::new(),
                    ranges: Vec::new(),
                    pending: build_pending_findings(repo, name),
                });
            }
            Err(e) => return Err(e.into()),
        }
    }

    // Plan §"Phase 4" option (b): recompute the staged path set from
    // `git diff --cached --name-only` rather than widening the engine's
    // public return shape. Each entry is the post-rename path (or the
    // path being deleted) — we treat it as the set of paths participating
    // in the in-flight commit.
    let staged_paths = staged_paths(repo)?;

    let findings: Vec<Finding> = meshes
        .iter()
        .flat_map(|m| {
            m.ranges
                .iter()
                .filter(|r| r.status != RangeStatus::Fresh)
                .map(|r| Finding {
                    mesh: m.name.clone(),
                    range_id: r.range_id.clone(),
                    status: r.status.clone(),
                    source: r.source,
                    anchored: r.anchored.clone(),
                    current: r.current.clone(),
                    acknowledged_by: r.acknowledged_by.clone(),
                    culprit: r.culprit.clone(),
                })
        })
        .collect();

    let pending: Vec<PendingFinding> = meshes
        .iter()
        .flat_map(|m| m.pending.iter().cloned())
        .collect();

    // Filter findings to those whose `current` path participates in the
    // in-flight commit. If `current` is missing (terminal status) we keep
    // the finding only when its anchored path is staged — for those, the
    // commit is what made the path terminal.
    let kept_findings: Vec<&Finding> = findings
        .iter()
        .filter(|f| {
            let p: &std::path::Path = f
                .current
                .as_ref()
                .map(|c| c.path.as_path())
                .unwrap_or(f.anchored.path.as_path());
            staged_paths.contains(p)
        })
        .collect();

    // Filter pending findings to Add/Remove whose op.path intersects the
    // staged paths.
    let kept_pending: Vec<&PendingFinding> = pending
        .iter()
        .filter(|p| match p {
            PendingFinding::Add { op, .. } => staged_paths.contains(std::path::Path::new(&op.path)),
            PendingFinding::Remove { op, .. } => {
                staged_paths.contains(std::path::Path::new(&op.path))
            }
            PendingFinding::Why { .. } | PendingFinding::ConfigChange { .. } => false,
        })
        .collect();

    // Print the focused report (reuses the layered `stale` vocabulary).
    render_report(&meshes, &kept_findings, &kept_pending);

    // Exit logic per Phase 4.
    let index_drift_unacked = kept_findings.iter().any(|f| {
        f.source == Some(DriftSource::Index) && f.acknowledged_by.is_none()
    });
    let pending_drift = kept_pending.iter().any(|p| {
        matches!(
            p,
            PendingFinding::Add { drift: Some(_), .. }
                | PendingFinding::Remove { drift: Some(_), .. }
        )
    });

    if index_drift_unacked || pending_drift {
        Ok(1)
    } else {
        Ok(0)
    }
}

/// Union of (committed mesh names, mesh names with a staging ops file).
/// Pre-commit needs both so a brand-new mesh that exists only as
/// `.git/mesh/staging/<name>` is still inspected for pending drift.
fn mesh_names_with_staging(repo: &gix::Repository) -> Result<Vec<String>> {
    let mut names: HashSet<String> = list_mesh_names(repo)
        .map_err(anyhow::Error::from)?
        .into_iter()
        .collect();
    let workdir = repo
        .workdir()
        .ok_or_else(|| anyhow::anyhow!("bare repository"))?;
    let staging = workdir.join(".git").join("mesh").join("staging");
    if staging.is_dir() {
        for entry in std::fs::read_dir(&staging)? {
            let entry = entry?;
            let fname = entry.file_name();
            let Some(s) = fname.to_str() else { continue };
            // Sidecars / sidecar-meta / messages are derived; only
            // bare names (no `.`) are ops files. Per-mesh layout: see
            // `staging.rs` doc comment.
            if s.contains('.') {
                continue;
            }
            names.insert(s.to_string());
        }
    }
    let mut out: Vec<String> = names.into_iter().collect();
    out.sort();
    Ok(out)
}

fn staged_paths(repo: &gix::Repository) -> Result<HashSet<PathBuf>> {
    if repo.workdir().is_none() {
        anyhow::bail!("bare repository");
    }
    // Equivalent to `git diff --cached --name-only -z` against HEAD —
    // diff HEAD^{tree} vs the worktree index. If HEAD is unborn, compare
    // against the empty tree. Rename tracking is disabled so renames
    // decompose into deletion + addition; the union of emitted paths
    // matches what the previous subprocess produced (and ASCII paths
    // skip the `core.quotepath` quoting concern entirely).
    let head_tree = repo
        .head_tree_id_or_empty()
        .map_err(|e| anyhow::anyhow!("resolve HEAD tree: {e}"))?;
    let worktree_index = repo
        .index_or_load_from_head_or_empty()
        .map_err(|e| anyhow::anyhow!("load index: {e}"))?;
    let mut set: HashSet<PathBuf> = HashSet::new();
    repo.tree_index_status::<std::io::Error>(
        &head_tree,
        &worktree_index,
        None,
        gix::status::tree_index::TrackRenames::Disabled,
        |change, _tree_idx, _wt_idx| {
            use gix::diff::index::ChangeRef;
            let mut push = |loc: &gix::bstr::BStr| {
                if let Ok(s) = std::str::from_utf8(loc) {
                    set.insert(PathBuf::from(s));
                }
            };
            match change {
                ChangeRef::Addition { location, .. }
                | ChangeRef::Modification { location, .. }
                | ChangeRef::Deletion { location, .. } => push(&location),
                ChangeRef::Rewrite {
                    source_location,
                    location,
                    ..
                } => {
                    push(&source_location);
                    push(&location);
                }
            }
            Ok(std::ops::ControlFlow::Continue(()))
        },
    )
    .map_err(|e| anyhow::anyhow!("tree-index diff: {e}"))?;
    Ok(set)
}

fn render_report(
    meshes: &[MeshResolved],
    findings: &[&Finding],
    pending: &[&PendingFinding],
) {
    if findings.is_empty() && pending.is_empty() {
        return;
    }
    println!("git mesh pre-commit-check: stale ranges in the in-flight commit");
    for m in meshes {
        let mesh_findings: Vec<&&Finding> =
            findings.iter().filter(|f| f.mesh == m.name).collect();
        let mesh_pending: Vec<&&PendingFinding> = pending
            .iter()
            .filter(|p| pending_mesh(p) == m.name.as_str())
            .collect();
        if mesh_findings.is_empty() && mesh_pending.is_empty() {
            continue;
        }
        println!("  mesh {}", m.name);
        for f in &mesh_findings {
            let src = match f.source {
                Some(DriftSource::Head) => "H",
                Some(DriftSource::Index) => "I",
                Some(DriftSource::Worktree) => "W",
                None => "-",
            };
            let ack = if f.acknowledged_by.is_some() {
                " (ack)"
            } else {
                ""
            };
            println!(
                "    {src} {:?} {} {}{}",
                f.status,
                f.anchored.path.display(),
                f.range_id,
                ack
            );
        }
        for p in &mesh_pending {
            match p {
                PendingFinding::Add { op, drift, .. } => {
                    let note = match drift {
                        Some(PendingDrift::SidecarMismatch) => " (drift: sidecar mismatch)",
                        Some(PendingDrift::SidecarTampered) => " (drift: sidecar tampered)",
                        None => "",
                    };
                    println!("    + ADD    {}{}", op.path, note);
                }
                PendingFinding::Remove { op, drift, .. } => {
                    let note = match drift {
                        Some(PendingDrift::SidecarMismatch) => " (drift: sidecar mismatch)",
                        Some(PendingDrift::SidecarTampered) => " (drift: sidecar tampered)",
                        None => "",
                    };
                    println!("    - REMOVE {}{}", op.path, note);
                }
                _ => {}
            }
        }
    }
}

fn pending_mesh(p: &PendingFinding) -> &str {
    match p {
        PendingFinding::Add { mesh, .. }
        | PendingFinding::Remove { mesh, .. }
        | PendingFinding::Why { mesh, .. }
        | PendingFinding::ConfigChange { mesh, .. } => mesh,
    }
}

//! `git mesh pre-commit` — fail-closed gate for the staged tree.
//!
//! Runs the resolver in pre-commit mode (HEAD + Index + Staged-mesh; no
//! worktree) and fails iff anything is rendered:
//!
//! - any non-Fresh, unacknowledged range finding, OR
//! - any pending `Add`/`Remove` carrying `Some(PendingDrift)`.
//!
//! `--no-exit-code` keeps the report but always exits 0 (informational
//! mode). `Message` and `ConfigChange` pending ops never drive exit code.

use crate::Error;
use crate::cli::PreCommitArgs;
use crate::mesh::read::list_mesh_names;
use crate::resolver::{build_pending_findings, resolve_mesh};
use crate::types::{
    DriftSource, EngineOptions, Finding, LayerSet, MeshResolved, PendingDrift, PendingFinding,
    RangeExtent, RangeStatus,
};
use anyhow::Result;
use std::collections::HashSet;

const RESOLUTION_HINT: &str = "hint: re-anchor with `git mesh rm <name> <range>` and `git mesh add <name> <new-range>`,\n      or `git mesh mv <name> <new-name>` if the path moved, or revert the change.";

pub fn run_pre_commit(repo: &gix::Repository, args: PreCommitArgs) -> Result<i32> {
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
    // surfaced to the hook. `list_mesh_names` walks committed refs only.
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

    // Per-layer expansion: same as `stale_output.rs` adapter.
    let findings: Vec<Finding> = meshes
        .iter()
        .flat_map(|m| {
            m.ranges
                .iter()
                .filter(|r| r.status != RangeStatus::Fresh)
                .flat_map(|r| {
                    let ack = r.acknowledged_by.clone();
                    if r.layer_sources.is_empty() {
                        vec![Finding {
                            mesh: m.name.clone(),
                            range_id: r.range_id.clone(),
                            status: r.status.clone(),
                            source: r.source,
                            anchored: r.anchored.clone(),
                            current: r.current.clone(),
                            acknowledged_by: ack,
                            culprit: r.culprit.clone(),
                        }]
                    } else {
                        r.layer_sources
                            .iter()
                            .map(|&src| Finding {
                                mesh: m.name.clone(),
                                range_id: r.range_id.clone(),
                                status: r.status.clone(),
                                source: Some(src),
                                anchored: r.anchored.clone(),
                                current: r.current.clone(),
                                acknowledged_by: ack.clone(),
                                culprit: if src == DriftSource::Head {
                                    r.culprit.clone()
                                } else {
                                    None
                                },
                            })
                            .collect()
                    }
                })
        })
        .collect();

    let pending: Vec<PendingFinding> = meshes
        .iter()
        .flat_map(|m| m.pending.iter().cloned())
        .collect();

    // Whole-staged-tree gate per `<fail-closed>`: render every unacked
    // finding and every drift-bearing pending Add/Remove, regardless of
    // whether the in-flight diff touches the path. Acked findings are
    // suppressed — by acknowledging the drift, the in-flight commit is
    // resolving it.
    let rendered_findings: Vec<&Finding> = findings
        .iter()
        .filter(|f| f.acknowledged_by.is_none())
        .collect();
    let rendered_pending: Vec<&PendingFinding> = pending
        .iter()
        .filter(|p| {
            matches!(
                p,
                PendingFinding::Add { drift: Some(_), .. }
                    | PendingFinding::Remove { drift: Some(_), .. }
            )
        })
        .collect();

    let any_rendered = !rendered_findings.is_empty() || !rendered_pending.is_empty();
    render_report(&meshes, &rendered_findings, &rendered_pending);

    if any_rendered && !args.no_exit_code {
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
    let staging = crate::git::mesh_dir(repo).join("staging");
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

fn render_report(meshes: &[MeshResolved], findings: &[&Finding], pending: &[&PendingFinding]) {
    if findings.is_empty() && pending.is_empty() {
        return;
    }
    println!("git mesh pre-commit: drift in staged tree");
    for m in meshes {
        let mesh_findings: Vec<&&Finding> = findings.iter().filter(|f| f.mesh == m.name).collect();
        let mesh_pending: Vec<&&PendingFinding> = pending
            .iter()
            .filter(|p| pending_mesh(p) == m.name.as_str())
            .collect();
        if mesh_findings.is_empty() && mesh_pending.is_empty() {
            continue;
        }
        println!("  mesh {}", m.name);
        for f in &mesh_findings {
            let origin = match f.source {
                Some(DriftSource::Index) => "in-flight",
                _ => "pre-existing",
            };
            let path = f
                .current
                .as_ref()
                .map(|c| c.path.as_path())
                .unwrap_or(f.anchored.path.as_path());
            println!(
                "    {:<8} {}  {}",
                format!("{:?}", f.status),
                path.display(),
                origin
            );
        }
        for p in &mesh_pending {
            match p {
                PendingFinding::Add { op, drift, .. } => {
                    let note = drift_note(drift);
                    println!(
                        "    + ADD    {}  in-flight{}",
                        render_pending_address(&op.path, op.extent),
                        note
                    );
                }
                PendingFinding::Remove { op, drift, .. } => {
                    let note = drift_note(drift);
                    println!(
                        "    - REMOVE {}  in-flight{}",
                        render_pending_address(&op.path, op.extent),
                        note
                    );
                }
                _ => {}
            }
        }
    }
    println!("{RESOLUTION_HINT}");
}

fn drift_note(drift: &Option<PendingDrift>) -> &'static str {
    match drift {
        Some(PendingDrift::SidecarMismatch) => " (drift: sidecar mismatch)",
        Some(PendingDrift::SidecarTampered) => " (drift: sidecar tampered)",
        None => "",
    }
}

fn render_pending_address(path: &str, extent: RangeExtent) -> String {
    match extent {
        RangeExtent::Whole => format!("{path}  (whole)"),
        RangeExtent::Lines { start, end } => format!("{path}#L{start}-L{end}"),
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

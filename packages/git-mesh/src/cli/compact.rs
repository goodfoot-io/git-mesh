//! `git mesh stale --compact` driver and output rendering.
//!
//! This module is the only caller of `mesh::compact_mesh`. Everything in
//! `run_stale` below the `--compact` branch is unchanged and never writes.

use crate::cli::StaleArgs;
use crate::mesh::compact::{AnchorCompactOutcome, MeshCompactOutcome};
use crate::types::{EngineOptions, LayerSet};
use anyhow::Result;

pub fn run_compact(repo: &gix::Repository, args: &StaleArgs) -> Result<i32> {
    // HEAD-only resolution: no worktree, no index, no staged-mesh layer.
    let options = EngineOptions {
        layers: LayerSet {
            worktree: false,
            index: false,
            staged_mesh: false,
        },
        ignore_unavailable: args.ignore_unavailable,
        since: None, // --since not supported with --compact
        needs_all_layers: false,
    };

    // Enumerate meshes to compact.
    let mesh_names: Vec<String> = match &args.name {
        Some(n) => {
            // Verify the mesh exists.
            let wd = crate::git::work_dir(repo)?;
            let mesh_ref = format!("refs/meshes/v1/{n}");
            crate::git::resolve_ref_oid_optional(wd, &mesh_ref)?
                .ok_or_else(|| crate::Error::MeshNotFound(n.clone()))?;
            vec![n.clone()]
        }
        None => crate::mesh::read::list_mesh_names(repo)?,
    };

    let mut outcomes: Vec<MeshCompactOutcome> = Vec::new();
    for name in &mesh_names {
        // Per-mesh isolation: accumulate, never early-return via `?`.
        let outcome = crate::mesh::compact::compact_mesh(repo, name, options);
        outcomes.push(outcome.unwrap_or_else(|e| MeshCompactOutcome::error(name, e)));
    }

    // Render output.
    match args.format {
        crate::cli::StaleFormat::Json => render_json(&outcomes)?,
        _ => render_human(&outcomes)?,
    }

    // Exit code.
    // Hard errors always exit nonzero — `--no-exit-code` does NOT suppress them.
    // CAS conflicts are suppressed by `--no-exit-code`.
    let hard_error = outcomes.iter().any(|o| o.is_hard_error());
    let cas_conflict = outcomes.iter().any(|o| o.conflicts > 0);
    if hard_error {
        Ok(2) // always nonzero; --no-exit-code has no effect
    } else if cas_conflict && !args.no_exit_code {
        Ok(1) // CAS conflict suppressed by --no-exit-code
    } else {
        Ok(0)
    }
}

fn render_human(outcomes: &[MeshCompactOutcome]) -> Result<()> {
    for o in outcomes {
        if let Some(err) = &o.hard_error {
            eprintln!("[{}] error: {}", o.name, err);
            continue;
        }
        if o.skipped_staged > 0 {
            println!("[{}] skipped (staging ops present)", o.name);
            continue;
        }
        if o.conflicts > 0 {
            println!("[{}] CAS conflict exhausted retries", o.name);
            continue;
        }
        if o.advanced == 0 {
            println!("[{}] nothing to compact", o.name);
        } else {
            // Show the HEAD sha from any advanced record.
            let head_sha = o
                .anchors
                .iter()
                .find(|a| a.outcome == AnchorCompactOutcome::Advanced)
                .and_then(|a| a.new_commit.as_deref())
                .map(|sha| &sha[..12.min(sha.len())])
                .unwrap_or("unknown");
            println!(
                "[{}] advanced {} anchor(s) to {}",
                o.name, o.advanced, head_sha
            );
        }
    }
    Ok(())
}

fn render_json(outcomes: &[MeshCompactOutcome]) -> Result<()> {
    for o in outcomes {
        let anchors: Vec<serde_json::Value> = o
            .anchors
            .iter()
            .map(|a| {
                serde_json::json!({
                    "anchor_id": a.anchor_id,
                    "outcome": match &a.outcome {
                        AnchorCompactOutcome::Advanced => "advanced",
                        AnchorCompactOutcome::SkippedStale => "skipped_stale",
                        AnchorCompactOutcome::SkippedMoved => "skipped_moved",
                        AnchorCompactOutcome::SkippedStagedOps => "skipped_staged_ops",
                        AnchorCompactOutcome::SkippedAlreadyHead => "skipped_already_head",
                    },
                    "old_commit": a.old_commit,
                    "new_commit": a.new_commit,
                    "old_path": a.old_path,
                    "new_path": a.new_path,
                    "old_blob": a.old_blob,
                    "new_blob": a.new_blob,
                })
            })
            .collect();

        let obj = serde_json::json!({
            "schema": "compact-v1",
            "mesh": o.name,
            "advanced": o.advanced,
            "skipped_stale": o.skipped_stale,
            "skipped_moved": o.skipped_moved,
            "skipped_staged": o.skipped_staged,
            "conflicts": o.conflicts,
            "errors": o.errors,
            "hard_error": o.hard_error,
            "anchors": anchors,
        });
        println!("{}", serde_json::to_string(&obj)?);
    }
    Ok(())
}

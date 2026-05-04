//! `git mesh stale --compact` driver and output rendering.
//!
//! This module is the only caller of `mesh::compact_mesh`. Everything in
//! `run_stale` below the `--compact` branch is unchanged and never writes.

use crate::cli::{StaleArgs, StaleFormat};
use crate::mesh::compact::{AnchorCompactOutcome, MeshCompactOutcome};
use crate::types::{EngineOptions, LayerSet};
use anyhow::Result;
use std::io::Write as _;

pub fn run_compact(repo: &gix::Repository, args: &StaleArgs) -> Result<i32> {
    // F8: Reject incompatible --format values BEFORE any mutation.
    // Only 'human' and 'json' are supported in compact mode.
    match args.format {
        StaleFormat::Human | StaleFormat::Json => {}
        other => {
            let name = match other {
                StaleFormat::Porcelain => "porcelain",
                StaleFormat::Junit => "junit",
                StaleFormat::GithubActions => "github-actions",
                StaleFormat::Human | StaleFormat::Json => unreachable!(),
            };
            eprintln!(
                "error: the argument '--compact' cannot be used with '--format {name}' \
                 (only 'human' and 'json' are supported in compact mode)"
            );
            return Ok(2);
        }
    }

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

    // Enumerate meshes to compact. Compact only supports a single mesh
    // name or the all-mesh sweep; path/glob positional args are rejected.
    let mesh_names: Vec<String> = match args.paths.as_slice() {
        [] => crate::mesh::read::list_mesh_names(repo)?,
        [n] => {
            let wd = crate::git::work_dir(repo)?;
            let mesh_ref = format!("refs/meshes/v1/{n}");
            crate::git::resolve_ref_oid_optional(wd, &mesh_ref)?
                .ok_or_else(|| crate::Error::MeshNotFound((*n).clone()))?;
            vec![(*n).clone()]
        }
        _ => {
            anyhow::bail!(
                "git mesh stale --compact: expected at most one mesh name, \
                 got {} positional args (--compact only supports a single \
                 mesh name or no args for all-mesh)",
                args.paths.len()
            );
        }
    };

    // Per-mesh stream callback: NDJSON when --format=json. The batch
    // path expects the crate `Result` type, so we surface I/O errors
    // through `Error::Git` rather than letting `anyhow` leak in.
    //
    // For `--format=human` we defer all rendering until after compaction
    // so the regular `stale` view (post-compaction) renders first and the
    // compaction summary trails it.
    let stream_outcome = |outcome: &MeshCompactOutcome| -> crate::Result<()> {
        if args.format == StaleFormat::Json {
            render_json_one(outcome).map_err(|e| crate::Error::Git(e.to_string()))?;
            let mut stdout = std::io::stdout();
            stdout
                .flush()
                .map_err(|e| crate::Error::Git(e.to_string()))?;
        }
        Ok(())
    };

    // Item 5: when invoked without an explicit name, share resolver
    // state across the all-mesh sweep. Named-mesh path stays simple.
    let outcomes: Vec<MeshCompactOutcome> = if args.paths.len() == 1 {
        let mut out = Vec::with_capacity(mesh_names.len());
        for name in &mesh_names {
            let outcome = crate::mesh::compact::compact_mesh(repo, name, options)
                .unwrap_or_else(|e| MeshCompactOutcome::error(name, e));
            stream_outcome(&outcome)?;
            out.push(outcome);
        }
        out
    } else {
        crate::mesh::compact::compact_meshes_batch(repo, &mesh_names, options, stream_outcome)?
    };

    // Human format: render the regular `stale` view (post-compaction
    // state) and then either a short summary line or the detailed
    // per-mesh outcomes when `--verbose` is set.
    if args.format == StaleFormat::Human {
        let mut stale_args = args.clone();
        stale_args.compact = false;
        stale_args.verbose = false;
        stale_args.no_exit_code = true;
        let _ = super::stale_output::run_stale(repo, stale_args)?;

        if args.verbose {
            render_human(&outcomes)?;
        } else {
            render_human_summary(&outcomes);
        }
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

/// One-line summary trailing the regular stale output when `--compact`
/// is invoked without `--verbose`. Mentions the words other tooling and
/// tests look for: `advanced`, `nothing to compact`, `staging ops
/// present`, `CAS conflict`. Hard errors are still reported on stderr.
fn render_human_summary(outcomes: &[MeshCompactOutcome]) {
    for o in outcomes {
        if let Some(err) = &o.hard_error {
            eprintln!("[{}] error: {}", o.name, err);
        }
    }

    let advanced: u32 = outcomes.iter().map(|o| o.advanced).sum();
    let advanced_meshes = outcomes.iter().filter(|o| o.advanced > 0).count();
    let staged_skipped = outcomes.iter().filter(|o| o.skipped_staged > 0).count();
    let conflicts = outcomes.iter().filter(|o| o.conflicts > 0).count();

    let mut parts: Vec<String> = Vec::new();
    if advanced > 0 {
        parts.push(format!(
            "advanced {advanced} anchor(s) across {advanced_meshes} mesh(es)"
        ));
    }
    if staged_skipped > 0 {
        parts.push(format!(
            "{staged_skipped} mesh(es) skipped (staging ops present)"
        ));
    }
    if conflicts > 0 {
        parts.push(format!("{conflicts} mesh(es) had CAS conflict"));
    }
    if parts.is_empty() {
        println!("nothing to compact.");
    } else {
        println!("{}.", parts.join("; "));
    }
}

fn render_json_one(o: &MeshCompactOutcome) -> Result<()> {
    let anchors: Vec<serde_json::Value> = o
        .anchors
        .iter()
        .map(|a| {
            serde_json::json!({
                "anchor_id": a.anchor_id,
                "outcome": match &a.outcome {
                    AnchorCompactOutcome::Advanced => "advanced",
                    AnchorCompactOutcome::ConflictExhausted => "conflict_exhausted",
                    AnchorCompactOutcome::SkippedChanged => "skipped_changed",
                    AnchorCompactOutcome::SkippedOrphaned => "skipped_orphaned",
                    AnchorCompactOutcome::SkippedMergeConflict => "skipped_merge_conflict",
                    AnchorCompactOutcome::SkippedSubmodule => "skipped_submodule",
                    AnchorCompactOutcome::SkippedUnavailable => "skipped_unavailable",
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

    // F9: staged_ops_present reason token.
    let reason: Option<&str> = if o.staged_ops_present {
        Some("staged_ops_present")
    } else {
        None
    };

    let obj = serde_json::json!({
        "schema": "compact-v1",
        "mesh": o.name,
        "advanced": o.advanced,
        "skipped_clean_not_head": o.skipped_clean_not_head,
        "skipped_stale": o.skipped_stale,
        "skipped_moved": o.skipped_moved,
        "skipped_staged": o.skipped_staged,
        "conflicts": o.conflicts,
        "errors": o.errors,
        "hard_error": o.hard_error,
        "reason": reason,
        "anchors": anchors,
    });
    println!("{}", serde_json::to_string(&obj)?);
    Ok(())
}

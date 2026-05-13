//! `git mesh stale` output rendering — §10.4.
//!
//! Slice 8 of the layered-stale rewrite (see
//! `docs/stale-layers-plan.md` §"Renderers"). Renderers consume
//! `Finding` / `PendingFinding` end-to-end via a thin adapter that maps
//! the engine's `AnchorResolved` + `MeshResolved.pending` into the
//! plan's "Key types" shape.

#![allow(dead_code)]

use crate::cli::format;
use crate::cli::{CliError, NextStep, StaleArgs, StaleFormat};
use crate::git;
use crate::mesh::follow::{follow_moves, FollowDecision};
use crate::resolver::{
    build_pending_findings, resolve_named_meshes, sort_meshes_by_anchor_path, stale_meshes,
    stale_meshes_with_trace,
};
use crate::staging::{StagedAdd, StagedConfig, StagedRemove};
use crate::types::{
    AnchorExtent, AnchorLocation, AnchorStatus, DriftLocus, DriftSource, EngineOptions, Finding,
    LayerSet, MeshResolved, PendingDrift, PendingFinding, StagedOpRef, UnavailableReason,
};
use crate::validation::validate_mesh_name_shape;
use anyhow::Result;
use serde_json::{json, Value};
use std::collections::HashSet;

fn csv_escape(s: &str) -> String {
    if s.contains(',') || s.contains('"') || s.contains('\n') || s.contains('\r') {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

fn write_perf_trace_csv(
    file: std::fs::File,
    path: &std::path::Path,
    rows: &[crate::perf::TraceRow],
) -> Result<()> {
    use std::io::Write as _;
    let mut w = std::io::BufWriter::new(file);
    let write_err = |e: std::io::Error| -> anyhow::Error {
        CliError {
            subcommand: "stale",
            summary: format!("failed to write perf trace to '{}'", path.display()),
            what_happened: e.to_string(),
            next_steps: vec![NextStep::Prose("Check that the path is writable.".into())],
        }
        .into()
    };
    w.write_all(b"mesh,anchor_id,anchor_sha,path,wall_us,fast_path,status\n")
        .map_err(&write_err)?;
    for r in rows {
        let mut line = String::new();
        line.push_str(&csv_escape(&r.mesh));
        line.push(',');
        line.push_str(&csv_escape(&r.anchor_id));
        line.push(',');
        line.push_str(&csv_escape(&r.anchor_sha));
        line.push(',');
        line.push_str(&csv_escape(&r.path));
        line.push(',');
        line.push_str(&r.wall_us.to_string());
        line.push(',');
        line.push_str(if r.fast_path { "true" } else { "false" });
        line.push(',');
        line.push_str(r.status);
        line.push('\n');
        w.write_all(line.as_bytes()).map_err(&write_err)?;
    }
    w.flush().map_err(write_err)
}

fn open_perf_trace_file(path: &std::path::Path) -> Result<std::fs::File> {
    std::fs::File::create(path).map_err(|e| {
        CliError {
            subcommand: "stale",
            summary: format!("failed to write perf trace to '{}'", path.display()),
            what_happened: e.to_string(),
            next_steps: vec![NextStep::Prose("Check that the path is writable.".into())],
        }
        .into()
    })
}

pub fn run_stale(repo: &gix::Repository, args: StaleArgs) -> Result<i32> {
    if args.compact {
        return super::compact::run_compact(repo, &args);
    }
    let layers = LayerSet {
        worktree: !args.no_worktree,
        index: !args.no_index,
        staged_mesh: !args.no_staged_mesh,
    };
    let show_src_column = layers.worktree || layers.index;
    // Slice 5: resolve `--since <commit-ish>` once, fail-closed on
    // unresolvable input (no silent fallback per `<fail-closed>`).
    let since = match args.since.as_deref() {
        Some(s) => {
            let _perf = crate::perf::span("stale.resolve-since");
            Some(
                crate::git::resolve_commit(repo, s)
                    .map(|hex| {
                        use std::str::FromStr;
                        gix::ObjectId::from_str(&hex).expect("resolve_commit returns valid hex")
                    })
                    .map_err(|e| CliError {
                        subcommand: "stale",
                        summary: format!("`--since {s}` could not be resolved."),
                        what_happened: format!("{e}"),
                        next_steps: vec![
                            NextStep::Bash("git rev-parse HEAD".into()),
                            NextStep::Bash("git log --oneline".into()),
                        ],
                    })?,
            )
        }
        None => None,
    };
    // Phase 4: only the renderers that present per-layer detail need
    // every layer evaluated. The `human` renderer always shows per-layer
    // expansion; `--patch` / `--stat` need every drifting layer's
    // content. Otherwise (oneline, porcelain, json, junit, github), HEAD
    // alone is enough to drive the exit code, so the engine may
    // short-circuit Index/Worktree once HEAD says "stale".
    let needs_all_layers = matches!(args.format, StaleFormat::Human) || args.patch || args.stat;
    let options = EngineOptions {
        layers,
        ignore_unavailable: args.ignore_unavailable,
        since,
        needs_all_layers,
    };

    // Count total committed meshes and anchors before stale_meshes filtering,
    // so the summary can report "0 stale across N meshes (A anchors checked)"
    // even when all are clean.
    let total_committed_mesh_count: usize = crate::mesh::read::list_mesh_refs(repo)
        .map(|refs| refs.len())
        .unwrap_or(0);
    let total_committed_anchor_count: usize = crate::mesh::read::list_mesh_refs(repo)
        .map(|refs| {
            refs.iter()
                .filter_map(|(name, oid)| {
                    crate::mesh::read::read_mesh_from_commit(repo, name, oid).ok()
                })
                .map(|m| m.anchors.len())
                .sum()
        })
        .unwrap_or(0);

    // --perf-trace conflicts with positional paths (requires a full scan).
    if args.perf_trace.is_some() && !args.paths.is_empty() {
        return Err(CliError {
            subcommand: "stale",
            summary: "--perf-trace requires a full scan; remove positional paths".into(),
            what_happened: "--perf-trace captures per-anchor timing across all meshes and cannot \
                            be combined with a subset selection via positional paths."
                .into(),
            next_steps: vec![NextStep::Bash("git mesh stale --perf-trace <path>".into())],
        }
        .into());
    }

    let mut meshes = if args.paths.is_empty() {
        // No positional args: scan every mesh. Pending-only meshes are NOT
        // included — workspace scans answer "what's stale?" only.
        let _perf = crate::perf::span("stale.resolve-all-meshes");
        if let Some(trace_path) = &args.perf_trace {
            // Open the file before running the resolver so a bad path fails
            // in milliseconds rather than after a full scan.
            let trace_file = open_perf_trace_file(trace_path)?;
            let (resolved, trace_rows) = stale_meshes_with_trace(repo, options)?;
            write_perf_trace_csv(trace_file, trace_path, &trace_rows)?;
            resolved
        } else {
            stale_meshes(repo, options)?
        }
    } else {
        // Resolve each positional arg through mesh-name → path-index dispatch.
        let _perf = crate::perf::span("stale.resolve-args");
        let mut mesh_names: Vec<String> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        let mut missing_files: Vec<String> = Vec::new();

        for arg in &args.paths {
            let mut found = false;

            // Step 1: try mesh name first when arg matches mesh-name shape.
            if validate_mesh_name_shape(arg).is_ok() {
                let mesh_ref = format!("refs/meshes/v1/{arg}");
                match crate::git::resolve_ref_oid_optional_repo(repo, &mesh_ref)? {
                    Some(_oid) => {
                        // Committed mesh ref exists.
                        if seen.insert(arg.clone()) {
                            mesh_names.push(arg.clone());
                        }
                        found = true;
                    }
                    None => {
                        // Not a committed mesh; check for staging-only mesh before
                        // falling through to path-index lookup. Matches the original
                        // single-mesh path's MeshNotFound → staging check.
                        if layers.staged_mesh && !build_pending_findings(repo, arg).is_empty() {
                            if seen.insert(arg.clone()) {
                                mesh_names.push(arg.clone());
                            }
                            found = true;
                        }
                    }
                }
            }

            // Step 2: fall back to path index (glob-aware).
            if !found {
                let names = if crate::mesh::path_index::is_glob_pattern(arg) {
                    crate::mesh::path_index::matching_mesh_names_glob(repo, arg, None)
                        .unwrap_or_default()
                } else {
                    crate::mesh::path_index::matching_mesh_names(repo, arg, None)
                        .unwrap_or_default()
                };
                for name in names {
                    if seen.insert(name.clone()) {
                        mesh_names.push(name);
                    }
                    found = true;
                }
            }

            // Step 3: arg matched neither a mesh nor any path-index entry. If
            // it names an existing file in the worktree, silently skip — no
            // mesh tracks it, so there is nothing for stale to scan and that
            // is not an error. If the file doesn't exist, surface it.
            if !found {
                let exists = repo
                    .workdir()
                    .map(|w| w.join(arg).exists())
                    .unwrap_or(false);
                if !exists {
                    missing_files.push(arg.clone());
                }
            }
        }

        // Step 4: any arg that named a non-existent file → CliError.
        if !missing_files.is_empty() {
            let all = missing_files.join("`, `");
            let summary = if missing_files.len() == 1 {
                format!("`{all}` is not tracked.")
            } else {
                format!("`{all}` are not tracked.")
            };
            return Err(CliError {
                subcommand: "stale",
                summary,
                what_happened: format!(
                    "The following files were requested but do not exist in the working tree: `{all}`. \
                     Git-mesh resolves positional arguments as mesh names, file paths, or globs."
                ),
                next_steps: vec![
                    NextStep::Prose("Check the file paths for typos.".into()),
                    NextStep::Bash("git mesh list".into()),
                ],
            }
            .into());
        }

        // Step 5: resolve candidate mesh names through one shared EngineState.
        let resolved = {
            let _perf = crate::perf::span("stale.resolve-named-meshes");
            resolve_named_meshes(repo, &mesh_names, options)?
        };
        let mut meshes: Vec<MeshResolved> = Vec::with_capacity(resolved.len());
        for (_name, result) in resolved {
            match result {
                Ok(mesh) => meshes.push(mesh),
                Err(crate::Error::MeshNotFound(_)) => {
                    // Staging-only mesh; step 6 will check for pending entries.
                }
                Err(e) => return Err(e.into()),
            }
        }

        // Step 6: surface staging-only meshes for names that had no committed ref.
        if layers.staged_mesh {
            let _perf = crate::perf::span("stale.resolve-staging-only");
            for name in &mesh_names {
                if meshes.iter().any(|m| m.name == *name) {
                    continue;
                }
                let pending = build_pending_findings(repo, name);
                if !pending.is_empty() {
                    meshes.push(MeshResolved {
                        name: name.clone(),
                        message: String::new(),
                        anchors: Vec::new(),
                        pending,
                    });
                }
            }
        }

        meshes
    };

    // Sort all collected meshes (including staging-only meshes with empty
    // anchors) by anchor path for deterministic output regardless of whether
    // they came from a full scan, positional args, or staging-only discovery.
    sort_meshes_by_anchor_path(&mut meshes);

    // Adapter: engine output (`MeshResolved`) → renderer input
    // (`Finding` / `PendingFinding`). The adapter is a pure data shape
    // transform; semantics live in the engine.
    //
    // Per-layer expansion: each non-Fresh anchor emits one `Finding` per
    // drifting layer in `layer_sources` (shallow-to-deep: I → W → H).
    // Terminal statuses (Orphaned, MergeConflict, Submodule,
    // ContentUnavailable) have an empty `layer_sources` and emit exactly
    // one row with `source=None`. MOVED also emits one row.
    let (findings, pending): (Vec<Finding>, Vec<PendingFinding>) = {
        let _perf = crate::perf::span("stale.build-findings");
        let findings: Vec<Finding> = meshes
            .iter()
            .flat_map(|m| {
                m.anchors
                    .iter()
                    .filter(|r| r.status != AnchorStatus::Fresh)
                    .flat_map(|r| {
                        let ack = r.acknowledged_by.clone();
                        if r.layer_sources.is_empty() {
                            // Terminal status or MOVED with no tracked layer:
                            // emit one row with the stored source.
                            vec![Finding {
                                mesh: m.name.clone(),
                                anchor_id: r.anchor_id.clone(),
                                status: r.status.clone(),
                                source: r.source,
                                anchored: r.anchored.clone(),
                                current: r.current.clone(),
                                acknowledged_by: ack,
                                locus: r.locus,
                            }]
                        } else {
                            // Emit one Finding per drifting layer.
                            r.layer_sources
                                .iter()
                                .map(|&src| Finding {
                                    mesh: m.name.clone(),
                                    anchor_id: r.anchor_id.clone(),
                                    status: r.status.clone(),
                                    source: Some(src),
                                    anchored: r.anchored.clone(),
                                    current: r.current.clone(),
                                    acknowledged_by: ack.clone(),
                                    locus: if src == DriftSource::Head {
                                        r.locus
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
        (findings, pending)
    };

    // Auto-follow: for each mesh, compute FollowDecisions and rewrite the
    // mesh if opt-in is active (--auto-follow flag or follow-moves=true in
    // mesh config) and all guardrails hold.
    //
    // Exit-code accounting: we subtract followed anchor_ids from the stale
    // count here (same run). Per spec the "next run reports Fresh" guarantee
    // is what matters; within this run we subtract rather than re-resolve to
    // keep the implementation simple and avoid a second round-trip.
    let followed_ids: HashSet<String> = run_auto_follow_pass(repo, &args, &meshes);

    // Plan §B3: an acknowledged finding does not drive exit code; nor
    // does a `ContentUnavailable` finding under `--ignore-unavailable`.
    // Followed Moved findings are also subtracted: we just rewrote them so
    // they are logically Fresh for this invocation's exit code.
    let unacked_findings: usize = findings
        .iter()
        .filter(|f| {
            if f.acknowledged_by.is_some() {
                return false;
            }
            if args.ignore_unavailable && matches!(f.status, AnchorStatus::ContentUnavailable(_)) {
                return false;
            }
            if followed_ids.contains(&f.anchor_id) {
                return false;
            }
            true
        })
        .count();
    // Pending ops with sidecar drift (mismatch or tampered) drive exit code:
    // a tampered or mismatched sidecar is a real integrity problem callers
    // must surface (`<fail-closed>`). Clean pending ops do not.
    let drifting_pending: usize = pending
        .iter()
        .filter(|p| matches!(
            p,
            PendingFinding::Add { drift: Some(_), .. }
                | PendingFinding::Remove { drift: Some(_), .. }
        ))
        .count();
    let stale_count = unacked_findings + drifting_pending;

    match args.format {
        StaleFormat::Human => {
            let _perf = crate::perf::span("stale.render-human");
            let printed = render_human(
                repo,
                &meshes,
                &findings,
                &pending,
                &followed_ids,
                HumanRenderOptions {
                    oneline: args.oneline,
                    stat: args.stat,
                    patch: args.patch,
                    show_src: show_src_column,
                    named_lookup: !args.paths.is_empty(),
                },
            )?;

            // No-drift messages: when nothing was printed and there is no
            // drift to report, output a summary or clean confirmation.
            // Silence is the wrong answer when the user names a mesh, and
            // even scan-all should report a clean result with counts.
            // Scan-all, all clean: print summary line with counts.
            // Named-lookup clean meshes: render_human already emitted the block;
            // no fallback message needed here.
            if !printed && stale_count == 0 && args.paths.is_empty() {
                // Use total_committed_mesh_count / total_committed_anchor_count
                // (all committed meshes / anchors) rather than meshes.len()
                // or meshes[.].anchors.len() (which only includes meshes with
                // findings after stale_meshes filtering).
                let total_anchors = total_committed_anchor_count;
                let mesh_word = if total_committed_mesh_count == 1 { "mesh" } else { "meshes" };
                let anchor_word = if total_anchors == 1 { "anchor" } else { "anchors" };
                println!(
                    "0 stale across {} {} ({} {} checked)",
                    total_committed_mesh_count,
                    mesh_word,
                    total_anchors,
                    anchor_word,
                );
            }
        }
        StaleFormat::Porcelain => {
            let _perf = crate::perf::span("stale.render-porcelain");
            render_porcelain(&findings, show_src_column);
        }
        StaleFormat::Json => {
            let _perf = crate::perf::span("stale.render-json");
            render_json(&meshes, &findings, &pending, &followed_ids)?;
        }
        StaleFormat::Junit => {
            let _perf = crate::perf::span("stale.render-junit");
            render_junit(&findings);
        }
        StaleFormat::GithubActions => {
            let _perf = crate::perf::span("stale.render-github");
            render_github(&findings);
        }
    }

    let exit = if stale_count == 0 || args.no_exit_code {
        0
    } else {
        1
    };
    Ok(exit)
}

fn staging_only_mesh_names(repo: &gix::Repository) -> Result<Vec<String>> {
    let dir = crate::git::mesh_dir(repo).join("staging");
    let mut out = Vec::new();
    if !dir.is_dir() {
        return Ok(out);
    }
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.contains('.') {
            out.push(crate::staging::decode_name_from_fs(&name));
        }
    }
    out.sort();
    Ok(out)
}

// ---------------------------------------------------------------------------
// Shared formatting helpers.
// ---------------------------------------------------------------------------

fn extent_lines(extent: AnchorExtent) -> (u32, u32) {
    match extent {
        AnchorExtent::LineRange { start, end } => (start, end),
        AnchorExtent::WholeFile => (0, 0),
    }
}

fn extent_to_options(extent: AnchorExtent) -> (Option<u32>, Option<u32>) {
    match extent {
        AnchorExtent::LineRange { start, end } => (Some(start), Some(end)),
        AnchorExtent::WholeFile => (None, None),
    }
}

/// Prose description of an anchor finding's status, source, and destination.
///
/// Delegates to `format_drift_label` for `Changed`/`Orphaned`; renders the
/// remaining statuses (Moved, MergeConflict, Submodule, ContentUnavailable)
/// inline in the uppercase voice the human renderer's header column uses.
fn describe_finding(f: &Finding) -> String {
    let src_phrase = |src: Option<DriftSource>| -> &'static str {
        match src {
            Some(DriftSource::Head) => "in HEAD",
            Some(DriftSource::Index) => "in the index",
            Some(DriftSource::Worktree) => "in the working tree",
            None => "",
        }
    };

    match &f.status {
        AnchorStatus::Changed => {
            let label = super::drift_label::format_drift_label(
                &f.status,
                f.source,
                f.locus.as_ref(),
                f.current.is_some(),
            );
            uppercase_first(&label)
        }
        AnchorStatus::Moved => {
            let src = src_phrase(f.source);
            if let Some(cur) = &f.current {
                let (s, e) = extent_to_options(cur.extent);
                let dest = format::format_anchor_address(&cur.path.to_string_lossy(), s, e);
                if src.is_empty() {
                    format!("Moved to `{dest}`")
                } else {
                    format!("Moved {src} to `{dest}`")
                }
            } else {
                if src.is_empty() {
                    "Moved".to_string()
                } else {
                    format!("Moved {src}")
                }
            }
        }
        AnchorStatus::Orphaned => {
            let label = super::drift_label::format_drift_label(
                &f.status,
                f.source,
                f.locus.as_ref(),
                f.current.is_some(),
            );
            uppercase_first(&label)
        }
        AnchorStatus::MergeConflict => {
            let src = src_phrase(f.source);
            if src.is_empty() {
                "Merge conflict".to_string()
            } else {
                format!("Merge conflict {src}")
            }
        }
        AnchorStatus::Submodule => {
            let src = src_phrase(f.source);
            if src.is_empty() {
                "Submodule".to_string()
            } else {
                format!("Submodule {src}")
            }
        }
        AnchorStatus::ContentUnavailable(reason) => {
            let src = src_phrase(f.source);
            let detail = match reason {
                UnavailableReason::LfsNotFetched => "LFS not fetched",
                UnavailableReason::LfsNotInstalled => "LFS not installed",
                UnavailableReason::PromisorMissing => "promisor missing",
                UnavailableReason::SparseExcluded => "sparse excluded",
                UnavailableReason::FilterFailed { .. } => "filter failed",
                UnavailableReason::IoError { .. } => "I/O error",
            };
            if src.is_empty() {
                format!("Content unavailable ({detail})")
            } else {
                format!("Content unavailable {src} ({detail})")
            }
        }
        AnchorStatus::Fresh => unreachable!("Fresh anchors have no description"),
    }
}

/// Lowercase prose description for use in the unified block-shape suffix.
///
/// Returns strings like:
/// - "changed"
/// - "changed in HEAD"
/// - "moved to new/path#L1-L10"
/// - "orphaned in HEAD (path no longer exists)"
fn describe_finding_lower(f: &Finding) -> String {
    let src_phrase = |src: Option<DriftSource>| -> &'static str {
        match src {
            Some(DriftSource::Head) => "in HEAD",
            Some(DriftSource::Index) => "in the index",
            Some(DriftSource::Worktree) => "in the working tree",
            None => "",
        }
    };

    match &f.status {
        AnchorStatus::Changed => super::drift_label::format_drift_label(
            &f.status,
            f.source,
            f.locus.as_ref(),
            f.current.is_some(),
        ),
        AnchorStatus::Moved => {
            let src = src_phrase(f.source);
            if let Some(cur) = &f.current {
                let dest = render_path_extent_plain(&cur.path, cur.extent);
                if src.is_empty() {
                    format!("moved to {dest}")
                } else {
                    format!("moved {src} to {dest}")
                }
            } else if src.is_empty() {
                "moved".to_string()
            } else {
                format!("moved {src}")
            }
        }
        AnchorStatus::Orphaned => super::drift_label::format_drift_label(
            &f.status,
            f.source,
            f.locus.as_ref(),
            f.current.is_some(),
        ),
        AnchorStatus::MergeConflict => {
            let src = src_phrase(f.source);
            if src.is_empty() {
                "merge conflict".to_string()
            } else {
                format!("merge conflict {src}")
            }
        }
        AnchorStatus::Submodule => {
            let src = src_phrase(f.source);
            if src.is_empty() {
                "submodule".to_string()
            } else {
                format!("submodule {src}")
            }
        }
        AnchorStatus::ContentUnavailable(reason) => {
            let src = src_phrase(f.source);
            let detail = match reason {
                UnavailableReason::LfsNotFetched => "LFS not fetched",
                UnavailableReason::LfsNotInstalled => "LFS not installed",
                UnavailableReason::PromisorMissing => "promisor missing",
                UnavailableReason::SparseExcluded => "sparse excluded",
                UnavailableReason::FilterFailed { .. } => "filter failed",
                UnavailableReason::IoError { .. } => "I/O error",
            };
            if src.is_empty() {
                format!("content unavailable ({detail})")
            } else {
                format!("content unavailable {src} ({detail})")
            }
        }
        AnchorStatus::Fresh => unreachable!("Fresh anchors have no description"),
    }
}

/// Uppercase the first character of a lowercase label.
fn uppercase_first(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

fn status_str(s: &AnchorStatus) -> &'static str {
    match s {
        AnchorStatus::Fresh => "FRESH",
        AnchorStatus::Moved => "MOVED",
        AnchorStatus::Changed => "CHANGED",
        AnchorStatus::Orphaned => "ORPHANED",
        AnchorStatus::MergeConflict => "MERGE_CONFLICT",
        AnchorStatus::Submodule => "SUBMODULE",
        AnchorStatus::ContentUnavailable(reason) => match reason {
            UnavailableReason::LfsNotFetched => "LFS_NOT_FETCHED",
            UnavailableReason::LfsNotInstalled => "LFS_NOT_INSTALLED",
            UnavailableReason::PromisorMissing => "PROMISOR_MISSING",
            UnavailableReason::SparseExcluded => "SPARSE_EXCLUDED",
            UnavailableReason::FilterFailed { .. } => "FILTER_FAILED",
            UnavailableReason::IoError { .. } => "IO_ERROR",
        },
    }
}

fn status_word(s: &AnchorStatus) -> &'static str {
    match s {
        AnchorStatus::Fresh => "Fresh",
        AnchorStatus::Moved => "Moved",
        AnchorStatus::Changed => "Changed",
        AnchorStatus::Orphaned => "Orphaned",
        AnchorStatus::MergeConflict => "Merge conflict",
        AnchorStatus::Submodule => "Submodule",
        AnchorStatus::ContentUnavailable(_) => "Content unavailable",
    }
}

fn source_word(src: DriftSource) -> &'static str {
    match src {
        DriftSource::Head => "HEAD",
        DriftSource::Index => "index",
        DriftSource::Worktree => "worktree",
    }
}

fn source_marker(src: Option<DriftSource>) -> &'static str {
    match src {
        Some(DriftSource::Head) => "H",
        Some(DriftSource::Index) => "I",
        Some(DriftSource::Worktree) => "W",
        None => "-",
    }
}

/// Human-facing `(path, extent)` render. Whole-file pins read
/// `hero.png  (whole)`; line ranges read `src/foo.rs#L1-L10`.
fn render_path_extent_human(path: &std::path::Path, extent: AnchorExtent) -> String {
    match extent {
        AnchorExtent::WholeFile => format!("{}  (whole)", path.display()),
        AnchorExtent::LineRange { start, end } => {
            format!("{}#L{}-L{}", path.display(), start, end)
        }
    }
}

/// Plain `(path, extent)` render for the bullet listing — whole-file
/// pins drop the `(whole)` decoration since the bare path already
/// communicates "this entire file" in context.
fn render_path_extent_plain(path: &std::path::Path, extent: AnchorExtent) -> String {
    match extent {
        AnchorExtent::WholeFile => format!("{}", path.display()),
        AnchorExtent::LineRange { start, end } => {
            format!("{}#L{}-L{}", path.display(), start, end)
        }
    }
}

fn render_pending_range_id(anchor_id: &str) -> String {
    if anchor_id.is_empty() {
        String::new()
    } else {
        format!(" ({anchor_id})")
    }
}

// ---------------------------------------------------------------------------
// Human renderer.
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug)]
struct HumanRenderOptions {
    oneline: bool,
    stat: bool,
    patch: bool,
    show_src: bool,
    named_lookup: bool,
}

fn render_human(
    repo: &gix::Repository,
    meshes: &[MeshResolved],
    findings: &[Finding],
    pending: &[PendingFinding],
    followed_ids: &HashSet<String>,
    options: HumanRenderOptions,
) -> Result<bool> {
    // named_lookup: true when positional args were given (named lookup mode).
    // For workspace scan: suppress clean meshes and pending bullets.
    // For named lookup: always render block, append pending bullets.
    let is_named_lookup = options.named_lookup;

    let mut printed_any_mesh = false;
    for m in meshes.iter() {
        // Build per-mesh collapsed findings (one row per anchor, deepest layer).
        let mesh_findings_owned: Vec<Finding> = m
            .anchors
            .iter()
            .flat_map(|r| {
                if r.status == AnchorStatus::Fresh {
                    vec![Finding {
                        mesh: m.name.clone(),
                        anchor_id: r.anchor_id.clone(),
                        status: AnchorStatus::Fresh,
                        source: None,
                        anchored: r.anchored.clone(),
                        current: r.current.clone(),
                        acknowledged_by: r.acknowledged_by.clone(),
                        locus: None,
                    }]
                } else {
                    // Collapse per-layer expansions to a single row per
                    // anchor, picking the deepest drifting source
                    // (Worktree > Index > HEAD).
                    let deepest = findings
                        .iter()
                        .filter(|f| f.mesh == m.name && f.anchor_id == r.anchor_id)
                        .max_by_key(|f| match f.source {
                            Some(DriftSource::Worktree) => 3,
                            Some(DriftSource::Index) => 2,
                            Some(DriftSource::Head) => 1,
                            None => 0,
                        })
                        .cloned();
                    deepest.into_iter().collect()
                }
            })
            .collect();
        let mesh_findings: Vec<&Finding> = mesh_findings_owned.iter().collect();
        let mesh_pending: Vec<&PendingFinding> = pending
            .iter()
            .filter(|p| pending_mesh(p) == m.name.as_str())
            .collect();

        let mesh_stale = mesh_findings
            .iter()
            .filter(|f| f.status != AnchorStatus::Fresh)
            .count();

        // Workspace scan: suppress meshes with no stale anchors (including
        // meshes that only have pending staged ops). Named lookup: always
        // render the block even if fully clean.
        if !is_named_lookup && mesh_stale == 0 {
            continue;
        }

        if printed_any_mesh {
            println!();
            println!("---");
            println!();
        }
        printed_any_mesh = true;

        if options.oneline {
            for f in &mesh_findings {
                println!(
                    "{:<8}  {}",
                    status_str(&f.status),
                    render_path_extent_human(&f.anchored.path, f.anchored.extent),
                );
            }
            continue;
        }

        if options.stat {
            let mesh_total = mesh_findings.len();
            if mesh_stale > 0 && mesh_stale == mesh_total {
                println!("All anchors in {} are stale:", m.name);
            } else if mesh_stale > 0 {
                println!(
                    "{} out of {} anchors in mesh {} are stale:",
                    mesh_stale, mesh_total, m.name
                );
            }
            for f in &mesh_findings {
                let (insertions, deletions) = diff_counts(repo, f);
                println!(
                    "  {} | +{} -{}",
                    render_path_extent_human(&f.anchored.path, f.anchored.extent),
                    insertions,
                    deletions
                );
            }
            continue;
        }

        // --- DEFAULT UNIFIED BLOCK OUTPUT ---
        // Shape: ## <mesh-name>
        //        - <plain-path-extent>[ — <status>][ — auto-updated][ — acknowledged]
        //        (blank line)
        //        <why text>

        println!("## {}", m.name);

        // Named lookup only: count pending add/remove bullets.
        let pending_add_remove: Vec<&PendingFinding> = if is_named_lookup {
            mesh_pending
                .iter()
                .filter(|p| matches!(p, PendingFinding::Add { .. } | PendingFinding::Remove { .. }))
                .copied()
                .collect()
        } else {
            Vec::new()
        };

        if m.anchors.is_empty() && pending_add_remove.is_empty() {
            println!("*Mesh has no anchors*");
        } else {
            // Committed anchors in stored order.
            for f in &mesh_findings {
                let addr = render_path_extent_plain(&f.anchored.path, f.anchored.extent);
                if f.status == AnchorStatus::Fresh {
                    println!("- {addr}");
                } else {
                    let is_followed = followed_ids.contains(&f.anchor_id);
                    let desc = describe_finding_lower(f);
                    let auto_tag = if is_followed { " — auto-updated" } else { "" };
                    let ack_tag = if f.acknowledged_by.is_some() { " — acknowledged" } else { "" };
                    println!("- {addr} — {desc}{auto_tag}{ack_tag}");
                    if options.patch {
                        let diff = render_patch(repo, f);
                        if !diff.trim().is_empty() {
                            println!("{diff}");
                        }
                    }
                }
            }

            // Named lookup only: append pending bullets after committed anchors.
            for p in &pending_add_remove {
                match p {
                    PendingFinding::Add { op, drift, .. } => {
                        let path = std::path::Path::new(&op.path);
                        let addr = render_path_extent_plain(path, op.extent);
                        let suffix = pending_drift_suffix(drift.as_ref());
                        println!("- {addr} — pending add{suffix}");
                    }
                    PendingFinding::Remove { op, drift, .. } => {
                        let path = std::path::Path::new(&op.path);
                        let addr = render_path_extent_plain(path, op.extent);
                        let suffix = pending_drift_suffix(drift.as_ref());
                        println!("- {addr} — pending remove{suffix}");
                    }
                    _ => {}
                }
            }
        }

        // Why text: print verbatim after a blank line if non-empty.
        let why = m.message.trim_end_matches('\n');
        if !why.is_empty() {
            println!();
            println!("{why}");
        }
    }
    Ok(printed_any_mesh)
}

fn diff_counts(repo: &gix::Repository, finding: &Finding) -> (usize, usize) {
    let (old, new) = finding_text_pair(repo, finding);
    let diff = similar::TextDiff::from_lines(&old, &new);
    let mut insertions = 0;
    let mut deletions = 0;
    for change in diff.iter_all_changes() {
        match change.tag() {
            similar::ChangeTag::Delete => deletions += 1,
            similar::ChangeTag::Insert => insertions += 1,
            similar::ChangeTag::Equal => {}
        }
    }
    (insertions, deletions)
}

fn render_patch(repo: &gix::Repository, finding: &Finding) -> String {
    let (old, new) = finding_text_pair(repo, finding);
    let old_header = format!(
        "{} (anchored)",
        render_path_extent_human(&finding.anchored.path, finding.anchored.extent)
    );
    let new_header = finding
        .current
        .as_ref()
        .map(|c| render_path_extent_human(&c.path, c.extent))
        .unwrap_or_else(|| "(missing)".to_string());
    similar::TextDiff::from_lines(&old, &new)
        .unified_diff()
        .header(&old_header, &new_header)
        .to_string()
}

fn finding_text_pair(repo: &gix::Repository, finding: &Finding) -> (String, String) {
    let old = read_location_text(repo, &finding.anchored);
    let new = finding
        .current
        .as_ref()
        .map(|current| read_location_text(repo, current))
        .unwrap_or_default();
    (old, new)
}

fn read_location_text(repo: &gix::Repository, location: &AnchorLocation) -> String {
    let bytes = if let Some(blob) = location.blob {
        read_blob_bytes(repo, blob).unwrap_or_default()
    } else {
        let Some(workdir) = repo.workdir() else {
            return String::new();
        };
        std::fs::read(workdir.join(&location.path)).unwrap_or_default()
    };
    let text = String::from_utf8_lossy(&bytes);
    match location.extent {
        AnchorExtent::WholeFile => text.into_owned(),
        AnchorExtent::LineRange { start, end } => slice_lines(&text, start, end),
    }
}

fn read_blob_bytes(repo: &gix::Repository, oid: gix::ObjectId) -> Option<Vec<u8>> {
    repo.find_object(oid)
        .ok()
        .map(|object| object.into_blob().detach().data)
}

fn slice_lines(text: &str, start: u32, end: u32) -> String {
    let start_idx = start.saturating_sub(1) as usize;
    let count = end.saturating_sub(start).saturating_add(1) as usize;
    let mut out = text
        .lines()
        .skip(start_idx)
        .take(count)
        .collect::<Vec<_>>()
        .join("\n");
    if !out.is_empty() {
        out.push('\n');
    }
    out
}

fn pending_drift_suffix(drift: Option<&PendingDrift>) -> &'static str {
    match drift {
        Some(PendingDrift::SidecarMismatch) => " (sidecar mismatch)",
        Some(PendingDrift::SidecarTampered) => " (sidecar tampered)",
        None => "",
    }
}

fn drift_note(drift: Option<&PendingDrift>) -> String {
    match drift {
        Some(PendingDrift::SidecarMismatch) => "  (drift: sidecar mismatch)".into(),
        Some(PendingDrift::SidecarTampered) => "  (drift: sidecar tampered)".into(),
        None => String::new(),
    }
}

fn config_str(c: &StagedConfig) -> String {
    match c {
        StagedConfig::CopyDetection(cd) => format!("copy-detection={cd:?}"),
        StagedConfig::IgnoreWhitespace(b) => format!("ignore-whitespace={b}"),
        StagedConfig::FollowMoves(b) => format!("follow-moves={b}"),
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

// ---------------------------------------------------------------------------
// Porcelain renderer.
// ---------------------------------------------------------------------------

fn render_porcelain(findings: &[Finding], show_src: bool) {
    if findings.is_empty() {
        return;
    }
    println!("# porcelain v2");
    for f in findings {
        // Whole-file pins emit `(whole)\t-` in place of the two line
        // columns, keeping the column count stable for parsers.
        let (start_col, end_col) = match f.anchored.extent {
            AnchorExtent::WholeFile => ("(whole)".to_string(), "-".to_string()),
            AnchorExtent::LineRange { start, end } => (start.to_string(), end.to_string()),
        };
        if show_src {
            let mut src = source_marker(f.source).to_string();
            if f.acknowledged_by.is_some() {
                src.push_str("/ack");
            }
            println!(
                "{}\t{}\t{}\t{}\t{}\t{}",
                status_str(&f.status),
                src,
                f.mesh,
                f.anchored.path.display(),
                start_col,
                end_col,
            );
        } else {
            println!(
                "{}\t{}\t{}\t{}\t{}",
                status_str(&f.status),
                f.mesh,
                f.anchored.path.display(),
                start_col,
                end_col,
            );
        }
    }
}

// ---------------------------------------------------------------------------
// JSON renderer (`{ "schema_version": 1, findings, pending }`).
// ---------------------------------------------------------------------------

fn render_json(
    meshes: &[MeshResolved],
    findings: &[Finding],
    pending: &[PendingFinding],
    followed_ids: &HashSet<String>,
) -> Result<()> {
    if findings.is_empty() && pending.is_empty() {
        return Ok(());
    }
    let v = json!({
        "schema_version": 2,
        "mesh": meshes.first().map(|m| m.name.clone()).unwrap_or_default(),
        "findings": findings.iter().map(|f| finding_json(f, followed_ids)).collect::<Vec<_>>(),
        "pending": pending.iter().map(pending_json).collect::<Vec<_>>(),
    });
    println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default());
    Ok(())
}

fn location_json(loc: &AnchorLocation) -> Value {
    json!({
        "path": loc.path.display().to_string(),
        "extent": extent_json(loc.extent),
        "blob": loc.blob.map(|o| o.to_string()),
    })
}

fn extent_json(e: AnchorExtent) -> Value {
    match e {
        AnchorExtent::WholeFile => json!({ "kind": "whole" }),
        AnchorExtent::LineRange { start, end } => json!({
            "kind": "lines",
            "start": start,
            "end": end,
        }),
    }
}

fn status_json(s: &AnchorStatus) -> Value {
    match s {
        AnchorStatus::ContentUnavailable(reason) => json!({
            "code": "CONTENT_UNAVAILABLE",
            "reason": status_str(s),
            "detail": match reason {
                UnavailableReason::FilterFailed { filter } => json!({"filter": filter}),
                UnavailableReason::IoError { message } => json!({"message": message}),
                _ => Value::Null,
            }
        }),
        _ => json!({ "code": status_str(s) }),
    }
}

fn finding_json(f: &Finding, followed_ids: &HashSet<String>) -> Value {
    let moved_to = if f.status == AnchorStatus::Moved {
        f.current.as_ref().map(|loc| {
            json!({
                "path": loc.path.display().to_string(),
                "extent": extent_json(loc.extent),
            })
        })
    } else {
        None
    };
    let auto_followed = followed_ids.contains(&f.anchor_id);
    json!({
        "mesh": f.mesh,
        "status": status_json(&f.status),
        "source": f.source.map(|s| match s {
            DriftSource::Head => "HEAD",
            DriftSource::Index => "INDEX",
            DriftSource::Worktree => "WORKTREE",
        }),
        "anchored": location_json(&f.anchored),
        "current": f.current.as_ref().map(location_json),
        "moved_to": moved_to,
        "auto_followed": if auto_followed { Value::Bool(true) } else { Value::Null },
        "acknowledged_by": f.acknowledged_by.as_ref().map(staged_op_ref_json),
        "locus": match f.locus {
            Some(DriftLocus::ChangedAt(oid)) => json!({ "changed_in": oid.to_string() }),
            Some(DriftLocus::OrphanedAt(oid)) => json!({ "orphaned_in": oid.to_string() }),
            Some(DriftLocus::Unreachable) | None => Value::Null,
        },
    })
}

fn staged_op_ref_json(s: &StagedOpRef) -> Value {
    json!({ "mesh": s.mesh, "index": s.index })
}

fn staged_add_json(a: &StagedAdd) -> Value {
    json!({
        "line_number": a.line_number,
        "path": a.path,
        "extent": extent_json(a.extent),
        "anchor": a.anchor,
    })
}

fn staged_remove_json(r: &StagedRemove) -> Value {
    json!({
        "path": r.path,
        "extent": extent_json(r.extent),
    })
}

fn staged_config_json(c: &StagedConfig) -> Value {
    match c {
        StagedConfig::CopyDetection(cd) => json!({
            "kind": "copy_detection",
            "value": format!("{cd:?}"),
        }),
        StagedConfig::IgnoreWhitespace(b) => json!({
            "kind": "ignore_whitespace",
            "value": b,
        }),
        StagedConfig::FollowMoves(b) => json!({
            "kind": "follow_moves",
            "value": b,
        }),
    }
}

fn drift_json(d: Option<&PendingDrift>) -> Value {
    match d {
        Some(PendingDrift::SidecarMismatch) => json!("SIDECAR_MISMATCH"),
        Some(PendingDrift::SidecarTampered) => json!("SIDECAR_TAMPERED"),
        None => Value::Null,
    }
}

fn pending_json(p: &PendingFinding) -> Value {
    match p {
        PendingFinding::Add {
            mesh,
            anchor_id,
            op,
            drift,
        } => json!({
            "kind": "add",
            "mesh": mesh,
            "anchor_id": anchor_id,
            "op": staged_add_json(op),
            "drift": drift_json(drift.as_ref()),
        }),
        PendingFinding::Remove {
            mesh,
            anchor_id,
            op,
            drift,
        } => json!({
            "kind": "remove",
            "mesh": mesh,
            "anchor_id": anchor_id,
            "op": staged_remove_json(op),
            "drift": drift_json(drift.as_ref()),
        }),
        PendingFinding::Why { mesh, body } => json!({
            "kind": "why",
            "mesh": mesh,
            "body": body,
        }),
        PendingFinding::ConfigChange { mesh, change } => json!({
            "kind": "config_change",
            "mesh": mesh,
            "change": staged_config_json(change),
        }),
    }
}

// ---------------------------------------------------------------------------
// JUnit / GitHub Actions renderers.
// ---------------------------------------------------------------------------

fn render_junit(findings: &[Finding]) {
    if findings.is_empty() {
        return;
    }
    println!(
        "<testsuite name=\"git-mesh\" tests=\"{}\" failures=\"{}\">",
        findings.len(),
        findings.len()
    );
    for f in findings {
        let addr = render_path_extent_human(&f.anchored.path, f.anchored.extent);
        let src = source_marker(f.source);
        let ack = if f.acknowledged_by.is_some() {
            " (ack)"
        } else {
            ""
        };
        println!(
            "  <testcase classname=\"{}\" name=\"{} [{}]{}\"><failure message=\"{}\"/></testcase>",
            f.mesh,
            addr,
            src,
            ack,
            status_str(&f.status)
        );
    }
    println!("</testsuite>");
}

fn render_github(findings: &[Finding]) {
    for f in findings {
        let level = match f.status {
            AnchorStatus::Moved => "warning",
            _ => "error",
        };
        let src = source_marker(f.source);
        let ack = if f.acknowledged_by.is_some() {
            " (ack)"
        } else {
            ""
        };
        // Whole-file pins omit `,line=N`; line ranges emit the start line.
        let loc = match f.anchored.extent {
            AnchorExtent::WholeFile => format!("file={}", f.anchored.path.display()),
            AnchorExtent::LineRange { start, .. } => {
                format!("file={},line={}", f.anchored.path.display(), start)
            }
        };
        println!(
            "::{level} {}::{} [{}]{}",
            loc,
            status_str(&f.status),
            src,
            ack,
        );
    }
}

// ---------------------------------------------------------------------------
// Kept for `cli/show.rs` — relative-time formatter.
// ---------------------------------------------------------------------------

pub(crate) fn format_relative(committer_time: i64) -> String {
    let now = chrono::Utc::now().timestamp();
    let diff = now - committer_time;
    if diff < 0 {
        return "in the future".into();
    }
    let secs = diff;
    let mins = secs / 60;
    let hours = mins / 60;
    let days = hours / 24;
    let weeks = days / 7;
    let months = days / 30;
    let years = days / 365;
    if years > 0 {
        format!("{years} year{} ago", plural(years))
    } else if months > 0 {
        format!("{months} month{} ago", plural(months))
    } else if weeks > 0 {
        format!("{weeks} week{} ago", plural(weeks))
    } else if days > 0 {
        format!("{days} day{} ago", plural(days))
    } else if hours > 0 {
        format!("{hours} hour{} ago", plural(hours))
    } else if mins > 0 {
        format!("{mins} minute{} ago", plural(mins))
    } else {
        format!("{secs} second{} ago", plural(secs))
    }
}

fn plural(n: i64) -> &'static str {
    if n == 1 {
        ""
    } else {
        "s"
    }
}

/// Auto-follow pass shared by `git mesh stale` and `git mesh stale --compact`.
///
/// Computes `FollowDecision`s for each mesh and rewrites it via
/// `follow_moves` when opt-in is active (`--auto-follow` flag or
/// `follow-moves=true` in mesh config) and all guardrails hold. Returns
/// the set of anchor_ids that were successfully followed in this run.
///
/// Failures are non-fatal: they are logged to stderr; the mesh is left
/// unchanged. The exit-code accounting in the stale renderer subtracts
/// followed anchor_ids from the stale count for the same run.
pub(super) fn run_auto_follow_pass(
    repo: &gix::Repository,
    args: &StaleArgs,
    meshes: &[MeshResolved],
) -> HashSet<String> {
    let mut followed_ids: HashSet<String> = HashSet::new();
    if !(args.auto_follow || meshes.iter().any(|m| effective_follow_moves(repo, &m.name))) {
        return followed_ids;
    }
    let _perf = crate::perf::span("stale.auto-follow");
    for m in meshes {
        if !args.auto_follow && !effective_follow_moves(repo, &m.name) {
            continue;
        }
        // Guardrail: any Changed anchor in this mesh suppresses the entire mesh.
        if m.anchors.iter().any(|r| r.status == AnchorStatus::Changed) {
            continue;
        }
        let mut decisions: Vec<FollowDecision> = Vec::new();
        for r in &m.anchors {
            if r.status != AnchorStatus::Moved {
                continue;
            }
            let Some(cur) = &r.current else { continue };
            // Guardrail: LineRange extents only — whole-file anchors are excluded.
            if !matches!(r.anchored.extent, AnchorExtent::LineRange { .. })
                || !matches!(cur.extent, AnchorExtent::LineRange { .. })
            {
                continue;
            }
            // Guardrail: path must not have been renamed.
            if cur.path != r.anchored.path {
                continue;
            }
            // Guardrail: the move must be committed to HEAD (fail-closed).
            let Some(anchored_file_blob) = r.anchored.blob else {
                continue;
            };
            let head_sha = match git::head_oid(repo) {
                Ok(s) => s,
                Err(_) => continue,
            };
            let head_file_blob =
                match git::path_blob_at(repo, &head_sha, cur.path.to_str().unwrap_or("")) {
                    Ok(s) => s,
                    Err(_) => continue,
                };
            if head_file_blob == anchored_file_blob.to_string() {
                continue;
            }
            decisions.push(FollowDecision {
                anchor_id: r.anchor_id.clone(),
                new_path: cur.path.clone(),
                new_extent: cur.extent,
            });
        }
        if decisions.is_empty() {
            continue;
        }
        match follow_moves(repo, &m.name, &decisions) {
            Ok(_commit) => {
                for d in &decisions {
                    followed_ids.insert(d.anchor_id.clone());
                }
            }
            Err(e) => {
                println!("git mesh stale: auto-follow failed for {}: {e}", m.name);
            }
        }
    }
    followed_ids
}

/// Resolve whether auto-follow is active for a mesh via its config.
///
/// Reads the committed mesh config and overlays any staged config so that
/// `git mesh config <name> follow-moves true` (which only stages the change)
/// is honoured without a separate `git mesh commit` step.
pub(super) fn effective_follow_moves(repo: &gix::Repository, mesh_name: &str) -> bool {
    let committed_fm = crate::mesh::read::read_mesh(repo, mesh_name)
        .map(|m| m.config.follow_moves)
        .unwrap_or(false);
    let staging = crate::staging::read_staging(repo, mesh_name).unwrap_or_default();
    let (_, _, fm) = crate::staging::resolve_staged_config(
        &staging,
        (
            crate::types::DEFAULT_COPY_DETECTION,
            crate::types::DEFAULT_IGNORE_WHITESPACE,
            committed_fm,
        ),
    );
    fm
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::StaleFormat;
    use crate::git::{apply_ref_transaction_repo, RefUpdate};
    use crate::mesh::path_index::ref_updates_for_mesh;
    use crate::types::{Anchor, AnchorExtent};
    use std::path::Path;
    use std::process::Command;

    fn seed_repo() -> (tempfile::TempDir, gix::Repository) {
        let td = tempfile::tempdir().unwrap();
        let dir = td.path();
        run_git(dir, &["init", "--initial-branch=main"]);
        run_git(dir, &["config", "user.email", "t@t"]);
        run_git(dir, &["config", "user.name", "t"]);
        run_git(dir, &["config", "commit.gpgsign", "false"]);
        std::fs::write(dir.join("a.txt"), "alpha\n").unwrap();
        run_git(dir, &["add", "."]);
        run_git(dir, &["commit", "-m", "init"]);
        let repo = gix::open(dir).unwrap();
        (td, repo)
    }

    fn run_git(dir: &Path, args: &[&str]) {
        let out = Command::new("git")
            .current_dir(dir)
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn anchor(path: &str, start: u32, end: u32) -> Anchor {
        Anchor {
            anchor_sha: "0000000000000000000000000000000000000000".to_string(),
            created_at: "2025-01-01T00:00:00Z".to_string(),
            path: path.to_string(),
            extent: AnchorExtent::LineRange { start, end },
            blob: "0000000000000000000000000000000000000000".to_string(),
        }
    }

    fn create_mesh_ref(repo: &gix::Repository, name: &str) {
        let head_oid = repo.head_id().unwrap().detach().to_string();
        let updates = vec![RefUpdate::Create {
            name: format!("refs/meshes/v1/{name}"),
            new_oid: head_oid,
        }];
        apply_ref_transaction_repo(repo, &updates).unwrap();
    }

    fn create_path_index_entry(
        repo: &gix::Repository,
        mesh_name: &str,
        path: &str,
        start: u32,
        end: u32,
    ) {
        let anchors = vec![("a1".to_string(), anchor(path, start, end))];
        let updates = ref_updates_for_mesh(repo, mesh_name, &[], &anchors).unwrap();
        apply_ref_transaction_repo(repo, &updates).unwrap();
    }

    // --- csv_escape unit tests ---

    #[test]
    fn csv_escape_plain_ascii_unchanged() {
        assert_eq!(csv_escape("hello"), "hello");
        assert_eq!(csv_escape(""), "");
        assert_eq!(csv_escape("abc123"), "abc123");
    }

    #[test]
    fn csv_escape_comma_quoted() {
        assert_eq!(csv_escape("a,b"), "\"a,b\"");
    }

    #[test]
    fn csv_escape_double_quote_doubled() {
        assert_eq!(csv_escape("say \"hi\""), "\"say \"\"hi\"\"\"");
    }

    #[test]
    fn csv_escape_lf_quoted() {
        assert_eq!(csv_escape("a\nb"), "\"a\nb\"");
    }

    #[test]
    fn csv_escape_cr_quoted() {
        assert_eq!(csv_escape("a\rb"), "\"a\rb\"");
    }

    // --- hierarchical mesh name reproduction tests ---

    #[test]
    fn run_stale_hierarchical_mesh_name_resolves() {
        let (_td, repo) = seed_repo();
        create_mesh_ref(&repo, "billing/payments/checkout");
        create_path_index_entry(&repo, "billing/payments/checkout", "a.txt", 1, 5);
        let args = StaleArgs {
            paths: vec!["billing/payments/checkout".to_string()],
            format: StaleFormat::Human,
            no_exit_code: false,
            no_worktree: false,
            no_index: false,
            no_staged_mesh: false,
            ignore_unavailable: false,
            oneline: false,
            stat: false,
            patch: false,
            since: None,
            compact: false,
            verbose: false,
            auto_follow: false,
            perf_trace: None,
        };
        let exit_code = run_stale(&repo, args).unwrap();
        assert_eq!(exit_code, 0);
    }

    #[test]
    fn run_stale_hierarchical_name_staging_only() {
        let (_td, repo) = seed_repo();
        let staging_dir = repo.git_dir().join("mesh").join("staging");
        std::fs::create_dir_all(&staging_dir).unwrap();
        std::fs::write(
            staging_dir.join("billing%2Fpayments%2Fcheckout"),
            "add a.txt#L1-L5\n",
        )
        .unwrap();
        let args = StaleArgs {
            paths: vec!["billing/payments/checkout".to_string()],
            format: StaleFormat::Human,
            no_exit_code: false,
            no_worktree: false,
            no_index: false,
            no_staged_mesh: false,
            ignore_unavailable: false,
            oneline: false,
            stat: false,
            patch: false,
            since: None,
            compact: false,
            verbose: false,
            auto_follow: false,
            perf_trace: None,
        };
        let exit_code = run_stale(&repo, args).unwrap();
        assert_eq!(exit_code, 0);
    }
}

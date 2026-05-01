//! `git mesh stale` output rendering — §10.4.
//!
//! Slice 8 of the layered-stale rewrite (see
//! `docs/stale-layers-plan.md` §"Renderers"). Renderers consume
//! `Finding` / `PendingFinding` end-to-end via a thin adapter that maps
//! the engine's `AnchorResolved` + `MeshResolved.pending` into the
//! plan's "Key types" shape.

#![allow(dead_code)]

use crate::cli::{StaleArgs, StaleFormat};
use crate::resolver::{build_pending_findings, resolve_mesh, stale_meshes};
use crate::staging::{StagedAdd, StagedConfig, StagedRemove};
use crate::types::{
    AnchorExtent, AnchorLocation, AnchorStatus, DriftSource, EngineOptions, Finding, LayerSet,
    MeshResolved, PendingDrift, PendingFinding, StagedOpRef, UnavailableReason,
};
use anyhow::Result;
use serde_json::{Value, json};

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
                    .map_err(|e| anyhow::anyhow!("--since `{s}`: {e}"))?,
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

    let meshes = match &args.name {
        Some(n) => {
            let _perf = crate::perf::span("stale.resolve-mesh");
            match resolve_mesh(repo, n, options) {
                Ok(mesh) => vec![mesh],
                Err(crate::Error::MeshNotFound(_)) if layers.staged_mesh => {
                    let pending = build_pending_findings(repo, n);
                    if pending.is_empty() {
                        return Err(crate::Error::MeshNotFound(n.clone()).into());
                    }
                    vec![MeshResolved {
                        name: n.clone(),
                        message: String::new(),
                        anchors: Vec::new(),
                        pending,
                    }]
                }
                Err(e) => return Err(e.into()),
            }
        }
        None => {
            let mut meshes = {
                let _perf = crate::perf::span("stale.resolve-all-meshes");
                stale_meshes(repo, options)?
            };
            if layers.staged_mesh {
                let _perf = crate::perf::span("stale.resolve-staging-only");
                for name in staging_only_mesh_names(repo)? {
                    if meshes.iter().any(|m| m.name == name) {
                        continue;
                    }
                    let pending = build_pending_findings(repo, &name);
                    if !pending.is_empty() {
                        meshes.push(MeshResolved {
                            name,
                            message: String::new(),
                            anchors: Vec::new(),
                            pending,
                        });
                    }
                }
            }
            meshes
        }
    };

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
                                culprit: r.culprit.clone(),
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
        (findings, pending)
    };

    // Plan §B3: an acknowledged finding does not drive exit code; nor
    // does a `ContentUnavailable` finding under `--ignore-unavailable`.
    let unacked_findings: usize = findings
        .iter()
        .filter(|f| {
            if f.acknowledged_by.is_some() {
                return false;
            }
            if args.ignore_unavailable && matches!(f.status, AnchorStatus::ContentUnavailable(_)) {
                return false;
            }
            true
        })
        .count();
    // Pending Add/Remove with `SidecarMismatch` drift drive exit too;
    // Message/ConfigChange never do.
    let pending_drift: usize = pending
        .iter()
        .filter(|p| {
            matches!(
                p,
                PendingFinding::Add { drift: Some(_), .. }
                    | PendingFinding::Remove { drift: Some(_), .. }
            )
        })
        .count();
    let stale_count = unacked_findings + pending_drift;

    match args.format {
        StaleFormat::Human => {
            let _perf = crate::perf::span("stale.render-human");
            render_human(
                repo,
                &meshes,
                &findings,
                &pending,
                HumanRenderOptions {
                    oneline: args.oneline,
                    stat: args.stat,
                    patch: args.patch,
                    show_src: show_src_column,
                },
            )?;
        }
        StaleFormat::Porcelain => {
            let _perf = crate::perf::span("stale.render-porcelain");
            render_porcelain(&findings, show_src_column);
        }
        StaleFormat::Json => {
            let _perf = crate::perf::span("stale.render-json");
            render_json(&meshes, &findings, &pending)?;
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
}

fn render_human(
    repo: &gix::Repository,
    meshes: &[MeshResolved],
    findings: &[Finding],
    pending: &[PendingFinding],
    options: HumanRenderOptions,
) -> Result<()> {
    let mesh_count = meshes.len();
    for (mesh_idx, m) in meshes.iter().enumerate() {
        // Include every tracked anchor — Fresh ones synthesized as
        // `Finding`s so default/oneline/stat/patch all list the full
        // mesh. Order follows the mesh's stored anchor order, with
        // per-layer expansions inlined for non-Fresh anchors.
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
                        culprit: None,
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

        let mesh_total = mesh_findings.len();
        let mesh_stale = mesh_findings
            .iter()
            .filter(|f| f.status != AnchorStatus::Fresh)
            .count();
        let pending_adds = mesh_pending
            .iter()
            .filter(|p| matches!(p, PendingFinding::Add { .. }))
            .count();
        if mesh_total == 0 {
            if pending_adds > 0 {
                println!(
                    "Mesh {} has {} pending anchors:",
                    m.name, pending_adds
                );
            } else {
                println!("Mesh {} has no anchors.", m.name);
            }
        } else if mesh_stale == 0 {
            println!("No anchors in {} are stale.", m.name);
        } else if mesh_stale == mesh_total {
            println!("All anchors in {} are stale:", m.name);
        } else {
            println!(
                "{} out of {} anchors in mesh {} are stale:",
                mesh_stale, mesh_total, m.name
            );
        }
        if options.stat {
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
        for f in &mesh_findings {
            let mut line = String::new();
            line.push_str("- ");
            line.push_str(&render_path_extent_plain(
                &f.anchored.path,
                f.anchored.extent,
            ));
            if f.status != AnchorStatus::Fresh {
                let status_word = status_word(&f.status);
                match (options.show_src, f.source) {
                    (true, Some(src)) => {
                        line.push_str(&format!(" ({} in {})", status_word, source_word(src)));
                    }
                    _ => {
                        line.push_str(&format!(" ({})", status_word));
                    }
                }
            }
            if f.acknowledged_by.is_some() {
                line.push_str(" (ack)");
            }
            println!("{line}");
            if options.patch {
                let diff = render_patch(repo, f);
                if !diff.trim().is_empty() {
                    println!("{diff}");
                }
            }
        }
        // Pending adds/removes — rendered inline with the same
        // `- <path> (<Status>)` shape as committed anchor findings.
        for p in &mesh_pending {
            match p {
                PendingFinding::Add {
                    anchor_id,
                    op,
                    drift,
                    ..
                } => {
                    let drift_note = drift_note(drift.as_ref());
                    println!(
                        "- {}{} (Pending add){}",
                        render_path_extent_plain(std::path::Path::new(&op.path), op.extent),
                        render_pending_range_id(anchor_id),
                        drift_note,
                    );
                }
                PendingFinding::Remove {
                    anchor_id,
                    op,
                    drift,
                    ..
                } => {
                    let drift_note = drift_note(drift.as_ref());
                    println!(
                        "- {}{} (Pending remove){}",
                        render_path_extent_plain(std::path::Path::new(&op.path), op.extent),
                        render_pending_range_id(anchor_id),
                        drift_note,
                    );
                }
                _ => {}
            }
        }
        // Informational pending (Why / ConfigChange) — never drives exit.
        let info: Vec<&&PendingFinding> = mesh_pending
            .iter()
            .filter(|p| {
                matches!(
                    p,
                    PendingFinding::Why { .. } | PendingFinding::ConfigChange { .. }
                )
            })
            .collect();
        // Prefer the committed why on the mesh ref. The pending Why
        // (if any) is shown afterwards so a staged edit is visible
        // alongside the live message.
        let trimmed_why = m.message.trim();
        if !trimmed_why.is_empty() {
            println!();
            println!("{trimmed_why}");
        }
        for p in &info {
            match p {
                PendingFinding::Why { body, .. } => {
                    println!();
                    println!("{body}");
                }
                PendingFinding::ConfigChange { change, .. } => {
                    println!();
                    println!("{}", config_str(change));
                }
                _ => {}
            }
        }
        if mesh_idx + 1 < mesh_count {
            println!();
            println!("---");
            println!();
        }
    }
    Ok(())
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
) -> Result<()> {
    if findings.is_empty() && pending.is_empty() {
        return Ok(());
    }
    let v = json!({
        "schema_version": 2,
        "mesh": meshes.first().map(|m| m.name.clone()).unwrap_or_default(),
        "findings": findings.iter().map(finding_json).collect::<Vec<_>>(),
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

fn finding_json(f: &Finding) -> Value {
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
        "acknowledged_by": f.acknowledged_by.as_ref().map(staged_op_ref_json),
        "culprit": f.culprit.as_ref().map(|c| json!({
            "commit": c.commit.to_string(),
            "author": c.author,
            "summary": c.summary,
        })),
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
    if n == 1 { "" } else { "s" }
}

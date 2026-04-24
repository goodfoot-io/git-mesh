//! `git mesh stale` output rendering — §10.4.
//!
//! Slice 8 of the layered-stale rewrite (see
//! `docs/stale-layers-plan.md` §"Renderers"). Renderers consume
//! `Finding` / `PendingFinding` end-to-end via a thin adapter that maps
//! the engine's `RangeResolved` + `MeshResolved.pending` into the
//! plan's "Key types" shape.

#![allow(dead_code)]

use crate::cli::{StaleArgs, StaleFormat};
use crate::resolver::{resolve_mesh, stale_meshes};
use crate::staging::{StagedAdd, StagedConfig, StagedRemove};
use crate::types::{
    DriftSource, EngineOptions, Finding, LayerSet, MeshResolved, PendingDrift, PendingFinding,
    RangeExtent, RangeLocation, RangeStatus, StagedOpRef, UnavailableReason,
};
use anyhow::Result;
use serde_json::{Value, json};

pub fn run_stale(repo: &gix::Repository, args: StaleArgs) -> Result<i32> {
    let layers = LayerSet {
        worktree: !args.no_worktree,
        index: !args.no_index,
        staged_mesh: !args.no_staged_mesh,
    };
    let show_src_column = layers.worktree || layers.index;
    // Slice 5: resolve `--since <commit-ish>` once, fail-closed on
    // unresolvable input (no silent fallback per `<fail-closed>`).
    let since = match args.since.as_deref() {
        Some(s) => Some(
            crate::git::resolve_commit(repo, s)
                .map(|hex| {
                    use std::str::FromStr;
                    gix::ObjectId::from_str(&hex).expect("resolve_commit returns valid hex")
                })
                .map_err(|e| anyhow::anyhow!("--since `{s}`: {e}"))?,
        ),
        None => None,
    };
    let options = EngineOptions {
        layers,
        ignore_unavailable: args.ignore_unavailable,
        since,
    };

    let meshes = match &args.name {
        Some(n) => vec![resolve_mesh(repo, n, options)?],
        None => stale_meshes(repo, options)?,
    };

    // Adapter: engine output (`MeshResolved`) → renderer input
    // (`Finding` / `PendingFinding`). The adapter is a pure data shape
    // transform; semantics live in the engine.
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

    // Plan §B3: an acknowledged finding does not drive exit code; nor
    // does a `ContentUnavailable` finding under `--ignore-unavailable`.
    let unacked_findings: usize = findings
        .iter()
        .filter(|f| {
            if f.acknowledged_by.is_some() {
                return false;
            }
            if args.ignore_unavailable && matches!(f.status, RangeStatus::ContentUnavailable(_)) {
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
        StaleFormat::Human => render_human(
            &meshes,
            &findings,
            &pending,
            args.oneline,
            args.stat,
            show_src_column,
        )?,
        StaleFormat::Porcelain => render_porcelain(&findings, show_src_column),
        StaleFormat::Json => render_json(&meshes, &findings, &pending)?,
        StaleFormat::Junit => render_junit(&findings),
        StaleFormat::GithubActions => render_github(&findings),
    }

    let exit = if stale_count == 0 || args.no_exit_code {
        0
    } else {
        1
    };
    Ok(exit)
}

// ---------------------------------------------------------------------------
// Shared formatting helpers.
// ---------------------------------------------------------------------------

fn extent_lines(extent: RangeExtent) -> (u32, u32) {
    match extent {
        RangeExtent::Lines { start, end } => (start, end),
        RangeExtent::Whole => (0, 0),
    }
}

fn status_str(s: &RangeStatus) -> &'static str {
    match s {
        RangeStatus::Fresh => "FRESH",
        RangeStatus::Moved => "MOVED",
        RangeStatus::Changed => "CHANGED",
        RangeStatus::Orphaned => "ORPHANED",
        RangeStatus::MergeConflict => "MERGE_CONFLICT",
        RangeStatus::Submodule => "SUBMODULE",
        RangeStatus::ContentUnavailable(reason) => match reason {
            UnavailableReason::LfsNotFetched => "LFS_NOT_FETCHED",
            UnavailableReason::LfsNotInstalled => "LFS_NOT_INSTALLED",
            UnavailableReason::PromisorMissing => "PROMISOR_MISSING",
            UnavailableReason::SparseExcluded => "SPARSE_EXCLUDED",
            UnavailableReason::FilterFailed { .. } => "FILTER_FAILED",
            UnavailableReason::IoError { .. } => "IO_ERROR",
        },
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

fn extent_str(extent: RangeExtent) -> String {
    match extent {
        RangeExtent::Whole => "whole".into(),
        RangeExtent::Lines { start, end } => format!("L{start}-L{end}"),
    }
}

/// Human-facing `(path, extent)` render. Whole-file pins read
/// `hero.png  (whole)`; line ranges read `src/foo.rs#L1-L10`.
fn render_path_extent_human(path: &std::path::Path, extent: RangeExtent) -> String {
    match extent {
        RangeExtent::Whole => format!("{}  (whole)", path.display()),
        RangeExtent::Lines { start, end } => {
            format!("{}#L{}-L{}", path.display(), start, end)
        }
    }
}

// ---------------------------------------------------------------------------
// Human renderer.
// ---------------------------------------------------------------------------

fn render_human(
    meshes: &[MeshResolved],
    findings: &[Finding],
    pending: &[PendingFinding],
    oneline: bool,
    stat: bool,
    show_src: bool,
) -> Result<()> {
    for m in meshes {
        let mesh_findings: Vec<&Finding> =
            findings.iter().filter(|f| f.mesh == m.name).collect();
        let mesh_pending: Vec<&PendingFinding> = pending
            .iter()
            .filter(|p| pending_mesh(p) == m.name.as_str())
            .collect();

        if oneline {
            for f in &mesh_findings {
                println!(
                    "{:<8}  {}",
                    status_str(&f.status),
                    render_path_extent_human(&f.anchored.path, f.anchored.extent),
                );
            }
            continue;
        }

        let mesh_total = m.ranges.len();
        let mesh_stale = mesh_findings.len();
        println!("mesh {}", m.name);
        println!();
        println!("{mesh_stale} stale of {mesh_total} ranges:");
        println!();
        if stat {
            continue;
        }
        for f in &mesh_findings {
            let mut line = String::new();
            if show_src {
                line.push_str(source_marker(f.source));
                line.push(' ');
            }
            line.push_str(status_str(&f.status));
            line.push(' ');
            line.push_str(&render_path_extent_human(
                &f.anchored.path,
                f.anchored.extent,
            ));
            if f.acknowledged_by.is_some() {
                line.push_str("  (ack)");
            }
            println!("  {line}");
        }
        // Pending adds/removes (trailing in the mesh's section).
        let pending_drift_section: Vec<&&PendingFinding> = mesh_pending
            .iter()
            .filter(|p| {
                matches!(
                    p,
                    PendingFinding::Add { .. } | PendingFinding::Remove { .. }
                )
            })
            .collect();
        if !pending_drift_section.is_empty() {
            println!();
            println!("Pending mesh ops:");
            for p in &pending_drift_section {
                match p {
                    PendingFinding::Add {
                        range_id, op, drift, ..
                    } => {
                        let drift_note = drift_note(drift.as_ref());
                        println!(
                            "  ADD    {} {} ({}){}",
                            op.path,
                            extent_str(op.extent),
                            range_id,
                            drift_note,
                        );
                    }
                    PendingFinding::Remove {
                        range_id, op, drift, ..
                    } => {
                        let drift_note = drift_note(drift.as_ref());
                        println!(
                            "  REMOVE {} {} ({}){}",
                            op.path,
                            extent_str(op.extent),
                            range_id,
                            drift_note,
                        );
                    }
                    _ => {}
                }
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
        if !info.is_empty() {
            println!();
            println!("Pending mesh metadata:");
            for p in &info {
                match p {
                    PendingFinding::Why { body, .. } => {
                        println!("  why: {body}");
                    }
                    PendingFinding::ConfigChange { change, .. } => {
                        println!("  config:  {}", config_str(change));
                    }
                    _ => {}
                }
            }
        }
        println!();
    }
    Ok(())
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
    println!("# porcelain v1");
    for f in findings {
        // Whole-file pins emit `(whole)\t-` in place of the two line
        // columns, keeping the column count stable for parsers.
        let (start_col, end_col) = match f.anchored.extent {
            RangeExtent::Whole => ("(whole)".to_string(), "-".to_string()),
            RangeExtent::Lines { start, end } => (start.to_string(), end.to_string()),
        };
        // Anchor sha lives on RangeResolved, not Finding. Recovering the
        // short anchor for porcelain output goes through the engine; the
        // adapter doesn't carry it. Slice 3 emitted an 8-char prefix; we
        // emit `-` to keep the column count stable when adapter input
        // doesn't surface it.
        let anchor_short = "-";
        if show_src {
            let mut src = source_marker(f.source).to_string();
            if f.acknowledged_by.is_some() {
                src.push_str("/ack");
            }
            println!(
                "{}\t{}\t{}\t{}\t{}\t{}\t{}",
                status_str(&f.status),
                src,
                f.mesh,
                f.anchored.path.display(),
                start_col,
                end_col,
                anchor_short
            );
        } else {
            println!(
                "{}\t{}\t{}\t{}\t{}\t{}",
                status_str(&f.status),
                f.mesh,
                f.anchored.path.display(),
                start_col,
                end_col,
                anchor_short
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
    let v = json!({
        "schema_version": 1,
        "mesh": meshes.first().map(|m| m.name.clone()).unwrap_or_default(),
        "findings": findings.iter().map(finding_json).collect::<Vec<_>>(),
        "pending": pending.iter().map(pending_json).collect::<Vec<_>>(),
    });
    println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default());
    Ok(())
}

fn location_json(loc: &RangeLocation) -> Value {
    json!({
        "path": loc.path.display().to_string(),
        "extent": extent_json(loc.extent),
        "blob": loc.blob.map(|o| o.to_string()),
    })
}

fn extent_json(e: RangeExtent) -> Value {
    match e {
        RangeExtent::Whole => json!({ "kind": "whole" }),
        RangeExtent::Lines { start, end } => json!({
            "kind": "lines",
            "start": start,
            "end": end,
        }),
    }
}

fn status_json(s: &RangeStatus) -> Value {
    match s {
        RangeStatus::ContentUnavailable(reason) => json!({
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
        "range_id": f.range_id,
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
            range_id,
            op,
            drift,
        } => json!({
            "kind": "add",
            "mesh": mesh,
            "range_id": range_id,
            "op": staged_add_json(op),
            "drift": drift_json(drift.as_ref()),
        }),
        PendingFinding::Remove {
            mesh,
            range_id,
            op,
            drift,
        } => json!({
            "kind": "remove",
            "mesh": mesh,
            "range_id": range_id,
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
            RangeStatus::Moved => "warning",
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
            RangeExtent::Whole => format!("file={}", f.anchored.path.display()),
            RangeExtent::Lines { start, .. } => {
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

//! `git mesh stale` output rendering — §10.4.
//!
//! Slice 1 of the layered-stale rewrite (see
//! `docs/stale-layers-plan.md`): only the HEAD-only fast path is wired,
//! producing porcelain / human / JSON / JUnit / GitHub-Actions output
//! over the new `RangeResolved` shape. Layered modes (`worktree` /
//! `index` / `staged-mesh`) bail until later slices land.

#![allow(dead_code)]

use crate::cli::{StaleArgs, StaleFormat};
use crate::stale::{resolve_mesh, stale_meshes};
use crate::types::{
    EngineOptions, LayerSet, MeshResolved, RangeExtent, RangeResolved, RangeStatus,
    UnavailableReason,
};
use anyhow::{bail, Result};

pub fn run_stale(repo: &gix::Repository, args: StaleArgs) -> Result<i32> {
    // Slice 1: only HEAD-only mode is supported. The CLI flags
    // `--no-worktree --no-index --no-staged-mesh` together collapse to
    // `LayerSet::committed_only()`.
    if !args.no_worktree || !args.no_index || !args.no_staged_mesh {
        bail!("layered modes pending later slice; pass --no-worktree --no-index --no-staged-mesh");
    }
    let options = EngineOptions {
        layers: LayerSet::committed_only(),
        ignore_unavailable: args.ignore_unavailable,
    };

    let meshes = match &args.name {
        Some(n) => vec![resolve_mesh(repo, n, options)?],
        None => stale_meshes(repo, options)?,
    };

    let mut findings: Vec<(String, RangeResolved)> = Vec::new();
    for m in &meshes {
        for r in &m.ranges {
            if r.status != RangeStatus::Fresh {
                findings.push((m.name.clone(), r.clone()));
            }
        }
    }
    let stale_count = findings.len();

    match args.format {
        StaleFormat::Human => render_human(&meshes, &findings, args.oneline, args.stat)?,
        StaleFormat::Porcelain => render_porcelain(&findings),
        StaleFormat::Json => render_json(&meshes, &findings)?,
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
        RangeStatus::ContentUnavailable(_) => "CONTENT_UNAVAILABLE",
    }
}

fn render_human(
    meshes: &[MeshResolved],
    findings: &[(String, RangeResolved)],
    oneline: bool,
    stat: bool,
) -> Result<()> {
    for m in meshes {
        let mesh_findings: Vec<&(String, RangeResolved)> =
            findings.iter().filter(|(n, _)| n == &m.name).collect();
        if oneline {
            for (_, r) in &mesh_findings {
                let (s, e) = extent_lines(r.anchored.extent);
                println!(
                    "{:<8}  {}#L{}-L{}",
                    status_str(&r.status),
                    r.anchored.path.display(),
                    s,
                    e
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
        for (_, r) in &mesh_findings {
            let (s, e) = extent_lines(r.anchored.extent);
            println!(
                "  {} {}#L{}-L{}",
                status_str(&r.status),
                r.anchored.path.display(),
                s,
                e
            );
        }
        println!();
    }
    Ok(())
}

fn render_porcelain(findings: &[(String, RangeResolved)]) {
    if findings.is_empty() {
        return;
    }
    println!("# porcelain v1");
    for (mesh, r) in findings {
        let (s, e) = extent_lines(r.anchored.extent);
        let anchor_short = r.anchor_sha.get(..8).unwrap_or(&r.anchor_sha);
        println!(
            "{}\t{}\t{}\t{}\t{}\t{}",
            status_str(&r.status),
            mesh,
            r.anchored.path.display(),
            s,
            e,
            anchor_short
        );
    }
}

fn render_json(meshes: &[MeshResolved], findings: &[(String, RangeResolved)]) -> Result<()> {
    use serde_json::json;
    let mesh_name = meshes.first().map(|m| m.name.clone()).unwrap_or_default();
    let mut ranges = Vec::new();
    for (_, r) in findings {
        let (s, e) = extent_lines(r.anchored.extent);
        let severity = match r.status {
            RangeStatus::Orphaned | RangeStatus::Changed => "error",
            RangeStatus::Moved => "warning",
            _ => "error",
        };
        ranges.push(json!({
            "severity": severity,
            "code": status_str(&r.status),
            "range": {
                "start": {"line": s.saturating_sub(1), "character": 0},
                "end": {"line": e.saturating_sub(1), "character": 0},
            },
            "message": status_str(&r.status),
        }));
    }
    let v = json!({
        "version": 1,
        "mesh": mesh_name,
        "ranges": ranges,
    });
    println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default());
    Ok(())
}

fn render_junit(findings: &[(String, RangeResolved)]) {
    println!(
        "<testsuite name=\"git-mesh\" tests=\"{}\" failures=\"{}\">",
        findings.len(),
        findings.len()
    );
    for (mesh, r) in findings {
        let (s, e) = extent_lines(r.anchored.extent);
        println!(
            "  <testcase classname=\"{}\" name=\"{}#L{}-L{}\"><failure message=\"{}\"/></testcase>",
            mesh,
            r.anchored.path.display(),
            s,
            e,
            status_str(&r.status)
        );
    }
    println!("</testsuite>");
}

fn render_github(findings: &[(String, RangeResolved)]) {
    for (_, r) in findings {
        let (s, _e) = extent_lines(r.anchored.extent);
        let level = match r.status {
            RangeStatus::Moved => "warning",
            _ => "error",
        };
        println!(
            "::{level} file={},line={}::{}",
            r.anchored.path.display(),
            s,
            status_str(&r.status)
        );
    }
}

// Kept for `cli/show.rs`.
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

// Silences unused-import warnings while UnavailableReason is unused.
const _: Option<UnavailableReason> = None;

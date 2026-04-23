//! `git mesh stale` output rendering — §10.4.
//!
//! Porcelain format (stable one-line-per-finding schema, `# porcelain v1`
//! header):
//!
//! ```text
//! # porcelain v1
//! <status>\t<mesh>\t<path>\t<start>\t<end>\t<anchor-short>
//! ```
//!
//! Fields are TAB-separated. `<status>` is one of `FRESH`, `MOVED`,
//! `CHANGED`, `ORPHANED` (FRESH is never emitted — only non-fresh
//! findings appear). `<anchor-short>` is the 8-character prefix of the
//! Range's anchor SHA.

use crate::cli::{StaleArgs, StaleFormat};
use crate::stale::{culprit_commit, resolve_mesh, stale_meshes};
use crate::types::{MeshResolved, RangeResolved, RangeStatus};
use anyhow::Result;
use serde_json::json;
use similar::{ChangeTag, TextDiff};

pub fn run_stale(repo: &gix::Repository, args: StaleArgs) -> Result<i32> {
    let meshes = match &args.name {
        Some(n) => vec![resolve_mesh(repo, n)?],
        None => stale_meshes(repo)?,
    };

    // Optional --since filter: only ranges whose anchor is in <since>..HEAD.
    let since_oids = if let Some(since) = &args.since {
        let wd = crate::git::work_dir(repo)?;
        let head = crate::git::git_stdout(wd, ["rev-parse", "HEAD"])?;
        let mut oids: std::collections::BTreeSet<String> = crate::git::git_stdout(
            wd,
            ["rev-list", &format!("{since}..{head}")],
        )
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect();
        let since_oid = crate::git::git_stdout(wd, ["rev-parse", since]).unwrap_or_default();
        if !since_oid.is_empty() {
            oids.insert(since_oid);
        }
        Some(oids)
    } else {
        None
    };

    // Flatten findings honoring --since.
    let mut findings: Vec<(String, RangeResolved)> = Vec::new();
    let mut total_ranges = 0usize;
    for m in &meshes {
        for r in &m.ranges {
            if let Some(since) = &since_oids
                && !since.contains(&r.anchor_sha)
            {
                continue;
            }
            total_ranges += 1;
            if r.status != RangeStatus::Fresh {
                findings.push((m.name.clone(), r.clone()));
            }
        }
    }
    let stale_count = findings.len();

    match args.format {
        StaleFormat::Human => render_human(
            repo,
            &meshes,
            &findings,
            total_ranges,
            args.oneline,
            args.stat,
            args.patch,
        )?,
        StaleFormat::Porcelain => render_porcelain(&findings),
        StaleFormat::Json => render_json(repo, &meshes, &findings)?,
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

fn render_human(
    repo: &gix::Repository,
    meshes: &[MeshResolved],
    findings: &[(String, RangeResolved)],
    total: usize,
    oneline: bool,
    stat: bool,
    _patch: bool,
) -> Result<()> {
    // Header: one mesh header per mesh (only the first mesh's header
    // when `<name>` was given; for workspace scans without a name, emit
    // per-mesh headers when there are findings for that mesh).
    // The current callers expect a single header, matching the spec
    // example which is per-mesh.
    for m in meshes {
        if !oneline {
            print_mesh_header(repo, m)?;
        }
        let mesh_findings: Vec<&(String, RangeResolved)> =
            findings.iter().filter(|(name, _)| name == &m.name).collect();
        let mesh_total = m.ranges.len();
        let mesh_stale = mesh_findings.len();

        if oneline {
            for (_mesh, r) in &mesh_findings {
                println!(
                    "{:<8}  {}#L{}-L{}",
                    status_str(r.status),
                    r.anchored.path,
                    r.anchored.start,
                    r.anchored.end
                );
            }
            continue;
        }

        println!("{mesh_stale} stale of {mesh_total} ranges:");
        println!();

        let orphaned: Vec<&RangeResolved> = mesh_findings
            .iter()
            .map(|(_, r)| r)
            .filter(|r| r.status == RangeStatus::Orphaned)
            .collect();
        let changed: Vec<&RangeResolved> = mesh_findings
            .iter()
            .map(|(_, r)| r)
            .filter(|r| r.status == RangeStatus::Changed)
            .collect();
        let moved: Vec<&RangeResolved> = mesh_findings
            .iter()
            .map(|(_, r)| r)
            .filter(|r| r.status == RangeStatus::Moved)
            .collect();

        if stat {
            // Compact counts panel: no per-range bodies.
            if !orphaned.is_empty() {
                println!("Orphaned ranges: {}", orphaned.len());
            }
            if !changed.is_empty() {
                println!("Changed ranges: {}", changed.len());
            }
            if !moved.is_empty() {
                println!("Moved ranges: {}", moved.len());
            }
            println!();
            continue;
        }

        if !orphaned.is_empty() {
            println!("Orphaned ranges:");
            println!();
            for r in &orphaned {
                println!("  {}#L{}-L{}", r.anchored.path, r.anchored.start, r.anchored.end);
                let short = r.anchor_sha.get(..8).unwrap_or(&r.anchor_sha);
                println!(
                    "  anchor {short} is unreachable — run `git fetch` or check for a force-push"
                );
                println!();
            }
        }

        if !changed.is_empty() {
            println!("Changed ranges:");
            println!();
            for r in &changed {
                println!("  {}#L{}-L{}", r.anchored.path, r.anchored.start, r.anchored.end);
                if let Some(c) = culprit_commit_info(repo, r)? {
                    println!("  caused by {} {}  ({})", c.short, c.subject, c.relative);
                }
                println!();
                // Flat diff body.
                print_changed_diff(repo, r)?;
                println!();
            }
        }

        if !moved.is_empty() {
            println!("Moved ranges:");
            println!();
            for r in &moved {
                if let Some(cur) = &r.current {
                    println!(
                        "  {}#L{}-L{} → {}#L{}-L{}",
                        r.anchored.path,
                        r.anchored.start,
                        r.anchored.end,
                        cur.path,
                        cur.start,
                        cur.end
                    );
                }
            }
            println!();
        }
    }
    let _ = total;
    Ok(())
}

fn print_mesh_header(repo: &gix::Repository, m: &MeshResolved) -> Result<()> {
    let info = crate::mesh::mesh_commit_info(repo, &m.name)?;
    println!("mesh {}", m.name);
    println!("commit {}", info.commit_oid);
    println!("Author: {} <{}>", info.author_name, info.author_email);
    println!("Date:   {}", info.author_date);
    println!();
    for line in m.message.lines() {
        println!("    {line}");
    }
    println!();
    Ok(())
}

struct CulpritInfo {
    short: String,
    subject: String,
    relative: String,
}

fn culprit_commit_info(
    repo: &gix::Repository,
    r: &RangeResolved,
) -> Result<Option<CulpritInfo>> {
    let Some(oid) = culprit_commit(repo, r)? else {
        return Ok(None);
    };
    let wd = crate::git::work_dir(repo)?;
    let short = crate::git::git_stdout(wd, ["rev-parse", "--short", &oid]).unwrap_or_else(|_| {
        oid.chars().take(8).collect::<String>()
    });
    let subject = crate::git::git_stdout(wd, ["show", "-s", "--format=%s", &oid]).unwrap_or_default();
    let relative =
        crate::git::git_stdout(wd, ["show", "-s", "--format=%cr", &oid]).unwrap_or_default();
    Ok(Some(CulpritInfo {
        short,
        subject,
        relative,
    }))
}

fn print_changed_diff(repo: &gix::Repository, r: &RangeResolved) -> Result<()> {
    let wd = crate::git::work_dir(repo)?;
    let anchored_text =
        crate::git::git_stdout(wd, ["cat-file", "-p", &r.anchored.blob]).unwrap_or_default();
    let anchored_lines: Vec<&str> = anchored_text.lines().collect();
    let a_lo = (r.anchored.start as usize).saturating_sub(1);
    let a_hi = (r.anchored.end as usize).min(anchored_lines.len());
    let a_slice: Vec<String> = anchored_lines[a_lo..a_hi].iter().map(|s| s.to_string()).collect();

    if let Some(cur) = &r.current {
        let current_text =
            crate::git::git_stdout(wd, ["cat-file", "-p", &cur.blob]).unwrap_or_default();
        let current_lines: Vec<&str> = current_text.lines().collect();
        let c_lo = (cur.start as usize).saturating_sub(1);
        let c_hi = (cur.end as usize).min(current_lines.len());
        let c_slice: Vec<String> = current_lines[c_lo..c_hi].iter().map(|s| s.to_string()).collect();
        println!(
            "--- {}#L{}-L{} (anchored)",
            r.anchored.path, r.anchored.start, r.anchored.end
        );
        println!(
            "+++ {}#L{}-L{} (HEAD)",
            cur.path, cur.start, cur.end
        );
        print_unified_hunk(&a_slice, &c_slice, r.anchored.start, cur.start);
    } else {
        // Deletion: diff against /dev/null.
        println!(
            "--- {}#L{}-L{} (anchored)",
            r.anchored.path, r.anchored.start, r.anchored.end
        );
        println!("+++ /dev/null");
        let n = a_slice.len();
        println!("@@ -{},{} +0,0 @@", r.anchored.start, n);
        for line in &a_slice {
            println!("-{line}");
        }
    }
    Ok(())
}

fn print_unified_hunk(a: &[String], b: &[String], a_start: u32, b_start: u32) {
    let a_refs: Vec<&str> = a.iter().map(String::as_str).collect();
    let b_refs: Vec<&str> = b.iter().map(String::as_str).collect();
    let diff = TextDiff::from_slices(&a_refs, &b_refs);
    println!(
        "@@ -{},{} +{},{} @@",
        a_start,
        a.len(),
        b_start,
        b.len()
    );
    for change in diff.iter_all_changes() {
        let prefix = match change.tag() {
            ChangeTag::Delete => "-",
            ChangeTag::Insert => "+",
            ChangeTag::Equal => " ",
        };
        // `change.value()` typically retains a trailing newline; trim one.
        let text = change.value();
        let trimmed = text.strip_suffix('\n').unwrap_or(text);
        println!("{prefix}{trimmed}");
    }
}

fn render_porcelain(findings: &[(String, RangeResolved)]) {
    if findings.is_empty() {
        return;
    }
    println!("# porcelain v1");
    for (mesh, r) in findings {
        let anchor_short = r.anchor_sha.get(..8).unwrap_or(&r.anchor_sha);
        println!(
            "{}\t{}\t{}\t{}\t{}\t{}",
            status_str(r.status),
            mesh,
            r.anchored.path,
            r.anchored.start,
            r.anchored.end,
            anchor_short
        );
    }
}

fn render_json(
    repo: &gix::Repository,
    meshes: &[MeshResolved],
    findings: &[(String, RangeResolved)],
) -> Result<()> {
    // Mesh + commit derive from the (first) mesh under inspection.
    let (mesh_name, commit_oid) = match meshes.first() {
        Some(m) => {
            let info = crate::mesh::mesh_commit_info(repo, &m.name).ok();
            (
                m.name.clone(),
                info.map(|i| i.commit_oid).unwrap_or_default(),
            )
        }
        None => (String::new(), String::new()),
    };
    let mut ranges: Vec<serde_json::Value> = Vec::new();
    for (_mesh, r) in findings {
        let severity = match r.status {
            RangeStatus::Orphaned | RangeStatus::Changed => "error",
            RangeStatus::Moved => "warning",
            RangeStatus::Fresh => continue,
        };
        let code = status_str(r.status);
        let (s, e) = (r.anchored.start.saturating_sub(1), r.anchored.end.saturating_sub(1));
        let data = if r.status == RangeStatus::Changed {
            let info = culprit_commit_info(repo, r)?;
            match info {
                Some(c) => json!({
                    "culprit": {
                        "sha": c.short,
                        "subject": c.subject,
                        "relative": c.relative,
                    }
                }),
                None => json!({}),
            }
        } else {
            json!({})
        };
        let message = match r.status {
            RangeStatus::Orphaned => format!(
                "anchor {} is unreachable",
                r.anchor_sha.get(..8).unwrap_or(&r.anchor_sha)
            ),
            RangeStatus::Changed => "range content changed since anchor".to_string(),
            RangeStatus::Moved => match &r.current {
                Some(cur) => format!(
                    "range moved to {}#L{}-L{}",
                    cur.path, cur.start, cur.end
                ),
                None => "range moved".to_string(),
            },
            RangeStatus::Fresh => String::new(),
        };
        ranges.push(json!({
            "severity": severity,
            "range": {
                "start": {"line": s, "character": 0},
                "end": {"line": e, "character": 0},
            },
            "message": message,
            "code": code,
            "data": data,
        }));
    }
    let v = json!({
        "version": 1,
        "mesh": mesh_name,
        "commit": commit_oid,
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
        println!(
            "  <testcase classname=\"{}\" name=\"{}#L{}-L{}\"><failure message=\"{}\"/></testcase>",
            mesh,
            r.anchored.path,
            r.anchored.start,
            r.anchored.end,
            status_str(r.status)
        );
    }
    println!("</testsuite>");
}

fn render_github(findings: &[(String, RangeResolved)]) {
    for (_mesh, r) in findings {
        let level = match r.status {
            RangeStatus::Orphaned | RangeStatus::Changed => "error",
            RangeStatus::Moved => "warning",
            RangeStatus::Fresh => continue,
        };
        let msg = match r.status {
            RangeStatus::Orphaned => format!(
                "anchor {} is unreachable",
                r.anchor_sha.get(..8).unwrap_or(&r.anchor_sha)
            ),
            RangeStatus::Changed => "range content changed since anchor".to_string(),
            RangeStatus::Moved => match &r.current {
                Some(cur) => format!(
                    "range moved to {}#L{}-L{}",
                    cur.path, cur.start, cur.end
                ),
                None => "range moved".to_string(),
            },
            RangeStatus::Fresh => continue,
        };
        println!(
            "::{level} file={},line={}::{msg}",
            r.anchored.path, r.anchored.start
        );
    }
}

fn status_str(s: RangeStatus) -> &'static str {
    match s {
        RangeStatus::Fresh => "FRESH",
        RangeStatus::Moved => "MOVED",
        RangeStatus::Changed => "CHANGED",
        RangeStatus::Orphaned => "ORPHANED",
    }
}

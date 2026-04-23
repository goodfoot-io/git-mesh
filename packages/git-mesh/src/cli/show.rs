//! `git mesh` list / `git mesh <name>` show / `git mesh ls` — §10.4, §3.4.

use crate::cli::{parse_range_address, LsArgs, ShowArgs};
use crate::range::read_range;
use crate::{
    list_mesh_names, ls_all, ls_by_path, ls_by_path_range, mesh_commit_info, mesh_commit_info_at,
    mesh_log, read_mesh, read_mesh_at,
};
use anyhow::Result;

pub fn run_list(repo: &gix::Repository) -> Result<i32> {
    let names = list_mesh_names(repo)?;
    if names.is_empty() {
        println!("no meshes");
        return Ok(0);
    }
    for name in names {
        let m = read_mesh(repo, &name)?;
        let summary = m.message.lines().next().unwrap_or_default();
        println!("{name}\t{} ranges\t{summary}", m.ranges.len());
    }
    Ok(0)
}

pub fn run_show(repo: &gix::Repository, args: ShowArgs) -> Result<i32> {
    if args.log {
        let entries = mesh_log(repo, &args.name, args.limit)?;
        for info in entries {
            if args.oneline {
                println!("{} {}", short(&info.commit_oid), info.summary);
            } else {
                println!("commit {}", info.commit_oid);
                println!("Author: {} <{}>", info.author_name, info.author_email);
                println!("Date:   {}", info.author_date);
                println!();
                for line in info.message.trim_end_matches('\n').lines() {
                    println!("    {line}");
                }
                println!();
            }
        }
        return Ok(0);
    }

    let mesh = read_mesh_at(repo, &args.name, args.at.as_deref())?;
    let info = mesh_commit_info_at(repo, &args.name, args.at.as_deref())?;

    // --format=<FMT> short-circuits the default rendering (§10.4).
    if let Some(fmt) = &args.format {
        let rendered = render_format(repo, &mesh, &info, fmt, args.no_abbrev)?;
        println!("{rendered}");
        return Ok(0);
    }

    if args.oneline {
        for id in &mesh.ranges {
            let r = read_range(repo, id)?;
            let sha = if args.no_abbrev { r.anchor_sha.clone() } else { short(&r.anchor_sha).into() };
            println!("{sha}  {}#L{}-L{}", r.path, r.start, r.end);
        }
        return Ok(0);
    }

    println!("mesh {}", mesh.name);
    println!("commit {}", info.commit_oid);
    println!("Author: {} <{}>", info.author_name, info.author_email);
    println!("Date:   {}", info.author_date);
    println!();
    for line in mesh.message.trim_end_matches('\n').lines() {
        println!("    {line}");
    }
    println!();
    println!("Ranges ({}):", mesh.ranges.len());
    for id in &mesh.ranges {
        let r = read_range(repo, id)?;
        let sha = if args.no_abbrev { r.anchor_sha.clone() } else { short(&r.anchor_sha).into() };
        println!("    {sha}  {}#L{}-L{}", r.path, r.start, r.end);
    }

    // Consume unused field warning via bind.
    let _ = mesh_commit_info(repo, &args.name);
    Ok(0)
}

/// Substitute mesh-specific `%(…)` placeholders, then shell out to
/// `git show -s --format=<residual>` to evaluate any remaining standard
/// commit placeholders (`%H`, `%s`, …) against the mesh tip commit.
fn render_format(
    repo: &gix::Repository,
    mesh: &crate::types::Mesh,
    info: &crate::MeshCommitInfo,
    fmt: &str,
    no_abbrev: bool,
) -> anyhow::Result<String> {
    let substituted = substitute_mesh_placeholders(repo, mesh, fmt, no_abbrev)?;
    // If there's any `%` left, delegate to git; else return as-is.
    if substituted.contains('%') {
        let wd = crate::git::work_dir(repo)
            .map_err(|e| anyhow::anyhow!("work dir: {e}"))?;
        let rendered = crate::git::git_stdout_raw(
            wd,
            [
                "show",
                "-s",
                &format!("--format={substituted}"),
                &info.commit_oid,
            ],
        )
        .map_err(|e| anyhow::anyhow!("git show: {e}"))?;
        // `git show` appends a trailing newline that `println!` will
        // double; strip it here.
        Ok(rendered.trim_end_matches('\n').to_string())
    } else {
        Ok(substituted)
    }
}

/// Replace every `%(ranges)`, `%(ranges:count)`, `%(config:<key>)`, and
/// any unknown `%(…)` tokens. Standard `%X` placeholders are left alone
/// for `git show --format=` to handle.
fn substitute_mesh_placeholders(
    repo: &gix::Repository,
    mesh: &crate::types::Mesh,
    fmt: &str,
    no_abbrev: bool,
) -> anyhow::Result<String> {
    let mut out = String::with_capacity(fmt.len());
    let mut chars = fmt.char_indices().peekable();
    while let Some((_, c)) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        // Look ahead for `(`; anything else is an ordinary %X token.
        match chars.peek().map(|(_, nc)| *nc) {
            Some('(') => {
                chars.next(); // consume '('
                let mut token = String::new();
                let mut closed = false;
                for (_, nc) in chars.by_ref() {
                    if nc == ')' {
                        closed = true;
                        break;
                    }
                    token.push(nc);
                }
                if !closed {
                    // Malformed — leave literal.
                    out.push('%');
                    out.push('(');
                    out.push_str(&token);
                    continue;
                }
                out.push_str(&evaluate_mesh_token(repo, mesh, &token, no_abbrev));
            }
            _ => {
                // Pass through — git will render it.
                out.push('%');
            }
        }
    }
    Ok(out)
}

fn evaluate_mesh_token(
    repo: &gix::Repository,
    mesh: &crate::types::Mesh,
    token: &str,
    no_abbrev: bool,
) -> String {
    match token {
        "ranges" => {
            let mut lines = Vec::with_capacity(mesh.ranges.len());
            for id in &mesh.ranges {
                match read_range(repo, id) {
                    Ok(r) => {
                        let sha = if no_abbrev {
                            r.anchor_sha.clone()
                        } else {
                            short(&r.anchor_sha).to_string()
                        };
                        lines.push(format!("{sha}  {}#L{}-L{}", r.path, r.start, r.end));
                    }
                    Err(_) => lines.push(format!("<missing>  <{id}>")),
                }
            }
            lines.join("\n")
        }
        "ranges:count" => mesh.ranges.len().to_string(),
        t if t.starts_with("config:") => {
            let key = &t["config:".len()..];
            match key {
                "copy-detection" => match mesh.config.copy_detection {
                    crate::types::CopyDetection::Off => "off".into(),
                    crate::types::CopyDetection::SameCommit => "same-commit".into(),
                    crate::types::CopyDetection::AnyFileInCommit => "any-file-in-commit".into(),
                    crate::types::CopyDetection::AnyFileInRepo => "any-file-in-repo".into(),
                },
                "ignore-whitespace" => mesh.config.ignore_whitespace.to_string(),
                // Unknown config key — documented choice: empty string.
                _ => String::new(),
            }
        }
        // Unknown mesh placeholder — leave literal.
        other => format!("%({other})"),
    }
}

pub fn run_ls(repo: &gix::Repository, args: LsArgs) -> Result<i32> {
    let entries = match args.target {
        None => ls_all(repo)?,
        Some(t) => {
            if t.contains("#L") {
                let (path, s, e) = parse_range_address(&t)?;
                ls_by_path_range(repo, &path, s, e)?
            } else {
                ls_by_path(repo, &t)?
            }
        }
    };
    for e in entries {
        println!(
            "{}\t{}\t{}\t{}\t{}-{}",
            e.path, e.mesh_name, e.range_id, e.anchor_short, e.start, e.end
        );
    }
    Ok(0)
}

fn short(sha: &str) -> &str {
    &sha[..sha.len().min(8)]
}

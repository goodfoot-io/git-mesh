//! Staging + commit handlers — §6.2, §6.3, §6.4, §10.5.

use crate::cli::{parse_range_address, AddArgs, CommitArgs, ConfigArgs, MessageArgs, RmArgs, StatusArgs};
use crate::staging::StagedConfig;
use crate::types::CopyDetection;
use crate::{
    append_add, append_config, append_remove, commit_mesh, read_mesh, set_message, status_view,
};
use anyhow::{anyhow, Context, Result};

pub fn run_add(repo: &gix::Repository, args: AddArgs) -> Result<i32> {
    crate::validation::validate_mesh_name(&args.name)?;
    for addr in &args.ranges {
        let (path, s, e) = parse_range_address(addr)?;
        append_add(repo, &args.name, &path, s, e, args.at.as_deref())?;
    }
    Ok(0)
}

pub fn run_rm(repo: &gix::Repository, args: RmArgs) -> Result<i32> {
    for addr in &args.ranges {
        let (path, s, e) = parse_range_address(addr)?;
        append_remove(repo, &args.name, &path, s, e)?;
    }
    Ok(0)
}

pub fn run_message(repo: &gix::Repository, args: MessageArgs) -> Result<i32> {
    // Per §10.2, bare `git mesh message <name>` (no flag) behaves like
    // `--edit`. `-m` / `-F` short-circuit the editor path.
    if let Some(m) = args.m {
        set_message(repo, &args.name, &m)?;
        return Ok(0);
    }
    if let Some(f) = args.file {
        let body = std::fs::read_to_string(&f).with_context(|| format!("failed to read {f}"))?;
        set_message(repo, &args.name, &body)?;
        return Ok(0);
    }
    // Editor flow (--edit or bare).
    run_message_editor(repo, &args.name)
}

fn run_message_editor(repo: &gix::Repository, name: &str) -> Result<i32> {
    crate::validation::validate_mesh_name(name)?;
    let wd = crate::git::work_dir(repo)?;
    let staging_dir = wd.join(".git").join("mesh").join("staging");
    std::fs::create_dir_all(&staging_dir)?;

    // Determine template content (§6.3):
    //   1. existing `<name>.msg` wins
    //   2. else parent mesh commit's message
    //   3. else blank buffer with a commented hint
    let msg_path = staging_dir.join(format!("{name}.msg"));
    let template: String = if msg_path.exists() {
        std::fs::read_to_string(&msg_path)?
    } else if let Ok(info) = crate::mesh::mesh_commit_info(repo, name) {
        info.message
    } else {
        String::from("\n# Write the relationship description. Empty message aborts.\n")
    };

    let edit_path = staging_dir.join(format!("{name}.msg.EDITMSG"));
    std::fs::write(&edit_path, &template)?;

    // Resolve editor — same lookup as `git commit`.
    let editor = std::env::var("GIT_EDITOR")
        .ok()
        .or_else(|| std::env::var("VISUAL").ok())
        .or_else(|| std::env::var("EDITOR").ok())
        .unwrap_or_else(|| "vi".to_string());

    // `git commit` spawns the editor via the shell for tokenization.
    let status = std::process::Command::new("sh")
        .arg("-c")
        .arg(format!("{editor} \"$@\"", editor = editor))
        .arg(&editor)
        .arg(&edit_path)
        .status()
        .with_context(|| format!("failed to spawn editor `{editor}`"))?;
    if !status.success() {
        return Err(anyhow!("editor `{editor}` exited with {status}"));
    }

    // Read + strip comment lines + trim trailing whitespace (git behavior).
    let raw = std::fs::read_to_string(&edit_path)?;
    let stripped = raw
        .lines()
        .filter(|l| !l.starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n");
    let body = stripped.trim_end().to_string();

    // Clean up the EDITMSG scratch file regardless of outcome.
    let _ = std::fs::remove_file(&edit_path);

    if body.is_empty() {
        return Err(anyhow!("aborting mesh message due to empty message"));
    }
    set_message(repo, name, &body)?;
    Ok(0)
}

pub fn run_commit(repo: &gix::Repository, args: CommitArgs) -> Result<i32> {
    let name = args
        .name
        .ok_or_else(|| anyhow!("`git mesh commit <name>` requires a name"))?;
    commit_mesh(repo, &name)?;
    println!("updated refs/meshes/v1/{name}");
    Ok(0)
}

pub fn run_status(repo: &gix::Repository, args: StatusArgs) -> Result<i32> {
    if args.check {
        // Walk every mesh with non-empty staging (including meshes not
        // yet committed).
        let wd = crate::git::work_dir(repo)?;
        let dir = wd.join(".git").join("mesh").join("staging");
        let mut candidates = std::collections::BTreeSet::new();
        for n in crate::list_mesh_names(repo).unwrap_or_default() {
            candidates.insert(n);
        }
        if dir.exists() {
            for entry in std::fs::read_dir(&dir)? {
                let entry = entry?;
                let fname = entry.file_name();
                let fn_str = fname.to_string_lossy();
                if !fn_str.contains('.') {
                    candidates.insert(fn_str.into_owned());
                }
            }
        }
        let mut drifted = false;
        for c in candidates {
            let sv = status_view(repo, &c)?;
            if !sv.drift.is_empty() {
                drifted = true;
                // Print the drift diffs for each affected range.
                println!("Working tree drift:");
                println!();
                for f in &sv.drift {
                    println!("  {}#L{}-L{}", f.path, f.start, f.end);
                }
                println!();
                for f in &sv.drift {
                    print_drift_diff(repo, &c, f)?;
                }
            }
        }
        return Ok(if drifted { 1 } else { 0 });
    }
    let name = args
        .name
        .ok_or_else(|| anyhow!("`git mesh status <name>` requires a name (or --check)"))?;
    let sv = status_view(repo, &name)?;

    // Header: `mesh <name>` + commit/author/date/message, matching
    // `git show` conventions. Skip cleanly if the mesh has no tip yet.
    if let Ok(info) = crate::mesh::mesh_commit_info(repo, &name) {
        println!("mesh {}", sv.name);
        println!("commit {}", info.commit_oid);
        println!("Author: {} <{}>", info.author_name, info.author_email);
        println!("Date:   {}", info.author_date);
        println!();
        for line in info.message.lines() {
            println!("    {line}");
        }
        println!();
    } else {
        println!("mesh {}", sv.name);
        println!();
    }

    let has_staged = !sv.staging.adds.is_empty()
        || !sv.staging.removes.is_empty()
        || !sv.staging.configs.is_empty();
    if has_staged {
        println!("Staged changes:");
        println!();
        for a in &sv.staging.adds {
            println!("  add     {}#L{}-L{}", a.path, a.start, a.end);
        }
        for r in &sv.staging.removes {
            println!("  remove  {}#L{}-L{}", r.path, r.start, r.end);
        }
        for c in &sv.staging.configs {
            match c {
                StagedConfig::CopyDetection(cd) => {
                    println!(
                        "  config  copy-detection {}",
                        crate::staging::serialize_copy_detection(*cd)
                    );
                }
                StagedConfig::IgnoreWhitespace(b) => {
                    println!("  config  ignore-whitespace {b}");
                }
            }
        }
        println!();
    }

    if let Some(msg) = &sv.staging.message {
        println!("Staged message:");
        println!();
        for line in msg.lines() {
            println!("  {line}");
        }
        println!();
    }

    if !sv.drift.is_empty() {
        println!("Working tree drift:");
        println!();
        for f in &sv.drift {
            println!("  {}#L{}-L{}", f.path, f.start, f.end);
        }
        println!();
        for f in &sv.drift {
            print_drift_diff(repo, &name, f)?;
        }
    }
    Ok(0)
}

fn print_drift_diff(
    repo: &gix::Repository,
    name: &str,
    f: &crate::staging::DriftFinding,
) -> Result<()> {
    use similar::{ChangeTag, TextDiff};
    // Load sidecar bytes for the staged add at `(path, start, end)`.
    let staging = crate::staging::read_staging(repo, name)?;
    let add = staging
        .adds
        .iter()
        .find(|a| a.path == f.path && a.start == f.start && a.end == f.end);
    let Some(add) = add else {
        return Ok(());
    };
    let wd = crate::git::work_dir(repo)?;
    let sidecar_p = wd
        .join(".git")
        .join("mesh")
        .join("staging")
        .join(format!("{name}.{}", add.line_number));
    let sidecar = std::fs::read(&sidecar_p).unwrap_or_default();
    let current = std::fs::read(wd.join(&f.path)).unwrap_or_default();
    let sidecar_text = String::from_utf8_lossy(&sidecar).to_string();
    let current_text = String::from_utf8_lossy(&current).to_string();
    let sidecar_lines: Vec<&str> = sidecar_text.lines().collect();
    let current_lines: Vec<&str> = current_text.lines().collect();
    let s_lo = (f.start as usize).saturating_sub(1);
    let s_hi = (f.end as usize).min(sidecar_lines.len());
    let c_hi = (f.end as usize).min(current_lines.len());
    let a_slice: Vec<String> = if s_lo <= s_hi {
        sidecar_lines[s_lo..s_hi].iter().map(|s| s.to_string()).collect()
    } else {
        Vec::new()
    };
    let b_slice: Vec<String> = if s_lo <= c_hi {
        current_lines[s_lo..c_hi].iter().map(|s| s.to_string()).collect()
    } else {
        Vec::new()
    };
    println!("--- {}#L{}-L{} (staged)", f.path, f.start, f.end);
    println!("+++ {}#L{}-L{} (working tree)", f.path, f.start, f.end);
    let a_refs: Vec<&str> = a_slice.iter().map(String::as_str).collect();
    let b_refs: Vec<&str> = b_slice.iter().map(String::as_str).collect();
    let diff = TextDiff::from_slices(&a_refs, &b_refs);
    println!(
        "@@ -{},{} +{},{} @@",
        f.start,
        a_slice.len(),
        f.start,
        b_slice.len()
    );
    for change in diff.iter_all_changes() {
        let prefix = match change.tag() {
            ChangeTag::Delete => "-",
            ChangeTag::Insert => "+",
            ChangeTag::Equal => " ",
        };
        let text = change.value();
        let trimmed = text.strip_suffix('\n').unwrap_or(text);
        println!("{prefix}{trimmed}");
    }
    println!();
    Ok(())
}

pub fn run_config(repo: &gix::Repository, args: ConfigArgs) -> Result<i32> {
    // Read mesh config.
    let mesh = read_mesh(repo, &args.name)?;
    match (args.unset, args.key, args.value) {
        (Some(unset), _, _) => {
            // §10.5: stage a reset to the built-in default for <key>.
            // Defaults come from DEFAULT_COPY_DETECTION / DEFAULT_IGNORE_WHITESPACE.
            let entry = match unset.as_str() {
                "copy-detection" => StagedConfig::CopyDetection(crate::types::DEFAULT_COPY_DETECTION),
                "ignore-whitespace" => {
                    StagedConfig::IgnoreWhitespace(crate::types::DEFAULT_IGNORE_WHITESPACE)
                }
                other => return Err(anyhow!("unknown config key `{other}`")),
            };
            crate::staging::append_config(repo, &args.name, &entry)?;
            Ok(0)
        }
        (None, None, _) => {
            let staging = crate::staging::read_staging(repo, &args.name).unwrap_or_default();
            let (staged_cd, staged_iw) = crate::staging::resolve_staged_config(
                &staging,
                (mesh.config.copy_detection, mesh.config.ignore_whitespace),
            );
            let cd_changed = staged_cd != mesh.config.copy_detection;
            let iw_changed = staged_iw != mesh.config.ignore_whitespace;
            println!(
                "{}copy-detection {}{}",
                if cd_changed { "* " } else { "" },
                cd_str(staged_cd),
                if cd_changed { " (staged)" } else { "" }
            );
            println!(
                "{}ignore-whitespace {}{}",
                if iw_changed { "* " } else { "" },
                staged_iw,
                if iw_changed { " (staged)" } else { "" }
            );
            Ok(0)
        }
        (None, Some(key), None) => {
            match key.as_str() {
                "copy-detection" => println!("{}", cd_str(mesh.config.copy_detection)),
                "ignore-whitespace" => println!("{}", mesh.config.ignore_whitespace),
                other => return Err(anyhow!("unknown config key `{other}`")),
            }
            Ok(0)
        }
        (None, Some(key), Some(value)) => {
            let entry = match key.as_str() {
                "copy-detection" => StagedConfig::CopyDetection(match value.as_str() {
                    "off" => CopyDetection::Off,
                    "same-commit" => CopyDetection::SameCommit,
                    "any-file-in-commit" => CopyDetection::AnyFileInCommit,
                    "any-file-in-repo" => CopyDetection::AnyFileInRepo,
                    _ => return Err(anyhow!("invalid copy-detection value `{value}`")),
                }),
                "ignore-whitespace" => StagedConfig::IgnoreWhitespace(match value.as_str() {
                    "true" => true,
                    "false" => false,
                    _ => return Err(anyhow!("invalid ignore-whitespace value `{value}`")),
                }),
                other => return Err(anyhow!("unknown config key `{other}`")),
            };
            append_config(repo, &args.name, &entry)?;
            Ok(0)
        }
    }
}

fn cd_str(cd: CopyDetection) -> &'static str {
    match cd {
        CopyDetection::Off => "off",
        CopyDetection::SameCommit => "same-commit",
        CopyDetection::AnyFileInCommit => "any-file-in-commit",
        CopyDetection::AnyFileInRepo => "any-file-in-repo",
    }
}

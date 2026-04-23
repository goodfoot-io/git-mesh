//! Staging + commit handlers — §6.2, §6.3, §6.4, §10.5.

use crate::cli::{
    AddArgs, CommitArgs, ConfigArgs, MessageArgs, RmArgs, StatusArgs, parse_range_address,
};
use crate::staging::{StagedConfig, append_prepared_add, prepare_add};
use crate::types::{CopyDetection, RangeExtent};
use crate::{append_config, append_remove, commit_mesh, read_mesh, set_message, status_view};
use anyhow::{Context, Result, anyhow};

pub fn run_add(repo: &gix::Repository, args: AddArgs) -> Result<i32> {
    crate::validation::validate_mesh_name(&args.name)?;

    // Parse every address first; fail-closed with no partial staging.
    let mut parsed: Vec<(String, u32, u32)> = Vec::with_capacity(args.ranges.len());
    for addr in &args.ranges {
        parsed.push(parse_range_address(addr)?);
    }

    // Reject duplicate `(path, start, end)` within this invocation.
    for (i, a) in parsed.iter().enumerate() {
        for b in &parsed[..i] {
            if a == b {
                return Err(anyhow!(
                    "duplicate range location in mesh: {}:{}-{}",
                    a.0,
                    a.1,
                    a.2
                ));
            }
        }
    }

    // Reject against already-staged adds for this mesh (net of staged
    // removes — a pending remove of the same location means the add is
    // a valid re-anchor within one staging pass).
    let existing = crate::staging::read_staging(repo, &args.name).unwrap_or_default();
    let removed: std::collections::HashSet<(String, u32, u32)> = existing
        .removes
        .iter()
        .map(|r| (r.path.clone(), r.start, r.end))
        .collect();
    for a in &parsed {
        let is_staged_add = existing
            .adds
            .iter()
            .any(|s| s.path == a.0 && s.start == a.1 && s.end == a.2);
        if is_staged_add && !removed.contains(a) {
            return Err(anyhow!(
                "duplicate range location in mesh: {}:{}-{}",
                a.0,
                a.1,
                a.2
            ));
        }
    }

    let mut adds = Vec::with_capacity(parsed.len());
    for (path, s, e) in &parsed {
        adds.push(prepare_add(repo, path, *s, *e, args.at.as_deref())?);
    }
    for add in &adds {
        append_prepared_add(repo, &args.name, add)?;
    }
    Ok(0)
}

pub fn run_rm(repo: &gix::Repository, args: RmArgs) -> Result<i32> {
    crate::validation::validate_mesh_name(&args.name)?;

    // Parse all addresses first; fail-closed with no partial staging.
    let mut parsed: Vec<(String, u32, u32)> = Vec::with_capacity(args.ranges.len());
    for addr in &args.ranges {
        parsed.push(parse_range_address(addr)?);
    }

    // Compute effective mesh state = committed ranges + staged adds − staged removes.
    let mut present: Vec<(String, u32, u32)> = Vec::new();
    match read_mesh(repo, &args.name) {
        Ok(mesh) => {
            for id in &mesh.ranges {
                let r = crate::range::read_range(repo, id)?;
                let (start, end) = match r.extent {
                    RangeExtent::Lines { start, end } => (start, end),
                    RangeExtent::Whole => todo!("whole-file support lands in a later slice"),
                };
                present.push((r.path, start, end));
            }
        }
        Err(crate::Error::MeshNotFound(_)) => {
            // New mesh — only staged state counts.
        }
        Err(e) => return Err(e.into()),
    }
    let staging = crate::staging::read_staging(repo, &args.name).unwrap_or_default();
    for a in &staging.adds {
        present.push((a.path.clone(), a.start, a.end));
    }
    for r in &staging.removes {
        if let Some(idx) = present
            .iter()
            .position(|(p, s, e)| p == &r.path && *s == r.start && *e == r.end)
        {
            present.remove(idx);
        }
    }

    // Validate every requested remove against `present`, applying earlier
    // removes in this invocation so multi-range rm works.
    let mut effective = present.clone();
    for (path, s, e) in &parsed {
        let idx = effective
            .iter()
            .position(|(p, ss, ee)| p == path && ss == s && ee == e);
        match idx {
            Some(i) => {
                effective.remove(i);
            }
            None => {
                return Err(anyhow!(
                    "range not in mesh {}: {}#L{}-L{}",
                    args.name,
                    path,
                    s,
                    e
                ));
            }
        }
    }

    for (path, s, e) in &parsed {
        append_remove(repo, &args.name, path, *s, *e)?;
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
    if let Some(name) = args.name {
        commit_mesh(repo, &name)?;
        println!("updated refs/meshes/v1/{name}");
        return Ok(0);
    }

    // No mesh name given: commit every mesh that has a non-empty staging
    // area (post-commit hook path, §10.2). Collect candidate names from
    // both `.git/mesh/staging/` files and any mesh that still has an
    // existing staging entry. If any commit fails, report all failures
    // and exit non-zero. If none are staged, exit 0 with a clear message.
    let wd = crate::git::work_dir(repo)?;
    let dir = wd.join(".git").join("mesh").join("staging");
    let mut candidates: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    if dir.exists() {
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            let fname = entry.file_name();
            let fn_str = fname.to_string_lossy().into_owned();
            // Ops files have no extension; `.msg` and `.<N>` are sidecars.
            if !fn_str.contains('.') {
                candidates.insert(fn_str);
            }
        }
    }

    // Filter to meshes that actually have something staged (ops or message).
    let mut staged: Vec<String> = Vec::new();
    for name in candidates {
        let s = crate::staging::read_staging(repo, &name).unwrap_or_default();
        let has_anything = !s.adds.is_empty()
            || !s.removes.is_empty()
            || !s.configs.is_empty()
            || s.message.is_some();
        if has_anything {
            staged.push(name);
        }
    }

    if staged.is_empty() {
        println!("nothing staged");
        return Ok(0);
    }

    let mut failures: Vec<(String, String)> = Vec::new();
    let mut committed: Vec<String> = Vec::new();
    for name in &staged {
        match commit_mesh(repo, name) {
            Ok(_) => {
                println!("updated refs/meshes/v1/{name}");
                committed.push(name.clone());
            }
            Err(e) => {
                eprintln!("error: mesh `{name}`: {e}");
                failures.push((name.clone(), e.to_string()));
            }
        }
    }

    if failures.is_empty() {
        Ok(0)
    } else {
        Err(anyhow!(
            "{} of {} mesh(es) failed to commit",
            failures.len(),
            staged.len()
        ))
    }
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
        sidecar_lines[s_lo..s_hi]
            .iter()
            .map(|s| s.to_string())
            .collect()
    } else {
        Vec::new()
    };
    let b_slice: Vec<String> = if s_lo <= c_hi {
        current_lines[s_lo..c_hi]
            .iter()
            .map(|s| s.to_string())
            .collect()
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
                "copy-detection" => {
                    StagedConfig::CopyDetection(crate::types::DEFAULT_COPY_DETECTION)
                }
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

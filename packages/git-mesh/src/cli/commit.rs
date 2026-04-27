//! Staging + commit handlers — §6.2, §6.3, §6.4, §10.5.

use crate::cli::{AddArgs, CommitArgs, ConfigArgs, RmArgs, WhyArgs};
use crate::staging::{StagedConfig, append_prepared_add, parse_address, prepare_add};
use crate::types::{CopyDetection, EngineOptions, RangeExtent, validate_add_target};
use crate::{append_config, append_remove, commit_mesh, read_mesh, set_why};
use anyhow::{Context, Result, anyhow};

pub fn run_add(repo: &gix::Repository, args: AddArgs) -> Result<i32> {
    crate::validation::validate_mesh_name(&args.name)?;

    // Parse every address first; fail-closed with no partial staging.
    let mut parsed: Vec<(String, RangeExtent)> = Vec::with_capacity(args.ranges.len());
    for addr in &args.ranges {
        let p = parse_address(addr)
            .ok_or_else(|| anyhow!("invalid range `{addr}`; expected <path>[#L<start>-L<end>]"))?;
        parsed.push(p);
    }

    // Slice 3: last-write-wins. Within a single invocation, coalesce
    // duplicate `(path, extent)` adds silently — keep the last
    // occurrence, drop earlier ones. Cross-invocation supersede is
    // handled by `append_prepared_add` (which strips + renumbers).
    {
        let mut last_idx: std::collections::HashMap<(String, RangeExtent), usize> =
            std::collections::HashMap::new();
        for (i, a) in parsed.iter().enumerate() {
            last_idx.insert(a.clone(), i);
        }
        let coalesced: Vec<(String, RangeExtent)> = parsed
            .iter()
            .enumerate()
            .filter(|(i, a)| last_idx.get(*a) == Some(i))
            .map(|(_, a)| a.clone())
            .collect();
        parsed = coalesced;
    }

    // Slice 6e: resolve `--at <commit-ish>` to a full OID up front so
    // the stage-time precheck and the prepare_add path operate on the
    // same anchor (independent of clap arg ordering).
    let anchor_oid: Option<String> = match args.at.as_deref() {
        Some(s) => Some(
            crate::git::resolve_commit(repo, s).map_err(|e| anyhow!("--at `{s}`: {e}"))?,
        ),
        None => None,
    };

    // Stage-time precheck (plan §"CLI and `git mesh add` prechecks").
    for (path, extent) in &parsed {
        validate_add_target(repo, std::path::Path::new(path), extent)
            .map_err(|err| anyhow!("{err}"))?;
    }

    // Resolve the existing range_id for this `(path, extent)` in the
    // mesh. Include resolved current locations so `git mesh add` over a
    // moved range's new address re-anchors the existing range instead of
    // creating a second range.
    let mut mesh_ranges_lookup = mesh_current_range_id_lookup(repo, &args.name);
    mesh_ranges_lookup.extend(mesh_range_id_lookup(repo, &args.name));

    let mut prepared = Vec::with_capacity(parsed.len());
    for (path, extent) in &parsed {
        prepared.push(prepare_add(repo, path, *extent, anchor_oid.as_deref())?);
    }
    for (add, (path, extent)) in prepared.iter().zip(parsed.iter()) {
        let range_id = mesh_ranges_lookup.get(&(path.clone(), *extent)).cloned();
        append_prepared_add(repo, &args.name, add, range_id)?;
    }
    Ok(0)
}

fn mesh_range_id_lookup(
    repo: &gix::Repository,
    mesh_name: &str,
) -> std::collections::HashMap<(String, RangeExtent), String> {
    let mut out = std::collections::HashMap::new();
    let Ok(mesh) = read_mesh(repo, mesh_name) else {
        return out;
    };
    for id in &mesh.ranges {
        if let Ok(r) = crate::range::read_range(repo, id) {
            out.insert((r.path, r.extent), id.clone());
        }
    }
    out
}

fn mesh_current_range_id_lookup(
    repo: &gix::Repository,
    mesh_name: &str,
) -> std::collections::HashMap<(String, RangeExtent), String> {
    let mut out = std::collections::HashMap::new();
    let Ok(resolved) = crate::resolver::resolve_mesh(repo, mesh_name, EngineOptions::full()) else {
        return out;
    };
    for r in resolved.ranges {
        let Some(current) = r.current else { continue };
        out.insert(
            (current.path.to_string_lossy().into_owned(), current.extent),
            r.range_id,
        );
    }
    out
}

pub fn run_rm(repo: &gix::Repository, args: RmArgs) -> Result<i32> {
    crate::validation::validate_mesh_name(&args.name)?;

    let mut parsed: Vec<(String, RangeExtent)> = Vec::with_capacity(args.ranges.len());
    for addr in &args.ranges {
        let p = parse_address(addr)
            .ok_or_else(|| anyhow!("invalid range `{addr}`; expected <path>[#L<start>-L<end>]"))?;
        parsed.push(p);
    }

    let mut present: Vec<(String, RangeExtent)> = Vec::new();
    match read_mesh(repo, &args.name) {
        Ok(mesh) => {
            for id in &mesh.ranges {
                let r = crate::range::read_range(repo, id)?;
                present.push((r.path, r.extent));
            }
        }
        Err(crate::Error::MeshNotFound(_)) => {}
        Err(e) => return Err(e.into()),
    }
    let staging = crate::staging::read_staging(repo, &args.name).unwrap_or_default();
    for a in &staging.adds {
        present.push((a.path.clone(), a.extent));
    }
    for r in &staging.removes {
        if let Some(idx) = present
            .iter()
            .position(|(p, e)| p == &r.path && *e == r.extent)
        {
            present.remove(idx);
        }
    }

    let mut effective = present.clone();
    for (path, extent) in &parsed {
        let idx = effective.iter().position(|(p, e)| p == path && e == extent);
        match idx {
            Some(i) => {
                effective.remove(i);
            }
            None => {
                let addr = match extent {
                    RangeExtent::Lines { start, end } => {
                        format!("{path}#L{start}-L{end}")
                    }
                    RangeExtent::Whole => path.clone(),
                };
                return Err(anyhow!("range not in mesh {}: {addr}", args.name));
            }
        }
    }

    for (path, extent) in &parsed {
        match extent {
            RangeExtent::Lines { start, end } => {
                append_remove(repo, &args.name, path, *start, *end)?;
            }
            RangeExtent::Whole => {
                crate::staging::append_remove_whole(repo, &args.name, path)?;
            }
        }
    }
    Ok(0)
}

pub fn run_why(repo: &gix::Repository, args: WhyArgs) -> Result<i32> {
    // Reader vs. writer disambiguation per `docs/why-plan.md` §B2:
    // any of `-m`/`-F`/`--edit` ⇒ writer; otherwise reader (which
    // optionally accepts `--at <commit>` for historical reads).
    let writer = args.m.is_some() || args.file.is_some() || args.edit;
    if !writer {
        return run_why_reader(repo, &args.name, args.at.as_deref());
    }
    if let Some(m) = args.m {
        set_why(repo, &args.name, &m)?;
        return Ok(0);
    }
    if let Some(f) = args.file {
        let body = std::fs::read_to_string(&f).with_context(|| format!("failed to read {f}"))?;
        set_why(repo, &args.name, &body)?;
        return Ok(0);
    }
    // Editor flow (--edit).
    run_why_editor(repo, &args.name)
}

fn run_why_reader(repo: &gix::Repository, name: &str, at: Option<&str>) -> Result<i32> {
    crate::validation::validate_mesh_name(name)?;
    // Resolution path: read the mesh commit at HEAD (or `--at <commit-ish>`)
    // on `refs/meshes/v1/<name>` and print its commit message — the
    // git-layer carrier of the mesh-layer why text.
    let info = crate::mesh::mesh_commit_info_at(repo, name, at)?;
    let body = info.message.trim_end_matches('\n');
    if body.is_empty() {
        println!();
    } else {
        println!("{body}");
    }
    Ok(0)
}

fn run_why_editor(repo: &gix::Repository, name: &str) -> Result<i32> {
    crate::validation::validate_mesh_name(name)?;
    let staging_dir = crate::git::mesh_dir(repo).join("staging");
    std::fs::create_dir_all(&staging_dir)?;

    // Determine template content (§6.3):
    //   1. existing `<name>.why` wins
    //   2. else parent mesh commit's message (the prior why)
    //   3. else blank buffer with a commented hint
    let encoded = crate::staging::encode_name_for_fs(name);
    let why_path = staging_dir.join(format!("{encoded}.why"));
    let template: String = if why_path.exists() {
        std::fs::read_to_string(&why_path)?
    } else if let Ok(info) = crate::mesh::mesh_commit_info(repo, name) {
        info.message
    } else {
        String::from("\n# Write the relationship description. Empty why aborts.\n")
    };

    let edit_path = staging_dir.join(format!("{encoded}.why.EDITMSG"));
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
        return Err(anyhow!("aborting mesh why due to empty body"));
    }
    set_why(repo, name, &body)?;
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
    let dir = crate::git::mesh_dir(repo).join("staging");
    let mut candidates: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    if dir.exists() {
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            let fname = entry.file_name();
            let fn_str = fname.to_string_lossy().into_owned();
            // Ops files have no extension; `.why` and `.<N>` are sidecars.
            if !fn_str.contains('.') {
                candidates.insert(crate::staging::decode_name_from_fs(&fn_str));
            }
        }
    }

    // Filter to meshes that actually have something staged (ops or why).
    let mut staged: Vec<String> = Vec::new();
    for name in candidates {
        let s = crate::staging::read_staging(repo, &name).unwrap_or_default();
        let has_anything =
            !s.adds.is_empty() || !s.removes.is_empty() || !s.configs.is_empty() || s.why.is_some();
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

//! Structural handlers (restore, revert, delete, move) + doctor — §6.6, §6.7, §6.8.

use crate::cli::{CliError, DeleteArgs, MoveArgs, NextStep, RestoreArgs, RevertArgs};
use crate::cli::format::{DESTRUCTIVE_TAG, IDEMPOTENT_TAG, format_fast_forward, format_ref_deletion};
use crate::sync::default_remote;
use crate::{delete_mesh, file_index, list_mesh_names, rename_mesh, revert_mesh, Error};
use anyhow::{Result, anyhow};
use std::collections::BTreeSet;
use std::fs;

pub fn run_restore(repo: &gix::Repository, args: RestoreArgs) -> Result<i32> {
    let staging = crate::staging::read_staging(repo, &args.name)?;
    let count = staging.adds.len() + staging.removes.len() + staging.configs.len()
        + staging.why.as_ref().map_or(0, |_| 1);
    if count == 0 {
        println!("`{}` has no staged operations.", args.name);
    } else {
        crate::staging::clear_staging(repo, &args.name)?;
        println!(
            "Cleared the staging area on `{}`. {} staged operations were discarded.{}",
            args.name, count, DESTRUCTIVE_TAG
        );
        println!();
        println!("Run `git mesh {}` to verify staging is empty.", args.name);
    }
    Ok(0)
}

pub fn run_revert(repo: &gix::Repository, args: RevertArgs) -> Result<i32> {
    let ref_name = format!("refs/meshes/v1/{}", args.name);
    let wd = crate::git::work_dir(repo)?;
    let current = crate::git::resolve_ref_oid_optional(wd, &ref_name)?
        .ok_or_else(|| Error::MeshNotFound(args.name.clone()))?;

    // Check if the target commit-ish is an ancestor of the current tip.
    let is_ancestor = crate::mesh::is_ancestor_commit(repo, &args.name, &args.commit_ish)
        .map_err(|e| anyhow!("{}", e))?;

    if !is_ancestor {
        let short_current: String = current.chars().take(7).collect();
        return Err(CliError {
            subcommand: "revert",
            summary: format!("cannot fast-forward `{}` to `{}`.", args.name, args.commit_ish),
            what_happened: format!(
                "`{}` is not an ancestor of the current mesh tip `{}`. \
                 `git mesh revert` only moves a mesh ref backwards along its own history.",
                args.commit_ish, short_current
            ),
            next_steps: vec![
                NextStep::Prose("List the mesh's history, then pick a commit on it:".into()),
                NextStep::Bash(format!(
                    "git mesh {} --log\ngit mesh revert {} <commit-on-the-log>",
                    args.name, args.name
                )),
            ],
        }
        .into());
    }

    let new_commit = revert_mesh(repo, &args.name, &args.commit_ish)?;
    let short_old: String = current.chars().take(7).collect();
    let short_new: String = new_commit.chars().take(7).collect();
    println!(
        "{}{}",
        format_fast_forward(&ref_name, &short_old, &short_new),
        DESTRUCTIVE_TAG
    );
    println!();
    println!("Run `git mesh {}` to verify.", args.name);
    Ok(0)
}

pub fn run_delete(repo: &gix::Repository, args: DeleteArgs) -> Result<i32> {
    let ref_name = format!("refs/meshes/v1/{}", args.name);
    let wd = crate::git::work_dir(repo)?;
    let current = match crate::git::resolve_ref_oid_optional(wd, &ref_name)? {
        Some(s) => s,
        None => {
            return Err(CliError {
                subcommand: "delete",
                summary: format!("no mesh named `{}`.", args.name),
                what_happened: format!("`{}` does not exist.", ref_name),
                next_steps: vec![NextStep::Bash("git mesh list".into())],
            }
            .into());
        }
    };

    delete_mesh(repo, &args.name)?;
    let short: String = current.chars().take(7).collect();
    println!(
        "{}{}",
        format_ref_deletion(&ref_name, &short),
        DESTRUCTIVE_TAG
    );
    println!();
    println!("Run `git mesh list` to confirm the mesh is gone.");
    Ok(0)
}

pub fn run_move(repo: &gix::Repository, args: MoveArgs) -> Result<i32> {
    let old_ref = format!("refs/meshes/v1/{}", args.old);
    let new_ref = format!("refs/meshes/v1/{}", args.new);

    // Check destination existence before calling rename_mesh so we can
    // produce a structured CliError.
    let wd = crate::git::work_dir(repo)?;
    if crate::git::resolve_ref_oid_optional(wd, &new_ref)?.is_some() {
        return Err(CliError {
            subcommand: "move",
            summary: format!("`{}` already exists.", args.new),
            what_happened: format!(
                "A mesh is already tracked at `{}`, so renaming `{}` over it would lose history.",
                new_ref, args.old
            ),
            next_steps: vec![
                NextStep::Prose("Pick a different name, or delete the existing mesh first:".into()),
                NextStep::Bash(format!(
                    "git mesh move {} <different-name>\ngit mesh delete {} && git mesh move {} {}",
                    args.old, args.new, args.old, args.new
                )),
            ],
        }
        .into());
    }

    // Check source existence early for a structured error.
    if crate::git::resolve_ref_oid_optional(wd, &old_ref)?.is_none() {
        return Err(CliError {
            subcommand: "move",
            summary: format!("no mesh named `{}`.", args.old),
            what_happened: format!("`{}` does not exist.", old_ref),
            next_steps: vec![NextStep::Bash("git mesh list".into())],
        }
        .into());
    }

    rename_mesh(repo, &args.old, &args.new)?;
    println!(
        "Renamed `{}` to `{}`. The mesh's history is preserved on `{}`.{}",
        args.old, args.new, new_ref, IDEMPOTENT_TAG
    );
    println!();
    println!("Run `git mesh {}` to verify.", args.new);
    Ok(0)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Severity {
    Info,
    Warn,
    Error,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DoctorFinding {
    pub code: DoctorCode,
    pub severity: Severity,
    pub message: String,
    pub remediation: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DoctorCode {
    MissingPostCommitHook,
    StagingCorrupt,
    RefspecMissing,
    OrphanRangeRef,
    FileIndexMissing,
    FileIndexRebuilt,
    DanglingRangeRef,
    SidecarTampered,
    /// Slice 6c: pre-existing duplicate mesh refspecs in
    /// `remote.<name>.{fetch,push}`. Doctor collapses them in-place and
    /// reports an INFO finding when it does.
    DuplicateRefspec,
    /// Slice 6d: `core.logAllRefUpdates` is not set to `always` (or is
    /// unset entirely), so refs under `refs/meshes/*` would not get
    /// reflog entries. Doctor sets it lazily and reports INFO.
    LogAllRefUpdatesSet,
    /// The `post-rewrite` hook is not installed or does not contain the
    /// `git mesh hooks git post-rewrite` marker.
    MissingPostRewriteHook,
    /// Doctor could not verify whether the hook installs the git-mesh
    /// marker because the hook script references shell variables or other
    /// constructs that cannot be resolved statically.
    CouldNotVerifyHook {
        hook: String,
        unresolvable_refs: Vec<String>,
    },
}

const POST_COMMIT_HOOK_BODY: &str = "#!/bin/sh\ngit mesh hooks git post-commit\n";
const POST_REWRITE_HOOK_BODY: &str = "#!/bin/sh\ngit mesh hooks git post-rewrite\n";
const POST_COMMIT_MARKER: &str = "git mesh hooks git post-commit";
const POST_REWRITE_MARKER: &str = "git mesh hooks git post-rewrite";

pub fn doctor_run(repo: &gix::Repository) -> crate::Result<Vec<DoctorFinding>> {
    let mut out = Vec::new();

    // ---- Hook checks --------------------------------------------------
    check_hook(
        repo,
        "post-commit",
        POST_COMMIT_MARKER,
        POST_COMMIT_HOOK_BODY,
        DoctorCode::MissingPostCommitHook,
        &mut out,
    );
    check_hook(
        repo,
        "post-rewrite",
        POST_REWRITE_MARKER,
        POST_REWRITE_HOOK_BODY,
        DoctorCode::MissingPostRewriteHook,
        &mut out,
    );

    // ---- Refspec check -----------------------------------------------
    let remote = default_remote(repo).unwrap_or_else(|_| "origin".into());
    let url = crate::sync::get_remote_url(repo, &remote);
    if url.is_some() {
        let fetch = crate::sync::get_remote_multi(repo, &remote, "fetch");
        if !fetch.iter().any(|l| l.contains("refs/meshes/")) {
            out.push(DoctorFinding {
                code: DoctorCode::RefspecMissing,
                severity: Severity::Info,
                message: format!("remote `{remote}` has no mesh refspec"),
                remediation: Some("run `git mesh push` or `fetch` once to bootstrap".into()),
            });
        }
        // Slice 6c: collapse duplicate mesh refspecs in place.
        if let Ok((fd, pd)) = crate::sync::dedupe_mesh_refspecs(repo, &remote)
            && (fd > 0 || pd > 0)
        {
            out.push(DoctorFinding {
                code: DoctorCode::DuplicateRefspec,
                severity: Severity::Info,
                message: format!(
                    "collapsed duplicate mesh refspecs on remote `{remote}` (fetch: {fd}, push: {pd})"
                ),
                remediation: None,
            });
        }
    }

    // ---- Reflog coverage for mesh refs (Slice 6d) -------------------
    if crate::git::log_all_ref_updates_value(repo).as_deref() != Some("always")
        && crate::git::ensure_log_all_ref_updates_always(repo).is_ok()
    {
        out.push(DoctorFinding {
            code: DoctorCode::LogAllRefUpdatesSet,
            severity: Severity::Info,
            message: "set `core.logAllRefUpdates = always` so refs/meshes/* get reflog entries"
                .into(),
            remediation: None,
        });
    }

    // ---- Staging area corruption -------------------------------------
    let git_dir = crate::git::git_dir(repo).to_path_buf();
    check_staging(&git_dir, &mut out);

    // ---- Sidecar integrity (Slice 4) --------------------------------
    check_sidecar_integrity(repo, &mut out);

    // ---- Legacy anchor ref hygiene ----------------------------------
    check_legacy_anchor_refs(repo, &mut out);

    // ---- File index self-heal ---------------------------------------
    check_file_index(repo, &mut out);

    Ok(out)
}

fn check_legacy_anchor_refs(repo: &gix::Repository, out: &mut Vec<DoctorFinding>) {
    let Ok(anchor_refs) = crate::git::list_refs_stripped(repo, "refs/anchors/v1") else {
        return;
    };
    if anchor_refs.is_empty() {
        return;
    }
    let mut active = BTreeSet::new();
    let Ok(mesh_names) = list_mesh_names(repo) else {
        return;
    };
    for mesh_name in mesh_names {
        let Ok(mesh) = crate::mesh::read_mesh(repo, &mesh_name) else {
            continue;
        };
        for (anchor_id, _anchor) in mesh.anchors_v2 {
            active.insert(anchor_id);
        }
    }
    for anchor_id in anchor_refs {
        if !active.contains(&anchor_id) {
            out.push(DoctorFinding {
                code: DoctorCode::DanglingRangeRef,
                severity: Severity::Warn,
                message: format!("legacy anchor ref `{anchor_id}` is not referenced by any mesh"),
                remediation: None,
            });
        }
    }
}

/// Resolve the hooks directory for `repo`, honoring `core.hooksPath`.
///
/// Strategy: use `crate::git::config_string` to read `core.hooksPath` from
/// the repository's merged config (gix handles all include/worktree layering).
/// If set and relative, resolve it relative to the worktree root (git's own
/// behaviour for relative `core.hooksPath` values). If unset, fall back to
/// `<common_git_dir>/hooks` which is where git itself looks by default.
///
/// We prefer gix-native config reading over shelling out to avoid the process
/// cost and to stay read-only.
fn resolve_hooks_dir(repo: &gix::Repository) -> std::path::PathBuf {
    if let Some(path_str) = crate::git::config_string(repo, "core.hooksPath") {
        let p = std::path::Path::new(&path_str);
        if p.is_absolute() {
            return p.to_path_buf();
        }
        // Relative: resolve against the worktree root, per git's convention.
        if let Ok(wd) = crate::git::work_dir(repo) {
            return wd.join(p);
        }
    }
    // Fallback: the canonical hooks dir inside the common git dir.
    crate::git::common_dir(repo).join("hooks")
}

/// The ignore directive. Must appear as an entire trimmed line to silence doctor.
const IGNORE_DIRECTIVE: &str = "# git-mesh-doctor-ignore";

/// Outcome of scanning a single hook file.
enum HookOutcome {
    /// Marker found or ignore directive found — no finding needed.
    Pass,
    /// Hook file does not exist.
    Missing,
    /// Could not verify: accumulated unresolvable references or parse errors.
    Unverifiable { refs: Vec<String> },
}

fn scan_hook_file(
    hook_path: &std::path::Path,
    marker: &str,
    hook_dir: &std::path::Path,
    worktree_root: &std::path::Path,
) -> HookOutcome {
    let content = match fs::read_to_string(hook_path) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return HookOutcome::Missing,
        Err(_) => return HookOutcome::Missing,
    };
    scan_content(&content, hook_path, marker, hook_dir, worktree_root, true)
}

/// Scan the textual content of a hook file.
/// `follow_chain`: when true, follow one level of resolvable literal paths.
fn scan_content(
    content: &str,
    hook_path: &std::path::Path,
    marker: &str,
    hook_dir: &std::path::Path,
    worktree_root: &std::path::Path,
    follow_chain: bool,
) -> HookOutcome {
    // Step 1: ignore directive — any line whose trimmed form equals the directive.
    for line in content.lines() {
        if line.trim() == IGNORE_DIRECTIVE {
            return HookOutcome::Pass;
        }
    }

    // Step 2: marker present anywhere in the content.
    if content.contains(marker) {
        return HookOutcome::Pass;
    }

    // Step 3: tokenize each line and classify tokens.
    // Canonicalize hook_dir and worktree_root so starts_with comparisons work
    // even when the paths contain symlinks or relative components.
    let hook_dir_canon = hook_dir.canonicalize().unwrap_or_else(|_| hook_dir.to_path_buf());
    let worktree_root_canon = worktree_root.canonicalize().unwrap_or_else(|_| worktree_root.to_path_buf());
    let hook_dir_str = hook_path.parent().unwrap_or(hook_dir);
    let mut unresolvable_refs: Vec<String> = Vec::new();
    let mut chained_paths: Vec<std::path::PathBuf> = Vec::new();

    for line in content.lines() {
        // Pre-process: replace ${0} and $(dirname "$0")/<rest> before tokenizing.
        let preprocessed = preprocess_line(line, hook_path);

        match shell_words::split(&preprocessed) {
            Err(e) => {
                unresolvable_refs.push(format!("parse error: {e}"));
            }
            Ok(tokens) => {
                for token in tokens {
                    match classify_token(&token, hook_dir_str) {
                        TokenClass::Resolvable(p) => {
                            chained_paths.push(p);
                        }
                        TokenClass::Unresolvable(s) => {
                            unresolvable_refs.push(s);
                        }
                        TokenClass::Skip => {}
                    }
                }
            }
        }
    }

    // Step 4: for each resolvable literal, read and re-check (one level).
    if follow_chain {
        for path in &chained_paths {
            // Containment check: must be under hook_dir or worktree_root.
            let canonical = match path.canonicalize() {
                Ok(c) => c,
                Err(_) => {
                    // File doesn't exist or unreadable — not a pass.
                    continue;
                }
            };
            let in_tree = canonical.starts_with(&hook_dir_canon)
                || canonical.starts_with(&worktree_root_canon);
            if !in_tree {
                unresolvable_refs.push(format!("out-of-tree: {}", canonical.display()));
                continue;
            }
            // Read chained file.
            if let Ok(chained) = fs::read_to_string(&canonical) {
                // One level: pass ignore + marker; no further tokenization.
                let passes = chained.lines().any(|l| l.trim() == IGNORE_DIRECTIVE)
                    || chained.contains(marker);
                if passes {
                    return HookOutcome::Pass;
                }
            }
        }
    }

    // Step 5: decide outcome.
    if !unresolvable_refs.is_empty() {
        HookOutcome::Unverifiable { refs: unresolvable_refs }
    } else {
        // No resolvable chains passed and no unresolvable refs — marker is missing.
        HookOutcome::Missing
    }
}

/// Replace `${0}` with the hook path and `$(dirname "$0")/` with the hook's
/// parent directory, before shell tokenization. This handles the common wrapper
/// idiom without needing full shell evaluation.
fn preprocess_line(line: &str, hook_path: &std::path::Path) -> String {
    let mut result = line.to_string();

    // Replace $(dirname "$0")/ with the hook's parent directory.
    if let Some(parent) = hook_path.parent() {
        let parent_s = parent.to_string_lossy();
        // Handle both $(dirname "$0")/ and $(dirname "$0") patterns.
        result = result.replace(
            "$(dirname \"$0\")/",
            &format!("{}/", parent_s),
        );
        result = result.replace("$(dirname \"$0\")", parent_s.as_ref());
    }

    // Replace ${0} with the hook path itself.
    let hook_s = hook_path.to_string_lossy();
    result = result.replace("${0}", &hook_s);

    result
}

enum TokenClass {
    /// A resolvable literal path.
    Resolvable(std::path::PathBuf),
    /// An unresolvable reference (variable, command substitution, backtick).
    Unresolvable(String),
    /// Not a path token — skip.
    Skip,
}

fn classify_token(
    token: &str,
    hook_dir: &std::path::Path,
) -> TokenClass {
    // Skip empty tokens and obvious shell keywords / flags.
    if token.is_empty() || token.starts_with('-') {
        return TokenClass::Skip;
    }

    // Shell assignment: VAR=<rhs>. Classify the RHS.
    // An assignment looks like: identifier chars followed by '=' with no '/'
    // before the '='.
    let effective = if let Some(eq_idx) = token.find('=') {
        let lhs = &token[..eq_idx];
        // lhs must be a valid shell identifier (letters, digits, underscore;
        // no path separators).
        if !lhs.is_empty() && !lhs.contains('/') && lhs.chars().all(|c| c.is_alphanumeric() || c == '_') {
            &token[eq_idx + 1..]
        } else {
            token
        }
    } else {
        token
    };

    // Unresolvable: contains unsubstituted variables or command substitutions.
    if effective.contains('$') || effective.contains('`') {
        return TokenClass::Unresolvable(effective.to_string());
    }

    // Empty RHS — skip (e.g. VAR=).
    if effective.is_empty() {
        return TokenClass::Skip;
    }

    // Resolvable literal: absolute path or relative (./  ../).
    if effective.starts_with('/') {
        return TokenClass::Resolvable(std::path::PathBuf::from(effective));
    }
    if effective.starts_with("./") || effective.starts_with("../") {
        return TokenClass::Resolvable(hook_dir.join(effective));
    }

    // Check if it looks like a plain command name (no path separator) — skip.
    if !effective.contains('/') {
        return TokenClass::Skip;
    }

    // Relative path with directory components — resolve against hook_dir.
    let resolved = hook_dir.join(effective);
    // Sanity: if it contains $ or backticks after join, still unresolvable.
    let s = resolved.to_string_lossy();
    if s.contains('$') || s.contains('`') {
        return TokenClass::Unresolvable(effective.to_string());
    }
    TokenClass::Resolvable(resolved)
}

fn check_hook(
    repo: &gix::Repository,
    name: &str,
    marker: &str,
    suggested_body: &str,
    code_missing: DoctorCode,
    out: &mut Vec<DoctorFinding>,
) {
    let hook_dir = resolve_hooks_dir(repo);
    let hook_path = hook_dir.join(name);
    let worktree_root = crate::git::work_dir(repo)
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|_| hook_dir.clone());
    let outcome = scan_hook_file(&hook_path, marker, &hook_dir, &worktree_root);
    match outcome {
        HookOutcome::Pass => {}
        HookOutcome::Missing => {
            out.push(DoctorFinding {
                code: code_missing,
                severity: Severity::Info,
                message: format!(
                    "`{name}` hook not installed — install at {} with body: {suggested_body}",
                    hook_path.display()
                ),
                remediation: Some(format!(
                    "Add `{marker}` to {} or run `git mesh hooks git {name}` directly.",
                    hook_path.display()
                )),
            });
        }
        HookOutcome::Unverifiable { refs } => {
            out.push(DoctorFinding {
                code: DoctorCode::CouldNotVerifyHook {
                    hook: name.to_string(),
                    unresolvable_refs: refs.clone(),
                },
                severity: Severity::Info,
                message: format!(
                    "`{name}` hook could not be verified — unresolvable references: {}",
                    refs.join(", ")
                ),
                remediation: Some(format!(
                    "Ensure the hook or a chained script contains `{marker}`, \
                     or add `{IGNORE_DIRECTIVE}` as a whole line to opt out of this check."
                )),
            });
        }
    }
}

fn check_staging(git_dir: &std::path::Path, out: &mut Vec<DoctorFinding>) {
    let dir = git_dir.join("mesh").join("staging");
    if !dir.exists() {
        return;
    }
    // Group files: ops files (no dot) vs. sidecars (<name>.<N>) vs. .why
    let mut ops_files: Vec<(String, std::path::PathBuf)> = Vec::new();
    let mut sidecars: Vec<(String, u32, std::path::PathBuf)> = Vec::new();
    let Ok(entries) = fs::read_dir(&dir) else {
        return;
    };
    for e in entries.flatten() {
        let fname = e.file_name();
        let Some(fn_str) = fname.to_str() else {
            continue;
        };
        if let Some((base, rest)) = fn_str.rsplit_once('.') {
            if rest == "why" {
                continue;
            }
            if let Ok(n) = rest.parse::<u32>() {
                // `base` is filesystem-encoded (`%2F` for `/` per
                // `staging::encode_name_for_fs`); decode for display and
                // for matching the ops-file basename.
                let decoded = crate::staging::decode_name_from_fs(base);
                sidecars.push((decoded, n, e.path()));
                continue;
            }
            // Unknown extension — skip
            continue;
        }
        ops_files.push((crate::staging::decode_name_from_fs(fn_str), e.path()));
    }

    for (name, path) in &ops_files {
        let Ok(text) = fs::read_to_string(path) else {
            continue;
        };
        let mut add_n: u32 = 0;
        let mut expected_sidecars: BTreeSet<u32> = BTreeSet::new();
        for (idx, line) in text.lines().enumerate() {
            let lineno = idx + 1;
            if line.trim().is_empty() {
                continue;
            }
            if let Some(rest) = line.strip_prefix("add ") {
                add_n += 1;
                let (addr, anchor) = match rest.split_once('\t') {
                    Some((addr, anchor)) => (addr, Some(anchor)),
                    None => (rest, None),
                };
                if !is_valid_addr(addr) {
                    out.push(DoctorFinding {
                        code: DoctorCode::StagingCorrupt,
                        severity: Severity::Error,
                        message: format!("malformed staging line in {}:{lineno}", path.display()),
                        remediation: Some(format!("`git mesh restore {name}` and re-stage")),
                    });
                    continue;
                }
                if anchor.is_none() {
                    // expect sidecar <name>.<add_n>
                    expected_sidecars.insert(add_n);
                    let sidecar_p = dir.join(format!(
                        "{}.{add_n}",
                        crate::staging::encode_name_for_fs(name)
                    ));
                    if !sidecar_p.exists() {
                        // Pair-write atomicity: with the sidecar-first
                        // writer in `append_prepared_add`, this state can
                        // only arise from a writer interrupted mid-pair
                        // (signal, OOM, devcontainer reload). It is
                        // self-healing on the next staging read for `name`,
                        // so downgrade to Warn with a non-destructive
                        // remediation rather than forcing `git mesh restore`.
                        out.push(DoctorFinding {
                            code: DoctorCode::StagingCorrupt,
                            severity: Severity::Warn,
                            message: format!(
                                "missing sidecar for {}:{lineno} (expected {}) — \
                                 likely an interrupted `git mesh add`/`why`; \
                                 auto-recovers on next read",
                                path.display(),
                                sidecar_p.display()
                            ),
                            remediation: Some(format!(
                                "run any `git mesh` read for `{name}` (e.g. `git mesh ls {name}`) to auto-recover, or re-stage the affected anchor"
                            )),
                        });
                    }
                }
            } else if let Some(rest) = line.strip_prefix("remove ") {
                if !is_valid_addr(rest) {
                    out.push(DoctorFinding {
                        code: DoctorCode::StagingCorrupt,
                        severity: Severity::Error,
                        message: format!("malformed staging line in {}:{lineno}", path.display()),
                        remediation: Some(format!("`git mesh restore {name}` and re-stage")),
                    });
                }
            } else if line.starts_with("config ") {
                // permissive: validated at commit time
            } else {
                out.push(DoctorFinding {
                    code: DoctorCode::StagingCorrupt,
                    severity: Severity::Error,
                    message: format!("unknown staging op in {}:{lineno}", path.display()),
                    remediation: Some(format!("`git mesh restore {name}` and re-stage")),
                });
            }
        }
        // Orphaned sidecars: sidecars for `name` whose N isn't in expected_sidecars.
        for (sc_name, n, sc_path) in &sidecars {
            if sc_name == name && !expected_sidecars.contains(n) {
                out.push(DoctorFinding {
                    code: DoctorCode::StagingCorrupt,
                    severity: Severity::Warn,
                    message: format!(
                        "orphaned sidecar {} (no matching anchor-less `add` line)",
                        sc_path.display()
                    ),
                    remediation: Some(format!(
                        "delete {} or `git mesh restore {name}`",
                        sc_path.display()
                    )),
                });
            }
        }
    }

    // Sidecars whose basename has no ops file at all.
    let ops_names: BTreeSet<&str> = ops_files.iter().map(|(n, _)| n.as_str()).collect();
    for (sc_name, _n, sc_path) in &sidecars {
        if !ops_names.contains(sc_name.as_str()) {
            out.push(DoctorFinding {
                code: DoctorCode::StagingCorrupt,
                severity: Severity::Warn,
                message: format!(
                    "orphaned sidecar {} (no staging ops file for `{sc_name}`)",
                    sc_path.display()
                ),
                remediation: Some(format!("delete {}", sc_path.display())),
            });
        }
    }
}

fn is_valid_addr(s: &str) -> bool {
    let Some((path, frag)) = s.split_once("#L") else {
        return false;
    };
    if path.is_empty() {
        return false;
    }
    let Some((a, b)) = frag.split_once("-L") else {
        return false;
    };
    let (Ok(a), Ok(b)) = (a.parse::<u32>(), b.parse::<u32>()) else {
        return false;
    };
    a >= 1 && b >= a
}

fn check_sidecar_integrity(repo: &gix::Repository, out: &mut Vec<DoctorFinding>) {
    let dir = crate::git::mesh_dir(repo).join("staging");
    if !dir.exists() {
        return;
    }
    // Collect the set of mesh names with an ops file (no extension).
    let entries = match fs::read_dir(&dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    let mut mesh_names: Vec<String> = Vec::new();
    for e in entries.flatten() {
        let Some(fname) = e.file_name().to_str().map(str::to_string) else {
            continue;
        };
        if !fname.contains('.') {
            mesh_names.push(fname);
        }
    }
    for name in &mesh_names {
        let staging = match crate::staging::read_staging(repo, name) {
            Ok(s) => s,
            Err(_) => continue,
        };
        for add in &staging.adds {
            match crate::staging::read_sidecar_verified(repo, name, add.line_number) {
                Ok(_) => {}
                Err(crate::staging::SidecarVerifyError::Tampered) => {
                    out.push(DoctorFinding {
                        code: DoctorCode::SidecarTampered,
                        severity: Severity::Error,
                        message: format!(
                            "sidecar for mesh `{name}` slot {} (`{}`) failed integrity check",
                            add.line_number, add.path
                        ),
                        remediation: Some(format!(
                            "`git mesh restore {name}` and re-stage `{}`",
                            add.path
                        )),
                    });
                }
                Err(crate::staging::SidecarVerifyError::Missing) => {
                    // Already covered by `check_staging`'s "missing
                    // sidecar" finding; don't double-report.
                }
            }
        }
    }
}

fn check_file_index(repo: &gix::Repository, out: &mut Vec<DoctorFinding>) {
    let p = crate::git::mesh_dir(repo).join("file-index");
    let problem: Option<String> = if !p.exists() {
        Some("file index missing".into())
    } else {
        match fs::read_to_string(&p) {
            Ok(text) if text.starts_with("# mesh-index v2") => None,
            Ok(_) => Some("file index header missing or corrupt".into()),
            Err(e) => Some(format!("file index unreadable: {e}")),
        }
    };
    if let Some(msg) = problem {
        out.push(DoctorFinding {
            code: DoctorCode::FileIndexMissing,
            severity: Severity::Warn,
            message: msg,
            remediation: Some("regenerating automatically".into()),
        });
        match file_index::rebuild_index(repo) {
            Ok(()) => out.push(DoctorFinding {
                code: DoctorCode::FileIndexRebuilt,
                severity: Severity::Info,
                message: "file index regenerated".into(),
                remediation: None,
            }),
            Err(e) => out.push(DoctorFinding {
                code: DoctorCode::FileIndexRebuilt,
                severity: Severity::Error,
                message: format!("file index regeneration failed: {e}"),
                remediation: Some("inspect `.git/mesh/file-index` manually".into()),
            }),
        }
    }
}


pub fn run_doctor(repo: &gix::Repository, args: crate::cli::DoctorArgs) -> Result<i32> {
    if args.gc_trail_cache {
        match crate::resolver::cache::Cache::open(repo) {
            Ok(cache) => match cache.gc(repo) {
                Ok(report) => {
                    println!(
                        "mesh doctor: sqlite cache gc — removed {} name_status, {} blob_diff, {} grouped_walk, {} rename_trail row(s)",
                        report.name_status_removed,
                        report.blob_diff_removed,
                        report.grouped_walk_removed,
                        report.rename_trail_removed,
                    );
                }
                Err(e) => {
                    println!("git-mesh: cache gc failed: {e}");
                }
            },
            Err(e) => {
                println!("git-mesh: cache open failed: {e}");
            }
        }
    }
    let findings = doctor_run(repo)?;
    let names = list_mesh_names(repo).unwrap_or_default();
    let n_meshes = names.len();

    if findings.is_empty() {
        println!(
            "mesh doctor: {n_meshes} meshes checked, no findings.{IDEMPOTENT_TAG}",
        );
        return Ok(0);
    }

    // Print findings report.
    println!("# mesh doctor");
    println!();
    println!("{n_meshes} meshes checked, {} findings.", findings.len());
    println!();
    println!("## Findings");
    println!();

    for f in &findings {
        let label = match f.severity {
            Severity::Info => "INFO",
            Severity::Warn => "WARN",
            Severity::Error => "ERROR",
        };
        let code = format!("{:?}", f.code);
        print!("- {label} `{code}` — {}", f.message);
        if let Some(rem) = &f.remediation {
            println!();
            println!("  {rem}");
        } else {
            println!();
        }
    }

    // Severity-driven exit codes (§6.7):
    //   ERROR              → exit 1
    //   INFO / WARN only   → exit 0
    //   --strict promotes INFO and WARN to exit 1
    let has_error = findings.iter().any(|f| f.severity == Severity::Error);
    let has_non_ok = findings.iter().any(|f| {
        matches!(
            f.severity,
            Severity::Info | Severity::Warn | Severity::Error
        )
    });

    let strict_escalates = args.strict && has_non_ok;

    // Print "What to do next" on stdout for non-strict escalations.
    // When strict escalates, the CliError on stderr provides the guidance instead.
    if !strict_escalates {
        println!();
        println!("## What to do next");
        println!();
        if has_error {
            println!("Address the ERROR-level findings before staging more work. The other findings are advisory.");
        } else {
            println!("All findings are advisory. No action is required.");
        }
    }

    if args.strict && has_non_ok {
        let non_error_count = findings
            .iter()
            .filter(|f| f.severity != Severity::Error)
            .count();
        let kind = if non_error_count > 0 {
            "INFO/WARN"
        } else {
            "ERROR"
        };
        return Err(CliError {
            subcommand: "doctor",
            summary: format!(
                "failing because `--strict` was set and {non_error_count} {kind} finding(s) was reported."
            ),
            what_happened: "In strict mode, INFO and WARN findings escalate to a non-zero exit so CI catches advisory drift early."
                .into(),
            next_steps: vec![NextStep::Prose(
                "Fix the finding above, or drop `--strict` if you only want errors to fail the run."
                    .into(),
            )],
        }
        .into());
    }

    if has_error {
        Ok(1)
    } else {
        Ok(0)
    }
}

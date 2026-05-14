//! Structural handlers (restore, revert, delete, move) + doctor — §6.6, §6.7, §6.8.

use crate::cli::{CliError, DeleteArgs, DoctorArgs, MoveArgs, NextStep, RestoreArgs, RevertArgs};
use crate::cli::format::{DESTRUCTIVE_TAG, IDEMPOTENT_TAG};
use crate::mesh::catalog::Catalog;
use crate::sync::default_remote;
use crate::{delete_mesh, file_index, list_mesh_names, rename_mesh, revert_mesh};
use anyhow::Result;
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
    // Validate the target commit-ish resolves.
    let target = repo
        .rev_parse_single(args.commit_ish.as_str())
        .map_err(|_| {
            CliError {
                subcommand: "revert",
                summary: format!("cannot resolve commit-ish `{}`.", args.commit_ish),
                what_happened: format!(
                    "`{}` could not be resolved to a commit in this repository.",
                    args.commit_ish
                ),
                next_steps: vec![
                    NextStep::Prose("Check the commit-ish for typos:".into()),
                    NextStep::Bash(format!("git log --oneline {}", args.commit_ish)),
                ],
            }
        })?;
    let target_oid = target.detach().to_string();

    // Verify the mesh exists in the catalog at the target commit.
    let mesh_at_target = crate::mesh::read::read_mesh_at(repo, &args.name, Some(&target_oid));
    if mesh_at_target.is_err() {
        return Err(CliError {
            subcommand: "revert",
            summary: format!("`{}` does not contain mesh `{}`.", args.commit_ish, args.name),
            what_happened: format!(
                "The commit `{}` does not have a catalog entry for mesh `{}`. \
                 `git mesh revert` requires a catalog commit that contains the mesh.",
                target_oid.chars().take(7).collect::<String>(),
                args.name,
            ),
            next_steps: vec![
                NextStep::Bash(format!("git mesh {} --log", args.name)),
            ],
        }.into());
    }

    let new_commit = revert_mesh(repo, &args.name, &args.commit_ish)?;
    let short_new: String = new_commit.chars().take(7).collect();
    let short_target: String = target_oid.chars().take(7).collect();
    println!(
        "Reverted `{}` to `{}` (commit {}).{}",
        args.name, short_target, short_new, DESTRUCTIVE_TAG
    );
    println!();
    println!("Run `git mesh {}` to verify.", args.name);
    Ok(0)
}

pub fn run_delete(repo: &gix::Repository, args: DeleteArgs) -> Result<i32> {
    // Check existence via catalog for a structured error.
    let catalog = Catalog::load(repo)?;
    let current_oid = catalog
        .entry_oid(&args.name)
        .ok_or_else(|| {
            CliError {
                subcommand: "delete",
                summary: format!("no mesh named `{}`.", args.name),
                what_happened: format!("`{}` is not tracked in the mesh catalog.", args.name),
                next_steps: vec![NextStep::Bash("git mesh list".into())],
            }
        })?;

    delete_mesh(repo, &args.name)?;
    let short: String = current_oid.chars().take(7).collect();
    println!(
        "Deleted `{}` (blob {}).{}",
        args.name, short, DESTRUCTIVE_TAG
    );
    println!();
    println!("Run `git mesh list` to confirm the mesh is gone.");
    Ok(0)
}

pub fn run_move(repo: &gix::Repository, args: MoveArgs) -> Result<i32> {
    // Check destination existence via catalog for a structured error.
    let catalog = Catalog::load(repo)?;
    if catalog.lookup(&args.new)?.is_some() {
        return Err(CliError {
            subcommand: "move",
            summary: format!("`{}` already exists.", args.new),
            what_happened: format!(
                "A mesh is already tracked as `{}`, so renaming `{}` over it would lose data.",
                args.new, args.old
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

    // Check source existence via catalog for a structured error.
    if catalog.lookup(&args.old)?.is_none() {
        return Err(CliError {
            subcommand: "move",
            summary: format!("no mesh named `{}`.", args.old),
            what_happened: format!("`{}` is not tracked in the mesh catalog.", args.old),
            next_steps: vec![NextStep::Bash("git mesh list".into())],
        }
        .into());
    }

    rename_mesh(repo, &args.old, &args.new)?;
    println!(
        "Renamed `{}` to `{}`. The mesh's history is preserved on the catalog ref.{}",
        args.old, args.new, DESTRUCTIVE_TAG
    );
    println!();
    println!("Run `git mesh {}` to verify the rename.", args.new);
    let _ = default_remote;
    Ok(0)
}

#[derive(Clone, Debug, PartialEq, Eq)]
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
    DuplicateRefspec,
    LogAllRefUpdatesSet,
    MissingPostRewriteHook,
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
        let Ok(mesh) = crate::mesh::read::read_mesh(repo, &mesh_name) else {
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

fn resolve_hooks_dir(repo: &gix::Repository) -> std::path::PathBuf {
    if let Some(path_str) = crate::git::config_string(repo, "core.hooksPath") {
        let p = std::path::Path::new(&path_str);
        if p.is_absolute() {
            return p.to_path_buf();
        }
        if let Ok(wd) = crate::git::work_dir(repo) {
            return wd.join(p);
        }
    }
    crate::git::common_dir(repo).join("hooks")
}

const IGNORE_DIRECTIVE: &str = "# git-mesh-doctor-ignore";

enum HookOutcome {
    Pass,
    Missing,
    Unverifiable { refs: Vec<String> },
}

fn scan_hook_file(
    hook_path: &std::path::Path,
    marker: &str,
    _hook_dir: &std::path::Path,
) -> HookOutcome {
    if !hook_path.exists() {
        return HookOutcome::Missing;
    }
    let content = match fs::read_to_string(hook_path) {
        Ok(c) => c,
        Err(_) => return HookOutcome::Unverifiable { refs: Vec::new() },
    };
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed == IGNORE_DIRECTIVE {
            return HookOutcome::Pass;
        }
        if trimmed == marker {
            return HookOutcome::Pass;
        }
    }
    // Check for unresolvable refs in the hook script.
    let mut refs: Vec<String> = Vec::new();
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("refs/") || trimmed.starts_with("$GIT_DIR/refs/") {
            refs.push(trimmed.to_string());
        }
    }
    HookOutcome::Unverifiable { refs }
}

fn check_hook(
    repo: &gix::Repository,
    hook_name: &str,
    marker: &str,
    default_body: &str,
    code: DoctorCode,
    out: &mut Vec<DoctorFinding>,
) {
    let hooks_dir = resolve_hooks_dir(repo);
    let hook_path = hooks_dir.join(hook_name);
    let outcome = scan_hook_file(&hook_path, marker, &hooks_dir);
    match outcome {
        HookOutcome::Pass => {}
        HookOutcome::Missing => {
            // Install the hook.
            if let Some(parent) = hook_path.parent() {
                let _ = fs::create_dir_all(parent);
            }
            if fs::write(&hook_path, default_body).is_ok() {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let _ = fs::set_permissions(&hook_path, std::fs::Permissions::from_mode(0o755));
                }
                out.push(DoctorFinding {
                    severity: Severity::Info,
                    message: format!("installed missing `{hook_name}` hook"),
                    code,
                    remediation: None,
                });
            } else {
                out.push(DoctorFinding {
                    severity: Severity::Warn,
                    message: format!("missing `{hook_name}` hook; could not install"),
                    code,
                    remediation: Some(format!(
                        "manually create `{}` with the contents:\n{default_body}",
                        hook_path.display()
                    )),
                });
            }
        }
        HookOutcome::Unverifiable { refs } => {
            if !refs.is_empty() {
                out.push(DoctorFinding {
                    code: DoctorCode::CouldNotVerifyHook {
                        hook: hook_name.to_string(),
                        unresolvable_refs: refs,
                    },
                    severity: Severity::Info,
                    message: format!(
                        "hook `{hook_name}` exists but could not be verified as containing the \
                         git-mesh marker; check its contents manually"
                    ),
                    remediation: None,
                });
            }
        }
    }
}

fn check_staging(git_dir: &std::path::Path, out: &mut Vec<DoctorFinding>) {
    let ops_dir = git_dir.join("mesh").join("staging");
    if !ops_dir.exists() {
        return;
    }
    let entries = match fs::read_dir(&ops_dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    let mut mesh_names: Vec<String> = Vec::new();
    for entry in entries.flatten() {
        let fname = match entry.file_name().to_str().map(str::to_string) {
            Some(n) => n,
            None => continue,
        };
        if !fname.contains('.') {
            mesh_names.push(fname.clone());
        }
        // Silently purge `.tmp` files older than 1 hour.
        if fname.ends_with(".tmp")
            && let Ok(meta) = entry.metadata()
            && let Ok(elapsed) = meta.modified().map(|t| t.elapsed())
            && elapsed.unwrap_or_default().as_secs() > 3600
        {
            let _ = fs::remove_file(entry.path());
        }
    }
    for name in &mesh_names {
        let ops_path = ops_dir.join(name);
        let content = match fs::read_to_string(&ops_path) {
            Ok(c) => c,
            Err(_) => continue,
        };
        for line in content.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let parts: Vec<&str> = trimmed.splitn(3, ' ').collect();
            if parts.len() < 2 || !["add", "remove", "config", "why"].contains(&parts[0]) {
                out.push(DoctorFinding {
                    code: DoctorCode::StagingCorrupt,
                    severity: Severity::Error,
                    message: format!("staging file `{name}` has invalid line: `{trimmed}`"),
                    remediation: Some(format!(
                        "`git mesh restore {name}` to discard, then re-stage anchors"
                    )),
                });
                break;
            }
        }
    }
}

fn check_sidecar_integrity(repo: &gix::Repository, out: &mut Vec<DoctorFinding>) {
    let dir = crate::git::mesh_dir(repo).join("staging");
    if !dir.exists() {
        return;
    }
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
                Err(crate::staging::SidecarVerifyError::Missing) => {}
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

pub fn run_doctor(repo: &gix::Repository, args: DoctorArgs) -> Result<i32> {
    if args.gc_trail_cache {
        match crate::resolver::cache::Cache::open(repo) {
            Ok(cache) => match cache.gc(repo) {
                Ok(report) => {
                    println!(
                        "mesh doctor: cache gc — removed {} rename_trail, {} drift_locus entr(ies)",
                        report.rename_trail_removed,
                        report.drift_locus_removed,
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
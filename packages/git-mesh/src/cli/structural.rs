//! Structural handlers (restore, revert, delete, mv) + doctor — §6.6, §6.7, §6.8.

use crate::cli::{DeleteArgs, MvArgs, RestoreArgs, RevertArgs};
use crate::range::range_ref_path;
use crate::sync::default_remote;
use crate::{
    delete_mesh, file_index, list_mesh_names, read_mesh, rename_mesh, restore_mesh, revert_mesh,
};
use anyhow::Result;
use std::collections::BTreeSet;
use std::fs;

pub fn run_restore(repo: &gix::Repository, args: RestoreArgs) -> Result<i32> {
    restore_mesh(repo, &args.name)?;
    Ok(0)
}

pub fn run_revert(repo: &gix::Repository, args: RevertArgs) -> Result<i32> {
    revert_mesh(repo, &args.name, &args.commit_ish)?;
    Ok(0)
}

pub fn run_delete(repo: &gix::Repository, args: DeleteArgs) -> Result<i32> {
    delete_mesh(repo, &args.name)?;
    Ok(0)
}

pub fn run_mv(repo: &gix::Repository, args: MvArgs) -> Result<i32> {
    rename_mesh(repo, &args.old, &args.new)?;
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DoctorCode {
    MissingPostCommitHook,
    MissingPreCommitHook,
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
}

const POST_COMMIT_HOOK_BODY: &str = "#!/bin/sh\ngit mesh commit\n";
const PRE_COMMIT_HOOK_BODY: &str = "#!/bin/sh\ngit mesh pre-commit\n";
const POST_COMMIT_MARKER: &str = "git mesh commit";
const PRE_COMMIT_MARKER: &str = "git mesh pre-commit";

pub fn doctor_run(repo: &gix::Repository) -> crate::Result<Vec<DoctorFinding>> {
    let mut out = Vec::new();
    let wd = crate::git::work_dir(repo)?;
    let git_dir = wd.join(".git");

    // ---- Hook checks --------------------------------------------------
    check_hook(
        &git_dir,
        "post-commit",
        POST_COMMIT_MARKER,
        POST_COMMIT_HOOK_BODY,
        DoctorCode::MissingPostCommitHook,
        &mut out,
    );
    check_hook(
        &git_dir,
        "pre-commit",
        PRE_COMMIT_MARKER,
        PRE_COMMIT_HOOK_BODY,
        DoctorCode::MissingPreCommitHook,
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
    check_staging(&git_dir, &mut out);

    // ---- Sidecar integrity (Slice 4) --------------------------------
    check_sidecar_integrity(repo, &mut out);

    // ---- Orphan range references + dangling range refs --------------
    check_range_reachability(repo, &remote, &mut out);

    // ---- File index self-heal ---------------------------------------
    check_file_index(repo, &mut out);

    Ok(out)
}

fn check_hook(
    git_dir: &std::path::Path,
    name: &str,
    marker: &str,
    suggested_body: &str,
    code: DoctorCode,
    out: &mut Vec<DoctorFinding>,
) {
    let hook_path = git_dir.join("hooks").join(name);
    let ok = fs::read_to_string(&hook_path)
        .map(|s| s.contains(marker))
        .unwrap_or(false);
    if !ok {
        let install = hook_path.display().to_string();
        let suggested = suggested_body.replace('\n', "\\n");
        out.push(DoctorFinding {
            code,
            severity: Severity::Info,
            message: format!("`{name}` hook not installed"),
            remediation: Some(format!("install at {install} with body: {suggested}")),
        });
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
                        out.push(DoctorFinding {
                            code: DoctorCode::StagingCorrupt,
                            severity: Severity::Error,
                            message: format!(
                                "missing sidecar for {}:{lineno} (expected {})",
                                path.display(),
                                sidecar_p.display()
                            ),
                            remediation: Some(format!("`git mesh restore {name}` and re-stage")),
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

fn check_range_reachability(repo: &gix::Repository, remote: &str, out: &mut Vec<DoctorFinding>) {
    let wd = match crate::git::work_dir(repo) {
        Ok(w) => w,
        Err(_) => return,
    };
    let Ok(names) = list_mesh_names(repo) else {
        return;
    };
    // Build set of all referenced range ids.
    let mut referenced: BTreeSet<String> = BTreeSet::new();
    for name in &names {
        let Ok(mesh) = read_mesh(repo, name) else {
            continue;
        };
        for id in &mesh.ranges {
            referenced.insert(id.clone());
            let ref_path = range_ref_path(id);
            let exists = crate::git::resolve_ref_oid_optional(wd, &ref_path)
                .ok()
                .flatten()
                .is_some();
            if !exists {
                // Decide remediation based on whether a remote is configured.
                let remote_url = crate::sync::get_remote_url(repo, remote);
                let remediation = if remote_url.is_some() {
                    format!("`git mesh fetch` to pull `{id}` from `{remote}`")
                } else {
                    format!("`git mesh rm` from `{name}` and re-anchor")
                };
                out.push(DoctorFinding {
                    code: DoctorCode::OrphanRangeRef,
                    severity: Severity::Error,
                    message: format!("mesh `{name}` references missing range `{id}`"),
                    remediation: Some(remediation),
                });
            }
        }
    }

    // Dangling: every refs/ranges/v1/* not in `referenced`.
    let Ok(range_refs) = crate::git::list_refs_stripped(repo, "refs/ranges/v1") else {
        return;
    };
    let _ = wd;
    for id in range_refs.iter().filter(|s| !s.is_empty()) {
        if !referenced.contains(id) {
            let descriptor = match read_range_safe(repo, id) {
                Some(r) => match r.extent {
                    crate::types::RangeExtent::Lines { start, end } => {
                        format!("`{}#L{}-L{}`", r.path, start, end)
                    }
                    crate::types::RangeExtent::Whole => format!("`{}` (whole)", r.path),
                },
                None => format!("`{}`", range_ref_path(id)),
            };
            out.push(DoctorFinding {
                code: DoctorCode::DanglingRangeRef,
                severity: Severity::Info,
                message: format!("dangling range ref at {descriptor} is not referenced by any mesh"),
                remediation: Some(
                    "harmless pending `git gc`; delete with `git update-ref -d` if intended".into(),
                ),
            });
        }
    }
}

/// Return the parsed `Range` for a ref whose object is a blob and parses
/// as the range record format. Returns `None` for non-blob targets (e.g.
/// a stray ref pointing at a commit) or unparseable contents — those
/// callers fall back to the raw ref name.
fn read_range_safe(repo: &gix::Repository, range_id: &str) -> Option<crate::types::Range> {
    let wd = crate::git::work_dir(repo).ok()?;
    let oid_hex = crate::git::resolve_ref_oid_optional(wd, &range_ref_path(range_id))
        .ok()
        .flatten()?;
    let oid = gix::ObjectId::from_hex(oid_hex.as_bytes()).ok()?;
    let obj = repo.find_object(oid).ok()?;
    if obj.kind != gix::object::Kind::Blob {
        return None;
    }
    let text = std::str::from_utf8(&obj.data).ok()?.to_string();
    crate::range::parse_range(&text).ok()
}

/// Slice 4: walk each staging mesh, verify every staged-add sidecar
/// against its `.meta` `content_sha256`, and emit a `SidecarTampered`
/// finding for every mismatch (or missing/empty hash). Per
/// `<fail-closed>`, an empty/absent meta hash is treated as tampering.
fn check_sidecar_integrity(repo: &gix::Repository, out: &mut Vec<DoctorFinding>) {
    let wd = match crate::git::work_dir(repo) {
        Ok(w) => w,
        Err(_) => return,
    };
    let dir = wd.join(".git").join("mesh").join("staging");
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
    let wd = match crate::git::work_dir(repo) {
        Ok(w) => w,
        Err(_) => return,
    };
    let p = wd.join(".git").join("mesh").join("file-index");
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
    let findings = doctor_run(repo)?;
    let names = list_mesh_names(repo).unwrap_or_default();
    println!("mesh doctor: checking refs/meshes/v1/*");
    for n in &names {
        println!("  ok      {n}");
    }
    for f in &findings {
        let label = match f.severity {
            Severity::Info => "INFO  ",
            Severity::Warn => "WARN  ",
            Severity::Error => "ERROR ",
        };
        match &f.remediation {
            Some(r) => println!("  {label} {:?}: {} — {}", f.code, f.message, r),
            None => println!("  {label} {:?}: {}", f.code, f.message),
        }
    }
    if findings.is_empty() {
        if names.is_empty() {
            println!("mesh doctor: ok (no meshes)");
        } else {
            println!("mesh doctor: ok ({} mesh(es) checked)", names.len());
        }
        return Ok(0);
    }
    println!("mesh doctor: found {} finding(s)", findings.len());
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
    if has_error || (args.strict && has_non_ok) {
        Ok(1)
    } else {
        Ok(0)
    }
}

//! Structural handlers (restore, revert, delete, mv) + doctor — §6.6, §6.7, §6.8.

use crate::cli::{DeleteArgs, MvArgs, RestoreArgs, RevertArgs};
use crate::range::range_ref_path;
use crate::sync::default_remote;
use crate::{delete_mesh, file_index, list_mesh_names, read_mesh, rename_mesh, restore_mesh, revert_mesh};
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
}

const POST_COMMIT_HOOK_BODY: &str = "#!/bin/sh\ngit mesh commit\n";
const PRE_COMMIT_HOOK_BODY: &str = "#!/bin/sh\ngit mesh status --check\n";
const POST_COMMIT_MARKER: &str = "git mesh commit";
const PRE_COMMIT_MARKER: &str = "git mesh status --check";

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
    let url = crate::git::git_stdout_optional(
        wd,
        ["config", "--get", &format!("remote.{remote}.url")],
    )
    .unwrap_or(None);
    if url.is_some() {
        let fetch = crate::git::git_stdout_lines(
            wd,
            ["config", "--get-all", &format!("remote.{remote}.fetch")],
        )
        .unwrap_or_default();
        if !fetch.iter().any(|l| l.contains("refs/meshes/")) {
            out.push(DoctorFinding {
                code: DoctorCode::RefspecMissing,
                severity: Severity::Warn,
                message: format!("remote `{remote}` has no mesh refspec"),
                remediation: Some("run `git mesh push` or `fetch` once to bootstrap".into()),
            });
        }
    }

    // ---- Staging area corruption -------------------------------------
    check_staging(&git_dir, &mut out);

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
            remediation: Some(format!(
                "install at {install} with body: {suggested}"
            )),
        });
    }
}

fn check_staging(git_dir: &std::path::Path, out: &mut Vec<DoctorFinding>) {
    let dir = git_dir.join("mesh").join("staging");
    if !dir.exists() {
        return;
    }
    // Group files: ops files (no dot) vs. sidecars (<name>.<N>) vs. .msg
    let mut ops_files: Vec<(String, std::path::PathBuf)> = Vec::new();
    let mut sidecars: Vec<(String, u32, std::path::PathBuf)> = Vec::new();
    let Ok(entries) = fs::read_dir(&dir) else {
        return;
    };
    for e in entries.flatten() {
        let fname = e.file_name();
        let Some(fn_str) = fname.to_str() else { continue };
        if let Some((base, rest)) = fn_str.rsplit_once('.') {
            if rest == "msg" {
                continue;
            }
            if let Ok(n) = rest.parse::<u32>() {
                sidecars.push((base.to_string(), n, e.path()));
                continue;
            }
            // Unknown extension — skip
            continue;
        }
        ops_files.push((fn_str.to_string(), e.path()));
    }

    for (name, path) in &ops_files {
        let Ok(text) = fs::read_to_string(path) else { continue };
        let mut add_n: u32 = 0;
        let mut expected_sidecars: BTreeSet<u32> = BTreeSet::new();
        for (idx, line) in text.lines().enumerate() {
            let lineno = idx + 1;
            if line.trim().is_empty() {
                continue;
            }
            if let Some(rest) = line.strip_prefix("add ") {
                add_n += 1;
                let mut parts = rest.splitn(2, ' ');
                let addr = parts.next().unwrap_or_default();
                let anchor = parts.next();
                if !is_valid_addr(addr) {
                    out.push(DoctorFinding {
                        code: DoctorCode::StagingCorrupt,
                        severity: Severity::Error,
                        message: format!(
                            "malformed staging line in {}:{lineno}",
                            path.display()
                        ),
                        remediation: Some(format!("`git mesh restore {name}` and re-stage")),
                    });
                    continue;
                }
                if anchor.is_none() {
                    // expect sidecar <name>.<add_n>
                    expected_sidecars.insert(add_n);
                    let sidecar_p = dir.join(format!("{name}.{add_n}"));
                    if !sidecar_p.exists() {
                        out.push(DoctorFinding {
                            code: DoctorCode::StagingCorrupt,
                            severity: Severity::Error,
                            message: format!(
                                "missing sidecar for {}:{lineno} (expected {})",
                                path.display(),
                                sidecar_p.display()
                            ),
                            remediation: Some(format!(
                                "`git mesh restore {name}` and re-stage"
                            )),
                        });
                    }
                }
            } else if let Some(rest) = line.strip_prefix("remove ") {
                if !is_valid_addr(rest) {
                    out.push(DoctorFinding {
                        code: DoctorCode::StagingCorrupt,
                        severity: Severity::Error,
                        message: format!(
                            "malformed staging line in {}:{lineno}",
                            path.display()
                        ),
                        remediation: Some(format!("`git mesh restore {name}` and re-stage")),
                    });
                }
            } else if line.starts_with("config ") {
                // permissive: validated at commit time
            } else {
                out.push(DoctorFinding {
                    code: DoctorCode::StagingCorrupt,
                    severity: Severity::Error,
                    message: format!(
                        "unknown staging op in {}:{lineno}",
                        path.display()
                    ),
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
                    remediation: Some(format!("delete {} or `git mesh restore {name}`", sc_path.display())),
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

fn check_range_reachability(
    repo: &gix::Repository,
    remote: &str,
    out: &mut Vec<DoctorFinding>,
) {
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
                let remote_url = crate::git::git_stdout_optional(
                    wd,
                    ["config", "--get", &format!("remote.{remote}.url")],
                )
                .unwrap_or(None);
                let remediation = if remote_url.is_some() {
                    format!("`git mesh fetch` to pull `{id}` from `{remote}`")
                } else {
                    format!("`git mesh rm` from `{name}` and re-anchor")
                };
                out.push(DoctorFinding {
                    code: DoctorCode::OrphanRangeRef,
                    severity: Severity::Error,
                    message: format!(
                        "mesh `{name}` references missing range `{id}`"
                    ),
                    remediation: Some(remediation),
                });
            }
        }
    }

    // Dangling: every refs/ranges/v1/* not in `referenced`.
    let Ok(range_refs) = crate::git::git_stdout_lines(
        wd,
        [
            "for-each-ref",
            "--format=%(refname:strip=3)",
            "refs/ranges/v1",
        ],
    ) else {
        return;
    };
    for id in range_refs.iter().filter(|s| !s.is_empty()) {
        if !referenced.contains(id) {
            out.push(DoctorFinding {
                code: DoctorCode::DanglingRangeRef,
                severity: Severity::Info,
                message: format!("range `{id}` is not referenced by any mesh"),
                remediation: Some(
                    "harmless pending `git gc`; delete with `git update-ref -d` if intended".into(),
                ),
            });
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
            Ok(text) if text.starts_with("# mesh-index v1") => None,
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

pub fn run_doctor(repo: &gix::Repository) -> Result<i32> {
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
        Ok(0)
    } else {
        println!("mesh doctor: found {} finding(s)", findings.len());
        // Exit 1 if any finding (fail-closed per CLAUDE.md).
        Ok(1)
    }
}

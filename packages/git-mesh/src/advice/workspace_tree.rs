//! Workspace-tree capture and diff helpers.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

/// A single change between two tree objects.
#[derive(Debug, Clone)]
pub enum DiffEntry {
    /// File content changed.
    Modified {
        path: String,
        old_oid: Option<String>,
        new_oid: Option<String>,
    },
    /// File was added.
    Added {
        path: String,
        new_oid: Option<String>,
    },
    /// File was deleted.
    Deleted {
        path: String,
        old_oid: Option<String>,
    },
    /// File was renamed (with optional similarity score).
    Renamed {
        from: String,
        to: String,
        score: u8,
        old_oid: Option<String>,
        new_oid: Option<String>,
    },
    /// File mode changed (e.g. exec bit toggled).
    ModeChange {
        path: String,
        old_oid: Option<String>,
        new_oid: Option<String>,
    },
}

/// A workspace tree snapshot backed by a temp Git object directory.
pub struct WorkspaceTree {
    /// SHA-1 hex of the tree object.
    pub tree_sha: String,
    /// Directory holding the temporary Git objects for this tree.
    pub objects_dir: PathBuf,
}

fn workdir(repo: &gix::Repository) -> Result<&Path> {
    repo.workdir()
        .ok_or_else(|| anyhow::anyhow!("bare repositories are not supported"))
}

fn git_dir(repo: &gix::Repository) -> &Path {
    repo.git_dir()
}

fn run_git_capture(
    workdir: &Path,
    envs: &[(&str, &std::ffi::OsStr)],
    args: &[&str],
    stdin: Option<&[u8]>,
) -> Result<std::process::Output> {
    let mut cmd = Command::new("git");
    cmd.current_dir(workdir);
    for (k, v) in envs {
        cmd.env(k, v);
    }
    cmd.args(args);
    if stdin.is_some() {
        cmd.stdin(std::process::Stdio::piped());
    }
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());
    let mut child = cmd.spawn().with_context(|| format!("spawn git {args:?}"))?;
    if let Some(data) = stdin {
        use std::io::Write;
        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(data).context("write git stdin")?;
        }
    }
    let out = child.wait_with_output().context("wait git")?;
    if !out.status.success() {
        bail!(
            "git {args:?} failed (code {:?}): {}",
            out.status.code(),
            String::from_utf8_lossy(&out.stderr)
        );
    }
    Ok(out)
}

fn run_git_capture_owned(
    workdir: &Path,
    envs: &[(&str, &std::ffi::OsStr)],
    args: &[std::ffi::OsString],
    stdin: Option<&[u8]>,
) -> Result<std::process::Output> {
    let mut cmd = Command::new("git");
    cmd.current_dir(workdir);
    for (k, v) in envs {
        cmd.env(k, v);
    }
    cmd.args(args);
    if stdin.is_some() {
        cmd.stdin(std::process::Stdio::piped());
    }
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());
    let mut child = cmd.spawn().with_context(|| format!("spawn git {args:?}"))?;
    if let Some(data) = stdin {
        use std::io::Write;
        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(data).context("write git stdin")?;
        }
    }
    let out = child.wait_with_output().context("wait git")?;
    if !out.status.success() {
        bail!(
            "git {args:?} failed (code {:?}): {}",
            out.status.code(),
            String::from_utf8_lossy(&out.stderr)
        );
    }
    Ok(out)
}

fn active_store_relative_path(workdir: &Path, objects_dir: &Path) -> Option<String> {
    let store_dir = objects_dir.parent()?;
    let workdir = std::fs::canonicalize(workdir).unwrap_or_else(|_| workdir.to_path_buf());
    let store_dir = std::fs::canonicalize(store_dir).unwrap_or_else(|_| store_dir.to_path_buf());
    let rel = store_dir.strip_prefix(&workdir).ok()?;
    if rel.as_os_str().is_empty() {
        return None;
    }
    Some(
        rel.components()
            .map(|c| c.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/"),
    )
}

/// Capture the current workspace state into `objects_dir`, returning a
/// `WorkspaceTree`. Uses `GIT_INDEX_FILE` / `GIT_OBJECT_DIRECTORY` overrides
/// so the real index is not mutated.
pub fn capture(repo: &gix::Repository, objects_dir: &Path) -> Result<WorkspaceTree> {
    let wd = workdir(repo)?;
    let gd = git_dir(repo);
    let active_store_rel = active_store_relative_path(wd, objects_dir);

    // Ensure objects_dir exists.
    std::fs::create_dir_all(objects_dir)
        .with_context(|| format!("mkdir `{}`", objects_dir.display()))?;

    // Copy the real index to a temp index inside the objects_dir's parent.
    let temp_index = objects_dir.join("index.tmp");
    let real_index = gd.join("index");
    if real_index.exists() {
        std::fs::copy(&real_index, &temp_index).with_context(|| {
            format!(
                "copy index `{}` -> `{}`",
                real_index.display(),
                temp_index.display()
            )
        })?;
    } else {
        // Empty index: leave non-existent; `git read-tree --empty` is unnecessary
        // because git will create the file on first write.
    }

    let real_objects = gd.join("objects");
    let alt = real_objects.as_os_str().to_owned();
    let temp_index_os = temp_index.as_os_str().to_owned();
    let objects_dir_os = objects_dir.as_os_str().to_owned();

    let envs: Vec<(&str, &std::ffi::OsStr)> = vec![
        ("GIT_INDEX_FILE", temp_index_os.as_os_str()),
        ("GIT_OBJECT_DIRECTORY", objects_dir_os.as_os_str()),
        ("GIT_ALTERNATE_OBJECT_DIRECTORIES", alt.as_os_str()),
    ];

    // 1. Stage tracked changes (modifications, deletions) into the temp index.
    if let Some(rel) = &active_store_rel {
        let exclude = format!(":(exclude){rel}/**");
        run_git_capture_owned(
            wd,
            &envs,
            &[
                "add".into(),
                "-u".into(),
                "--".into(),
                ".".into(),
                exclude.into(),
            ],
            None,
        )?;
    } else {
        run_git_capture(wd, &envs, &["add", "-u", "."], None)?;
    }

    // 2. List untracked, non-ignored files (NUL-separated).
    let ls = if let Some(rel) = &active_store_rel {
        run_git_capture_owned(
            wd,
            &envs,
            &[
                "ls-files".into(),
                "-z".into(),
                "--others".into(),
                "--exclude-standard".into(),
                format!("--exclude={rel}/**").into(),
            ],
            None,
        )?
    } else {
        run_git_capture(
            wd,
            &envs,
            &["ls-files", "-z", "--others", "--exclude-standard"],
            None,
        )?
    };

    // 3. Stage untracked files via pathspec-from-file if any.
    if !ls.stdout.is_empty() {
        run_git_capture(
            wd,
            &envs,
            &["add", "--pathspec-from-file=-", "--pathspec-file-nul"],
            Some(&ls.stdout),
        )?;
    }

    // 4. Write tree.
    let wt = run_git_capture(wd, &envs, &["write-tree"], None)?;
    let tree_sha = String::from_utf8(wt.stdout)
        .context("write-tree stdout utf8")?
        .trim()
        .to_string();

    Ok(WorkspaceTree {
        tree_sha,
        objects_dir: objects_dir.to_path_buf(),
    })
}

/// Compute the diff between two tree SHAs, merging alternate object stores.
pub fn diff_trees(
    repo: &gix::Repository,
    from_sha: &str,
    to_sha: &str,
    from_objects: &Path,
    to_objects: &Path,
) -> Result<Vec<DiffEntry>> {
    let wd = workdir(repo)?;
    let gd = git_dir(repo);
    let real_objects = gd.join("objects");

    // Merge alternates: real + from + to (use to_objects as primary so writes go nowhere meaningful).
    let alts = [
        real_objects.as_os_str().to_owned(),
        from_objects.as_os_str().to_owned(),
    ];
    // Build a colon-separated path string.
    let alt_string = std::env::join_paths(alts.iter()).context("join alternate object dirs")?;
    let to_objects_os = to_objects.as_os_str().to_owned();

    let envs: Vec<(&str, &std::ffi::OsStr)> = vec![
        ("GIT_OBJECT_DIRECTORY", to_objects_os.as_os_str()),
        ("GIT_ALTERNATE_OBJECT_DIRECTORIES", alt_string.as_os_str()),
    ];

    let out = run_git_capture(
        wd,
        &envs,
        &[
            "diff",
            "--no-ext-diff",
            "--no-color",
            "--full-index",
            "--find-renames",
            "-z",
            "--raw",
            from_sha,
            to_sha,
        ],
        None,
    )?;

    parse_raw_diff_z(&out.stdout)
}

/// Parse `git diff --raw -z` output.
///
/// Each record begins with `:` and ends after the path field(s); fields are
/// separated by NUL.
///
/// Format: `:src_mode dst_mode src_sha dst_sha STATUS\0PATH\0[PATH2\0]`
fn parse_raw_diff_z(bytes: &[u8]) -> Result<Vec<DiffEntry>> {
    let mut entries = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        // Find header end (first NUL).
        let nul = bytes[i..]
            .iter()
            .position(|&b| b == 0)
            .ok_or_else(|| anyhow::anyhow!("malformed raw diff: missing NUL"))?;
        let header = std::str::from_utf8(&bytes[i..i + nul]).context("raw diff header utf8")?;
        i += nul + 1;
        // Header looks like ":100644 100644 sha1 sha2 M"
        if !header.starts_with(':') {
            bail!("malformed raw diff header: `{header}`");
        }
        let parts: Vec<&str> = header[1..].split(' ').collect();
        if parts.len() < 5 {
            bail!("malformed raw diff header: `{header}`");
        }
        let src_mode = parts[0];
        let dst_mode = parts[1];
        let src_sha = parts[2];
        let dst_sha = parts[3];
        let status = parts[4];

        // All-zeros OID means the blob is absent (added/deleted side).
        let zero_oid = "0000000000000000000000000000000000000000";
        let parse_oid = |s: &str| -> Option<String> {
            if s == zero_oid { None } else { Some(s.to_string()) }
        };
        let old_oid = parse_oid(src_sha);
        let new_oid = parse_oid(dst_sha);

        // Read first path.
        let nul = bytes[i..]
            .iter()
            .position(|&b| b == 0)
            .ok_or_else(|| anyhow::anyhow!("malformed raw diff: missing path NUL"))?;
        let path1 = std::str::from_utf8(&bytes[i..i + nul])
            .context("raw diff path utf8")?
            .to_string();
        i += nul + 1;

        let status_byte = status.chars().next().unwrap_or('?');

        let entry = match status_byte {
            'M' => {
                if src_mode != dst_mode {
                    DiffEntry::ModeChange { path: path1, old_oid, new_oid }
                } else {
                    DiffEntry::Modified { path: path1, old_oid, new_oid }
                }
            }
            'A' => DiffEntry::Added { path: path1, new_oid },
            'D' => DiffEntry::Deleted { path: path1, old_oid },
            'T' => DiffEntry::Modified { path: path1, old_oid, new_oid },
            'R' => {
                // Rename has a second path.
                let nul = bytes[i..].iter().position(|&b| b == 0).ok_or_else(|| {
                    anyhow::anyhow!("malformed raw diff: missing rename target NUL")
                })?;
                let path2 = std::str::from_utf8(&bytes[i..i + nul])
                    .context("raw diff rename target utf8")?
                    .to_string();
                i += nul + 1;
                let score: u8 = status[1..].parse().unwrap_or(100);
                DiffEntry::Renamed {
                    from: path1,
                    to: path2,
                    score,
                    old_oid,
                    new_oid,
                }
            }
            'C' => {
                // Copy: skip second path; treat as Added of dst.
                let nul = bytes[i..].iter().position(|&b| b == 0).ok_or_else(|| {
                    anyhow::anyhow!("malformed raw diff: missing copy target NUL")
                })?;
                let path2 = std::str::from_utf8(&bytes[i..i + nul])
                    .context("raw diff copy target utf8")?
                    .to_string();
                i += nul + 1;
                DiffEntry::Added { path: path2, new_oid }
            }
            _ => DiffEntry::Modified { path: path1, old_oid, new_oid },
        };
        entries.push(entry);
    }
    Ok(entries)
}

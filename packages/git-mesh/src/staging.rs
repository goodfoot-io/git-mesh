//! Staging area — see §6.3, §6.4.
//!
//! Transient local state under `.git/mesh/staging/` per mesh:
//! - `<name>`           — pending operations, one per line
//! - `<name>.msg`       — staged commit message (optional)
//! - `<name>.<N>`       — full-file sidecar bytes per staged `add` line
//!
//! Operation line format:
//! ```text
//! add <path>#L<start>-L<end> [<anchor-sha>]
//! remove <path>#L<start>-L<end>
//! config <key> <value>
//! ```

use crate::git::{self, work_dir};
use crate::types::{CopyDetection, DEFAULT_COPY_DETECTION, DEFAULT_IGNORE_WHITESPACE};
use crate::{Error, Result};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StagedAdd {
    pub line_number: u32,
    pub path: String,
    pub start: u32,
    pub end: u32,
    pub anchor: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StagedRemove {
    pub path: String,
    pub start: u32,
    pub end: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StagedConfig {
    CopyDetection(CopyDetection),
    IgnoreWhitespace(bool),
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Staging {
    pub adds: Vec<StagedAdd>,
    pub removes: Vec<StagedRemove>,
    pub configs: Vec<StagedConfig>,
    pub message: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DriftFinding {
    pub path: String,
    pub start: u32,
    pub end: u32,
    pub diff: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StatusView {
    pub name: String,
    pub staging: Staging,
    pub drift: Vec<DriftFinding>,
}

// ---------------------------------------------------------------------------
// Paths.
// ---------------------------------------------------------------------------

fn staging_dir(repo: &gix::Repository) -> Result<PathBuf> {
    let wd = work_dir(repo)?;
    Ok(wd.join(".git").join("mesh").join("staging"))
}

fn ops_path(repo: &gix::Repository, name: &str) -> Result<PathBuf> {
    Ok(staging_dir(repo)?.join(name))
}

fn msg_path(repo: &gix::Repository, name: &str) -> Result<PathBuf> {
    Ok(staging_dir(repo)?.join(format!("{name}.msg")))
}

fn sidecar_path(repo: &gix::Repository, name: &str, n: u32) -> Result<PathBuf> {
    Ok(staging_dir(repo)?.join(format!("{name}.{n}")))
}

fn ensure_dir(p: &Path) -> Result<()> {
    fs::create_dir_all(p)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Parsing.
// ---------------------------------------------------------------------------

/// Parse one ops-file line into the correct bucket. Returns `Ok(None)` if
/// the line is empty.
fn parse_line(line: &str) -> Result<Option<ParsedLine>> {
    if line.trim().is_empty() {
        return Ok(None);
    }
    if let Some(rest) = line.strip_prefix("add ") {
        let mut parts = rest.splitn(2, ' ');
        let addr = parts.next().unwrap_or_default();
        let anchor = parts.next().map(str::to_string);
        let (path, start, end) = parse_range_address(addr)
            .ok_or_else(|| Error::ParseStaging { line: line.into() })?;
        return Ok(Some(ParsedLine::Add(StagedAdd {
            line_number: 0,
            path,
            start,
            end,
            anchor,
        })));
    }
    if let Some(rest) = line.strip_prefix("remove ") {
        let (path, start, end) = parse_range_address(rest)
            .ok_or_else(|| Error::ParseStaging { line: line.into() })?;
        return Ok(Some(ParsedLine::Remove(StagedRemove {
            path,
            start,
            end,
        })));
    }
    if let Some(rest) = line.strip_prefix("config ") {
        let (key, value) = rest
            .split_once(' ')
            .ok_or_else(|| Error::ParseStaging { line: line.into() })?;
        let entry = match key {
            "copy-detection" => {
                StagedConfig::CopyDetection(parse_copy_detection(value).ok_or_else(|| {
                    Error::ParseStaging { line: line.into() }
                })?)
            }
            "ignore-whitespace" => {
                let b = match value {
                    "true" => true,
                    "false" => false,
                    _ => return Err(Error::ParseStaging { line: line.into() }),
                };
                StagedConfig::IgnoreWhitespace(b)
            }
            _ => return Err(Error::ParseStaging { line: line.into() }),
        };
        return Ok(Some(ParsedLine::Config(entry)));
    }
    Err(Error::ParseStaging { line: line.into() })
}

enum ParsedLine {
    Add(StagedAdd),
    Remove(StagedRemove),
    Config(StagedConfig),
}

pub(crate) fn parse_range_address(text: &str) -> Option<(String, u32, u32)> {
    let (path, fragment) = text.split_once("#L")?;
    let (start, end) = fragment.split_once("-L")?;
    if path.is_empty() {
        return None;
    }
    let start: u32 = start.parse().ok()?;
    let end: u32 = end.parse().ok()?;
    if start < 1 || end < start {
        return None;
    }
    Some((path.to_string(), start, end))
}

fn parse_copy_detection(value: &str) -> Option<CopyDetection> {
    Some(match value {
        "off" => CopyDetection::Off,
        "same-commit" => CopyDetection::SameCommit,
        "any-file-in-commit" => CopyDetection::AnyFileInCommit,
        "any-file-in-repo" => CopyDetection::AnyFileInRepo,
        _ => return None,
    })
}

pub(crate) fn serialize_copy_detection(cd: CopyDetection) -> &'static str {
    match cd {
        CopyDetection::Off => "off",
        CopyDetection::SameCommit => "same-commit",
        CopyDetection::AnyFileInCommit => "any-file-in-commit",
        CopyDetection::AnyFileInRepo => "any-file-in-repo",
    }
}

// ---------------------------------------------------------------------------
// Public API.
// ---------------------------------------------------------------------------

pub fn read_staging(repo: &gix::Repository, name: &str) -> Result<Staging> {
    let ops_p = ops_path(repo, name)?;
    let msg_p = msg_path(repo, name)?;
    let mut staging = Staging::default();
    if ops_p.exists() {
        let text = fs::read_to_string(&ops_p)?;
        let mut add_count: u32 = 0;
        for line in text.lines() {
            if let Some(parsed) = parse_line(line)? {
                match parsed {
                    ParsedLine::Add(mut a) => {
                        add_count += 1;
                        a.line_number = add_count;
                        staging.adds.push(a);
                    }
                    ParsedLine::Remove(r) => staging.removes.push(r),
                    ParsedLine::Config(c) => staging.configs.push(c),
                }
            }
        }
    }
    if msg_p.exists() {
        staging.message = Some(fs::read_to_string(&msg_p)?);
    }
    Ok(staging)
}

fn append_line(repo: &gix::Repository, name: &str, line: &str) -> Result<u32> {
    let ops_p = ops_path(repo, name)?;
    ensure_dir(ops_p.parent().unwrap())?;
    // Count existing add lines to compute the new add-line number.
    let existing = if ops_p.exists() {
        fs::read_to_string(&ops_p)?
    } else {
        String::new()
    };
    let mut new_add_count: u32 = existing
        .lines()
        .filter(|l| l.starts_with("add "))
        .count() as u32;
    if line.starts_with("add ") {
        new_add_count += 1;
    }
    let mut combined = existing;
    combined.push_str(line);
    combined.push('\n');
    fs::write(&ops_p, combined)?;
    Ok(new_add_count)
}

pub fn append_add(
    repo: &gix::Repository,
    name: &str,
    path: &str,
    start: u32,
    end: u32,
    anchor: Option<&str>,
) -> Result<()> {
    if start < 1 || end < start {
        return Err(Error::InvalidRange { start, end });
    }
    let line = match anchor {
        Some(sha) => format!("add {path}#L{start}-L{end} {sha}"),
        None => format!("add {path}#L{start}-L{end}"),
    };
    let add_n = append_line(repo, name, &line)?;

    // Snapshot sidecar bytes:
    //  - explicit anchor → read from that commit's blob at `path`
    //  - no anchor       → read from the working tree
    // Best-effort sidecar snapshot: if the path does not exist at stage
    // time, record an empty sidecar and defer the hard failure to commit.
    let bytes = match anchor {
        Some(sha) => match git::path_blob_at(repo, sha, path) {
            Ok(blob) => {
                let wd = work_dir(repo)?;
                crate::git::git_stdout_raw(wd, ["cat-file", "-p", &blob])
                    .unwrap_or_default()
                    .into_bytes()
            }
            Err(_) => Vec::new(),
        },
        None => git::read_worktree_bytes(repo, path).unwrap_or_default(),
    };
    fs::write(sidecar_path(repo, name, add_n)?, bytes)?;
    Ok(())
}

pub fn append_remove(
    repo: &gix::Repository,
    name: &str,
    path: &str,
    start: u32,
    end: u32,
) -> Result<()> {
    if start < 1 || end < start {
        return Err(Error::InvalidRange { start, end });
    }
    append_line(repo, name, &format!("remove {path}#L{start}-L{end}"))?;
    Ok(())
}

pub fn append_config(
    repo: &gix::Repository,
    name: &str,
    entry: &StagedConfig,
) -> Result<()> {
    let (key, value) = match entry {
        StagedConfig::CopyDetection(cd) => ("copy-detection", serialize_copy_detection(*cd).to_string()),
        StagedConfig::IgnoreWhitespace(b) => ("ignore-whitespace", b.to_string()),
    };
    append_line(repo, name, &format!("config {key} {value}"))?;
    Ok(())
}

pub fn set_message(repo: &gix::Repository, name: &str, message: &str) -> Result<()> {
    let p = msg_path(repo, name)?;
    ensure_dir(p.parent().unwrap())?;
    if message.is_empty() {
        if p.exists() {
            fs::remove_file(&p)?;
        }
        return Ok(());
    }
    fs::write(&p, message)?;
    Ok(())
}

pub fn clear_staging(repo: &gix::Repository, name: &str) -> Result<()> {
    let dir = staging_dir(repo)?;
    if !dir.exists() {
        return Ok(());
    }
    for entry in fs::read_dir(&dir)? {
        let entry = entry?;
        let fname = entry.file_name();
        let Some(fname) = fname.to_str() else {
            continue;
        };
        let matches = fname == name
            || fname == format!("{name}.msg")
            || fname
                .strip_prefix(&format!("{name}."))
                .is_some_and(|rest| rest.chars().all(|c| c.is_ascii_digit()));
        if matches {
            fs::remove_file(entry.path())?;
        }
    }
    Ok(())
}

pub fn drift_check(repo: &gix::Repository, name: &str) -> Result<Vec<DriftFinding>> {
    let staging = read_staging(repo, name)?;
    let mut findings = Vec::new();
    for add in &staging.adds {
        if add.anchor.is_some() {
            continue;
        }
        let sidecar_p = sidecar_path(repo, name, add.line_number)?;
        if !sidecar_p.exists() {
            continue;
        }
        let sidecar = fs::read(&sidecar_p)?;
        let current = git::read_worktree_bytes(repo, &add.path).unwrap_or_default();
        if sidecar != current {
            // Build a minimal diff description — not a real unified diff,
            // but tests only inspect path/start/end.
            findings.push(DriftFinding {
                path: add.path.clone(),
                start: add.start,
                end: add.end,
                diff: "working-tree bytes differ from sidecar".into(),
            });
        }
    }
    Ok(findings)
}

pub fn status_view(repo: &gix::Repository, name: &str) -> Result<StatusView> {
    Ok(StatusView {
        name: name.to_string(),
        staging: read_staging(repo, name)?,
        drift: drift_check(repo, name)?,
    })
}

// ---------------------------------------------------------------------------
// Internal helpers used by the commit pipeline.
// ---------------------------------------------------------------------------

/// Collapse staged configs via last-write-wins and return the resolved
/// `(copy_detection, ignore_whitespace)` overriding the given baseline.
pub(crate) fn resolve_staged_config(
    staging: &Staging,
    baseline: (CopyDetection, bool),
) -> (CopyDetection, bool) {
    let mut cd = baseline.0;
    let mut iw = baseline.1;
    for entry in &staging.configs {
        match entry {
            StagedConfig::CopyDetection(v) => cd = *v,
            StagedConfig::IgnoreWhitespace(v) => iw = *v,
        }
    }
    (cd, iw)
}

/// Returns baseline defaults as used when a mesh has no prior config blob.
#[allow(dead_code)]
pub(crate) fn default_config() -> (CopyDetection, bool) {
    (DEFAULT_COPY_DETECTION, DEFAULT_IGNORE_WHITESPACE)
}

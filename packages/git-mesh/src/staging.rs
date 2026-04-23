//! Staging area — see §6.3, §6.4.
//!
//! Transient local state under `.git/mesh/staging/` per mesh:
//! - `<name>`           — pending operations, one per line
//! - `<name>.msg`       — staged commit message (optional)
//! - `<name>.<N>`       — full-file sidecar bytes per staged `add` line
//! - `<name>.<N>.meta`  — JSON sidecar metadata (freshness stamp +
//!   acknowledged `range_id`); see plan §B2 / §D3.
//!
//! Operation line format:
//! ```text
//! add <path>#L<start>-L<end>[\t<anchor-sha>]
//! add <path>[\t<anchor-sha>]                    # whole-file pin (D2)
//! remove <path>#L<start>-L<end>
//! remove <path>                                 # whole-file pin (D2)
//! config <key> <value>
//! ```

use crate::git::{self, work_dir};
use crate::types::{
    CopyDetection, DEFAULT_COPY_DETECTION, DEFAULT_IGNORE_WHITESPACE, NormalizationStamp,
    RangeExtent,
};
use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

const ADD_ANCHOR_SEPARATOR: char = '\t';

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StagedAdd {
    /// `N` in `<mesh>.<N>` — sidecar/file ordering key (1-based).
    pub line_number: u32,
    pub path: String,
    pub extent: RangeExtent,
    pub anchor: Option<String>,
}

impl StagedAdd {
    /// Convenience: line-range start (or `0` for whole-file).
    pub fn start(&self) -> u32 {
        match self.extent {
            RangeExtent::Lines { start, .. } => start,
            RangeExtent::Whole => 0,
        }
    }
    /// Convenience: line-range end (or `0` for whole-file).
    pub fn end(&self) -> u32 {
        match self.extent {
            RangeExtent::Lines { end, .. } => end,
            RangeExtent::Whole => 0,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PreparedAdd {
    pub path: String,
    pub extent: RangeExtent,
    pub anchor: Option<String>,
    pub bytes: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StagedRemove {
    pub path: String,
    pub extent: RangeExtent,
}

impl StagedRemove {
    pub fn start(&self) -> u32 {
        match self.extent {
            RangeExtent::Lines { start, .. } => start,
            RangeExtent::Whole => 0,
        }
    }
    pub fn end(&self) -> u32 {
        match self.extent {
            RangeExtent::Lines { end, .. } => end,
            RangeExtent::Whole => 0,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StagedConfig {
    CopyDetection(CopyDetection),
    IgnoreWhitespace(bool),
}

/// A single staged mesh operation in `.git/mesh/staging/<mesh>`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StagedOp {
    Add(StagedAdd),
    Remove(StagedRemove),
    Config(StagedConfig),
    Message(String),
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

/// JSON sidecar for `<mesh>.<N>` capturing the normalization-version
/// stamp and the range_id this staged op acknowledges (if any). Plan
/// §B2 / §D3.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SidecarMeta {
    /// `gitattributes` SHA-1 + filter-driver-list hash captured at
    /// `git mesh add` time.
    pub stamp: NormalizationStamp,
    /// The `range_id` this staged op intends to ack, if any. Resolved
    /// at `git mesh add` time by looking up the active mesh range at
    /// `(path, extent)`.
    pub range_id: Option<String>,
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

pub(crate) fn sidecar_path(repo: &gix::Repository, name: &str, n: u32) -> Result<PathBuf> {
    Ok(staging_dir(repo)?.join(format!("{name}.{n}")))
}

pub(crate) fn sidecar_meta_path(repo: &gix::Repository, name: &str, n: u32) -> Result<PathBuf> {
    Ok(staging_dir(repo)?.join(format!("{name}.{n}.meta")))
}

fn ensure_dir(p: &Path) -> Result<()> {
    fs::create_dir_all(p)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Address parser (line-range vs. whole-file).
// ---------------------------------------------------------------------------

/// Parse a `<path>#L<start>-L<end>` line-range address, or a bare
/// `<path>` whole-file address. Returns `None` on malformed line-range
/// fragments — callers should reject those at the CLI boundary.
pub fn parse_address(text: &str) -> Option<(String, RangeExtent)> {
    if let Some((path, fragment)) = text.split_once("#L") {
        let (start, end) = fragment.split_once("-L")?;
        if path.is_empty() {
            return None;
        }
        let start: u32 = start.parse().ok()?;
        let end: u32 = end.parse().ok()?;
        if start < 1 || end < start {
            return None;
        }
        return Some((path.to_string(), RangeExtent::Lines { start, end }));
    }
    if text.is_empty() {
        return None;
    }
    Some((text.to_string(), RangeExtent::Whole))
}

/// Back-compat helper for the existing line-range tests.
#[allow(dead_code)]
pub(crate) fn parse_range_address(text: &str) -> Option<(String, u32, u32)> {
    match parse_address(text)? {
        (p, RangeExtent::Lines { start, end }) => Some((p, start, end)),
        (_, RangeExtent::Whole) => None,
    }
}

// ---------------------------------------------------------------------------
// Parsing.
// ---------------------------------------------------------------------------

fn parse_line(line: &str) -> Result<Option<ParsedLine>> {
    if line.trim().is_empty() {
        return Ok(None);
    }
    if let Some(rest) = line.strip_prefix("add ") {
        let (addr, anchor) = match rest.split_once(ADD_ANCHOR_SEPARATOR) {
            Some((addr, anchor)) => (addr, Some(anchor.to_string())),
            None => (rest, None),
        };
        let (path, extent) =
            parse_address(addr).ok_or_else(|| Error::ParseStaging { line: line.into() })?;
        return Ok(Some(ParsedLine::Add(StagedAdd {
            line_number: 0,
            path,
            extent,
            anchor,
        })));
    }
    if let Some(rest) = line.strip_prefix("remove ") {
        let (path, extent) =
            parse_address(rest).ok_or_else(|| Error::ParseStaging { line: line.into() })?;
        return Ok(Some(ParsedLine::Remove(StagedRemove { path, extent })));
    }
    if let Some(rest) = line.strip_prefix("config ") {
        let (key, value) = rest
            .split_once(' ')
            .ok_or_else(|| Error::ParseStaging { line: line.into() })?;
        let entry = match key {
            "copy-detection" => StagedConfig::CopyDetection(
                parse_copy_detection(value)
                    .ok_or_else(|| Error::ParseStaging { line: line.into() })?,
            ),
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

fn format_address(path: &str, extent: RangeExtent) -> String {
    match extent {
        RangeExtent::Lines { start, end } => format!("{path}#L{start}-L{end}"),
        RangeExtent::Whole => path.to_string(),
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

/// Snapshot all staged ops in canonical order (matches the on-disk file
/// line order). Used by the engine to populate `PendingFinding`s.
pub fn read_staged_ops(repo: &gix::Repository, name: &str) -> Result<Vec<StagedOp>> {
    let ops_p = ops_path(repo, name)?;
    let mut out: Vec<StagedOp> = Vec::new();
    if ops_p.exists() {
        let text = fs::read_to_string(&ops_p)?;
        let mut add_count: u32 = 0;
        for line in text.lines() {
            if let Some(parsed) = parse_line(line)? {
                match parsed {
                    ParsedLine::Add(mut a) => {
                        add_count += 1;
                        a.line_number = add_count;
                        out.push(StagedOp::Add(a));
                    }
                    ParsedLine::Remove(r) => out.push(StagedOp::Remove(r)),
                    ParsedLine::Config(c) => out.push(StagedOp::Config(c)),
                }
            }
        }
    }
    let msg_p = msg_path(repo, name)?;
    if msg_p.exists() {
        let body = fs::read_to_string(&msg_p)?;
        out.push(StagedOp::Message(body));
    }
    Ok(out)
}

fn append_line(repo: &gix::Repository, name: &str, line: &str) -> Result<u32> {
    let ops_p = ops_path(repo, name)?;
    ensure_dir(ops_p.parent().unwrap())?;
    let existing = if ops_p.exists() {
        fs::read_to_string(&ops_p)?
    } else {
        String::new()
    };
    let mut new_add_count: u32 = existing.lines().filter(|l| l.starts_with("add ")).count() as u32;
    if line.starts_with("add ") {
        new_add_count += 1;
    }
    let mut combined = existing;
    combined.push_str(line);
    combined.push('\n');
    fs::write(&ops_p, combined)?;
    Ok(new_add_count)
}

fn validate_staging_path(path: &str) -> Result<()> {
    if path.is_empty() {
        return Err(Error::Parse("range path must not be empty".into()));
    }
    if let Some(bad) = path.chars().find(|c| matches!(c, '\t' | '\n' | '\0')) {
        return Err(Error::Parse(format!(
            "range path contains unsupported control character `{}`",
            bad.escape_debug()
        )));
    }
    Ok(())
}

fn count_lines(bytes: &[u8]) -> u32 {
    String::from_utf8_lossy(bytes).lines().count() as u32
}

fn validate_add_target(
    repo: &gix::Repository,
    path: &str,
    extent: RangeExtent,
    anchor: Option<&str>,
) -> Result<Vec<u8>> {
    validate_staging_path(path)?;
    let bytes = match anchor {
        Some(commit) => {
            // Whole-file submodule gitlinks: no blob to read; treat as empty.
            match git::path_blob_at(repo, commit, path) {
                Ok(blob) => git::read_blob_bytes(repo, &blob).unwrap_or_default(),
                Err(_) if matches!(extent, RangeExtent::Whole) => Vec::new(),
                Err(e) => return Err(e),
            }
        }
        None => {
            // For whole-file pins, allow missing worktree entries only
            // when the path resolves to a tree entry at HEAD (e.g.
            // submodule gitlinks). Otherwise reject — there is nothing
            // to anchor on.
            match git::read_worktree_bytes(repo, path) {
                Ok(b) => b,
                Err(_) if matches!(extent, RangeExtent::Whole) => {
                    let head = git::head_oid(repo)?;
                    if !path_exists_in_tree(repo, &head, path) {
                        return Err(Error::Git(format!(
                            "path not found in worktree or HEAD: {path}"
                        )));
                    }
                    Vec::new()
                }
                Err(e) => return Err(e),
            }
        }
    };
    if let RangeExtent::Lines { start, end } = extent {
        if start < 1 || end < start {
            return Err(Error::InvalidRange { start, end });
        }
        let line_count = count_lines(&bytes);
        if end > line_count {
            return Err(Error::InvalidRange { start, end });
        }
    }
    Ok(bytes)
}

pub(crate) fn prepare_add(
    repo: &gix::Repository,
    path: &str,
    extent: RangeExtent,
    anchor: Option<&str>,
) -> Result<PreparedAdd> {
    Ok(PreparedAdd {
        path: path.to_string(),
        extent,
        anchor: anchor.map(str::to_string),
        bytes: validate_add_target(repo, path, extent, anchor)?,
    })
}

pub(crate) fn append_prepared_add(
    repo: &gix::Repository,
    name: &str,
    add: &PreparedAdd,
    range_id: Option<String>,
) -> Result<()> {
    let addr = format_address(&add.path, add.extent);
    let line = match add.anchor.as_deref() {
        Some(sha) => format!("add {addr}{ADD_ANCHOR_SEPARATOR}{sha}"),
        None => format!("add {addr}"),
    };
    let add_n = append_line(repo, name, &line)?;
    fs::write(sidecar_path(repo, name, add_n)?, &add.bytes)?;
    // Write sidecar metadata: stamp + range_id.
    let stamp = crate::types::current_normalization_stamp(repo).unwrap_or_default();
    let meta = SidecarMeta { stamp, range_id };
    let meta_json = serde_json::to_vec_pretty(&meta)
        .map_err(|e| Error::Parse(format!("serialize sidecar meta: {e}")))?;
    fs::write(sidecar_meta_path(repo, name, add_n)?, meta_json)?;
    Ok(())
}

/// Append a line-range add. Convenience kept stable across slices.
pub fn append_add(
    repo: &gix::Repository,
    name: &str,
    path: &str,
    start: u32,
    end: u32,
    anchor: Option<&str>,
) -> Result<()> {
    let add = prepare_add(repo, path, RangeExtent::Lines { start, end }, anchor)?;
    append_prepared_add(repo, name, &add, None)
}

/// Append a whole-file add (plan §D2). The sidecar carries the bytes at
/// `anchor`, or worktree bytes when `anchor` is unset.
pub fn append_add_whole(
    repo: &gix::Repository,
    name: &str,
    path: &str,
    anchor: Option<&str>,
) -> Result<()> {
    let add = prepare_add(repo, path, RangeExtent::Whole, anchor)?;
    append_prepared_add(repo, name, &add, None)
}

pub fn append_remove(
    repo: &gix::Repository,
    name: &str,
    path: &str,
    start: u32,
    end: u32,
) -> Result<()> {
    validate_staging_path(path)?;
    if start < 1 || end < start {
        return Err(Error::InvalidRange { start, end });
    }
    append_line(repo, name, &format!("remove {path}#L{start}-L{end}"))?;
    Ok(())
}

pub fn append_remove_whole(repo: &gix::Repository, name: &str, path: &str) -> Result<()> {
    validate_staging_path(path)?;
    append_line(repo, name, &format!("remove {path}"))?;
    Ok(())
}

pub fn append_config(repo: &gix::Repository, name: &str, entry: &StagedConfig) -> Result<()> {
    let (key, value) = match entry {
        StagedConfig::CopyDetection(cd) => {
            ("copy-detection", serialize_copy_detection(*cd).to_string())
        }
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
            || fname.strip_prefix(&format!("{name}.")).is_some_and(|rest| {
                // `<N>` (sidecar) or `<N>.meta` (sidecar metadata).
                let stripped = rest.strip_suffix(".meta").unwrap_or(rest);
                !stripped.is_empty() && stripped.chars().all(|c| c.is_ascii_digit())
            });
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
            let (start, end) = match add.extent {
                RangeExtent::Lines { start, end } => (start, end),
                RangeExtent::Whole => (0, 0),
            };
            findings.push(DriftFinding {
                path: add.path.clone(),
                start,
                end,
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

#[allow(dead_code)]
pub(crate) fn default_config() -> (CopyDetection, bool) {
    (DEFAULT_COPY_DETECTION, DEFAULT_IGNORE_WHITESPACE)
}

fn path_exists_in_tree(repo: &gix::Repository, commit_sha: &str, path: &str) -> bool {
    let Some(workdir) = repo.workdir() else {
        return false;
    };
    let out = std::process::Command::new("git")
        .current_dir(workdir)
        .args(["ls-tree", commit_sha, "--", path])
        .output();
    matches!(out, Ok(o) if o.status.success() && !o.stdout.is_empty())
}

/// Read the sidecar metadata file (stamp + range_id) for `<mesh>.<N>`.
/// Returns `None` if the file is missing or malformed (treat as
/// "captured under unknown rules — re-normalize before comparing").
pub(crate) fn read_sidecar_meta(
    repo: &gix::Repository,
    name: &str,
    n: u32,
) -> Option<SidecarMeta> {
    let p = sidecar_meta_path(repo, name, n).ok()?;
    let bytes = fs::read(&p).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Update the `range_id` on an existing sidecar's `.meta` file. Used by
/// the dedup pass when a new add ends up shadowing an existing range.
#[allow(dead_code)]
pub(crate) fn update_sidecar_range_id(
    repo: &gix::Repository,
    name: &str,
    n: u32,
    range_id: Option<String>,
) -> Result<()> {
    let p = sidecar_meta_path(repo, name, n)?;
    let mut meta = read_sidecar_meta(repo, name, n).unwrap_or_else(|| SidecarMeta {
        stamp: NormalizationStamp::default(),
        range_id: None,
    });
    meta.range_id = range_id;
    let bytes = serde_json::to_vec_pretty(&meta)
        .map_err(|e| Error::Parse(format!("serialize sidecar meta: {e}")))?;
    fs::write(&p, bytes)?;
    Ok(())
}

//! Staging area — see §6.3, §6.4.
//!
//! Transient local state under `.git/mesh/staging/` per mesh:
//! - `<name>`           — pending operations, one per line
//! - `<name>.why`       — staged why text (optional)
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
    /// Line count of the captured (post-filter) bytes. Pinned at
    /// stage-time so the commit pipeline does not re-read the worktree
    /// or worse, fall back to the raw blob (e.g. an LFS pointer) and
    /// trip the bounds check. Slice 1 of the review plan.
    pub line_count: u32,
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
    Why(String),
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Staging {
    pub adds: Vec<StagedAdd>,
    pub removes: Vec<StagedRemove>,
    pub configs: Vec<StagedConfig>,
    pub why: Option<String>,
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
    /// Line count of the captured (post-filter) sidecar bytes. The
    /// commit pipeline reads this for line-range bounds checks so it
    /// never re-validates a `filter=lfs` path against the raw pointer
    /// blob (Slice 1).
    pub line_count: u32,
    /// Lowercase hex SHA-256 of the sidecar bytes at write time. Slice 4
    /// of the review plan — every sidecar read site verifies this hash
    /// before consuming the bytes; a mismatch surfaces as
    /// `Error::SidecarTampered` (commit) or `PendingDrift::SidecarTampered`
    /// (resolver).
    ///
    /// Greenfield: there is no migration path. An older (pre-Slice-4)
    /// `.meta` file deserializes with an empty string here and is
    /// treated as tampered (`<fail-closed>`).
    #[serde(default)]
    pub content_sha256: String,
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

fn why_path(repo: &gix::Repository, name: &str) -> Result<PathBuf> {
    Ok(staging_dir(repo)?.join(format!("{name}.why")))
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
    let why_p = why_path(repo, name)?;
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
    if why_p.exists() {
        staging.why = Some(fs::read_to_string(&why_p)?);
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
    let why_p = why_path(repo, name)?;
    if why_p.exists() {
        let body = fs::read_to_string(&why_p)?;
        out.push(StagedOp::Why(body));
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
        if std::str::from_utf8(&bytes).is_err() {
            return Err(Error::Parse(format!(
                "line-range pin rejected on binary path: {path}"
            )));
        }
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
    let bytes = validate_add_target(repo, path, extent, anchor)?;
    let line_count = count_lines(&bytes);
    Ok(PreparedAdd {
        path: path.to_string(),
        extent,
        anchor: anchor.map(str::to_string),
        bytes,
        line_count,
    })
}

pub(crate) fn append_prepared_add(
    repo: &gix::Repository,
    name: &str,
    add: &PreparedAdd,
    range_id: Option<String>,
) -> Result<()> {
    // Slice 3: last-write-wins for `(path, extent)`. Strip any existing
    // staged add at the same address (and its sidecar + meta files),
    // renumbering trailing sidecars so that parse-order ↔ on-disk `<N>`
    // alignment is preserved. Then append the new add as usual.
    supersede_existing_adds(repo, name, &add.path, add.extent)?;

    let addr = format_address(&add.path, add.extent);
    let line = match add.anchor.as_deref() {
        Some(sha) => format!("add {addr}{ADD_ANCHOR_SEPARATOR}{sha}"),
        None => format!("add {addr}"),
    };
    let add_n = append_line(repo, name, &line)?;
    fs::write(sidecar_path(repo, name, add_n)?, &add.bytes)?;
    // Write sidecar metadata: stamp + range_id + line_count + content_sha256.
    let stamp = crate::types::current_normalization_stamp(repo).unwrap_or_default();
    let meta = SidecarMeta {
        stamp,
        range_id,
        line_count: add.line_count,
        content_sha256: sha256_hex(&add.bytes),
    };
    let meta_json = serde_json::to_vec_pretty(&meta)
        .map_err(|e| Error::Parse(format!("serialize sidecar meta: {e}")))?;
    fs::write(sidecar_meta_path(repo, name, add_n)?, meta_json)?;
    Ok(())
}

/// Drop any staged add lines matching `(path, extent)` from the ops file
/// for `mesh`, delete their sidecars, and renumber trailing sidecars to
/// keep `<mesh>.<N>` dense.
///
/// Slice 3 of the review plan (`append_prepared_add` cleanup). The
/// renumbering is required because `read_staging` assigns
/// `StagedAdd::line_number` purely by parse position; without renaming
/// the on-disk sidecars to match, the engine would read the wrong
/// sidecar/meta for adds that follow the deleted slot.
fn supersede_existing_adds(
    repo: &gix::Repository,
    name: &str,
    path: &str,
    extent: RangeExtent,
) -> Result<()> {
    let ops_p = ops_path(repo, name)?;
    if !ops_p.exists() {
        return Ok(());
    }
    let text = fs::read_to_string(&ops_p)?;
    // Collect, per source line, whether it is an add and (if so) whether
    // it matches the supersede target. Keep ordering identical to the
    // file so we can safely rewrite.
    let mut new_lines: Vec<String> = Vec::new();
    // For each kept add we record its old `<N>` (1-based among adds in
    // the prior file) so we can rename its sidecar files to a fresh
    // dense `<N>` matching the new ops file.
    let mut keep_old_n: Vec<u32> = Vec::new();
    let mut dropped_old_n: Vec<u32> = Vec::new();
    let mut add_n: u32 = 0;
    for line in text.lines() {
        if let Some(parsed) = parse_line(line)? {
            match parsed {
                ParsedLine::Add(a) => {
                    add_n += 1;
                    if a.path == path && a.extent == extent {
                        dropped_old_n.push(add_n);
                        continue;
                    }
                    keep_old_n.push(add_n);
                    new_lines.push(line.to_string());
                }
                ParsedLine::Remove(_) | ParsedLine::Config(_) => {
                    new_lines.push(line.to_string());
                }
            }
        }
    }
    if dropped_old_n.is_empty() {
        return Ok(());
    }

    // Delete dropped sidecars first so the rename pass cannot collide.
    for n in &dropped_old_n {
        let _ = fs::remove_file(sidecar_path(repo, name, *n)?);
        let _ = fs::remove_file(sidecar_meta_path(repo, name, *n)?);
    }

    // Renumber kept sidecars from `old_n` → `new_n` (1-based). Iterate
    // ascending so renames never overwrite an old_n that is still
    // pending (new_n <= old_n always when dropping a lower-index slot).
    for (i, old_n) in keep_old_n.iter().enumerate() {
        let new_n = (i as u32) + 1;
        if new_n == *old_n {
            continue;
        }
        let from = sidecar_path(repo, name, *old_n)?;
        let to = sidecar_path(repo, name, new_n)?;
        if from.exists() {
            fs::rename(&from, &to)?;
        }
        let from_meta = sidecar_meta_path(repo, name, *old_n)?;
        let to_meta = sidecar_meta_path(repo, name, new_n)?;
        if from_meta.exists() {
            fs::rename(&from_meta, &to_meta)?;
        }
    }

    // Rewrite ops file. Preserve trailing newline semantics from
    // `append_line` (every line terminated with `\n`).
    let mut combined = String::with_capacity(text.len());
    for line in &new_lines {
        combined.push_str(line);
        combined.push('\n');
    }
    fs::write(&ops_p, combined)?;
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

pub fn set_why(repo: &gix::Repository, name: &str, why: &str) -> Result<()> {
    let p = why_path(repo, name)?;
    ensure_dir(p.parent().unwrap())?;
    if why.is_empty() {
        if p.exists() {
            fs::remove_file(&p)?;
        }
        return Ok(());
    }
    fs::write(&p, why)?;
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
            || fname == format!("{name}.why")
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
    matches!(
        crate::git::tree_entry_at(repo, commit_sha, std::path::Path::new(path)),
        Ok(Some(_))
    )
}

/// Read the sidecar metadata file (stamp + range_id) for `<mesh>.<N>`.
/// Returns `None` if the file is missing or malformed (treat as
/// "captured under unknown rules — re-normalize before comparing").
pub(crate) fn read_sidecar_meta(repo: &gix::Repository, name: &str, n: u32) -> Option<SidecarMeta> {
    let p = sidecar_meta_path(repo, name, n).ok()?;
    let bytes = fs::read(&p).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Lowercase hex SHA-256 of `bytes`. Slice 4 of the review plan.
pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(bytes);
    format!("{:x}", h.finalize())
}

/// Outcome of reading a sidecar with integrity verification (Slice 4).
///
/// `Ok(bytes)` — the sidecar exists, the meta records a non-empty
/// `content_sha256`, and `sha256(bytes) == meta.content_sha256`.
///
/// `Err(SidecarVerifyError::Tampered)` — the sidecar exists but its
/// content does not match the recorded hash, or the meta file is
/// missing/unreadable, or the meta records an empty hash. Per
/// `<fail-closed>`, all three collapse to "tampered" — there is no
/// passthrough for unverifiable sidecars.
///
/// `Err(SidecarVerifyError::Missing)` — the sidecar bytes file itself
/// is absent (separate from tampering; callers may want to skip
/// rather than fail-loud on a missing sidecar, depending on context).
pub(crate) fn read_sidecar_verified(
    repo: &gix::Repository,
    name: &str,
    n: u32,
) -> std::result::Result<Vec<u8>, SidecarVerifyError> {
    let p = sidecar_path(repo, name, n).map_err(|_| SidecarVerifyError::Missing)?;
    let bytes = match fs::read(&p) {
        Ok(b) => b,
        Err(_) => return Err(SidecarVerifyError::Missing),
    };
    let meta = match read_sidecar_meta(repo, name, n) {
        Some(m) => m,
        None => return Err(SidecarVerifyError::Tampered),
    };
    if meta.content_sha256.is_empty() {
        return Err(SidecarVerifyError::Tampered);
    }
    if sha256_hex(&bytes) != meta.content_sha256 {
        return Err(SidecarVerifyError::Tampered);
    }
    Ok(bytes)
}

#[derive(Debug)]
pub(crate) enum SidecarVerifyError {
    Missing,
    Tampered,
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
        line_count: 0,
        content_sha256: String::new(),
    });
    meta.range_id = range_id;
    let bytes = serde_json::to_vec_pretty(&meta)
        .map_err(|e| Error::Parse(format!("serialize sidecar meta: {e}")))?;
    fs::write(&p, bytes)?;
    Ok(())
}

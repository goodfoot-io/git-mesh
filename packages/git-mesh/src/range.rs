//! Range blob I/O (v1) — see §3.1, §4.1, §6.1.
//!
//! A Range is an immutable blob at `refs/ranges/v1/<rangeId>` with a
//! commit-object-style text format:
//!
//! ```text
//! anchor <sha>
//! created <iso-8601>
//! range <start> <end> <blob>\t<path>
//! ```

use crate::git::{self, work_dir};
use crate::types::{Range, RangeExtent};
use crate::{Error, Result};
use chrono::Utc;
use uuid::Uuid;

/// Canonical ref path for a range id.
pub fn range_ref_path(range_id: &str) -> String {
    format!("refs/ranges/v1/{range_id}")
}

fn validate_path(path: &str) -> Result<()> {
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

/// Create a Range record (line-range), write the blob, and create
/// `refs/ranges/v1/<uuid>`.
pub fn create_range(
    repo: &gix::Repository,
    anchor_sha: &str,
    path: &str,
    start: u32,
    end: u32,
) -> Result<String> {
    create_range_with_extent(repo, anchor_sha, path, RangeExtent::Lines { start, end })
}

/// Create a Range record at the given extent (line-range or whole-file).
pub fn create_range_with_extent(
    repo: &gix::Repository,
    anchor_sha: &str,
    path: &str,
    extent: RangeExtent,
) -> Result<String> {
    create_range_with_extent_inner(repo, anchor_sha, path, extent, true)
}

/// Variant used by the mesh commit pipeline (Slice 1). The pipeline has
/// already validated line bounds against the captured sidecar
/// `line_count`, which is the post-filter source of truth for
/// `filter=lfs` paths. Re-checking against the raw blob here would
/// re-introduce the bug fixed in `mesh/commit.rs` (the LFS pointer is
/// only ~3 lines).
pub(crate) fn create_range_with_extent_skipping_blob_bounds(
    repo: &gix::Repository,
    anchor_sha: &str,
    path: &str,
    extent: RangeExtent,
) -> Result<String> {
    create_range_with_extent_inner(repo, anchor_sha, path, extent, false)
}

fn create_range_with_extent_inner(
    repo: &gix::Repository,
    anchor_sha: &str,
    path: &str,
    extent: RangeExtent,
    check_blob_bounds: bool,
) -> Result<String> {
    validate_path(path)?;
    if let RangeExtent::Lines { start, end } = extent
        && (start < 1 || end < start)
    {
        return Err(Error::InvalidRange { start, end });
    }
    let _wd = work_dir(repo)?;
    if repo.rev_parse_single(anchor_sha).is_err() {
        return Err(Error::Unreachable {
            anchor_sha: anchor_sha.to_string(),
        });
    }
    // For whole-file submodule gitlink pins, the path resolves to a tree
    // entry with mode 160000 — `path_blob_at` will fail. Tolerate that
    // and store the gitlink SHA via `git ls-tree` instead.
    let blob = match git::path_blob_at(repo, anchor_sha, path) {
        Ok(b) => b,
        Err(_) if matches!(extent, RangeExtent::Whole) => {
            gitlink_sha_at(repo, anchor_sha, path).unwrap_or_default()
        }
        Err(e) => return Err(e),
    };
    if check_blob_bounds
        && let RangeExtent::Lines { start, end } = extent
        && !blob.is_empty()
    {
        let line_count = git::blob_line_count(repo, &blob)?;
        if end > line_count {
            return Err(Error::InvalidRange { start, end });
        }
    }
    let range = Range {
        anchor_sha: anchor_sha.to_string(),
        created_at: Utc::now().to_rfc3339(),
        path: path.to_string(),
        extent,
        blob,
    };
    let blob_oid = git::write_blob_bytes(repo, serialize_range(&range).as_bytes())?;
    let id = Uuid::new_v4().to_string();
    git::update_ref_cas(repo, &range_ref_path(&id), &blob_oid, None)?;
    Ok(id)
}

fn gitlink_sha_at(repo: &gix::Repository, commit_sha: &str, path: &str) -> Option<String> {
    let (_mode, oid) = git::tree_entry_at(repo, commit_sha, std::path::Path::new(path)).ok()??;
    Some(oid.to_string())
}

pub fn read_range(repo: &gix::Repository, range_id: &str) -> Result<Range> {
    let wd = work_dir(repo)?;
    let oid = crate::git::resolve_ref_oid_optional(wd, &range_ref_path(range_id))?
        .ok_or_else(|| Error::RangeNotFound(range_id.to_string()))?;
    let raw = crate::git::read_git_text(repo, &oid)?;
    parse_range(&raw)
}

pub fn parse_range(text: &str) -> Result<Range> {
    if text.is_empty() || !text.ends_with('\n') {
        return Err(Error::Parse(
            "range blob must end with a trailing newline".into(),
        ));
    }
    let mut anchor: Option<String> = None;
    let mut created: Option<String> = None;
    let mut range_line: Option<(u32, u32, String, String)> = None;

    for (idx, line) in text.lines().enumerate() {
        if line.is_empty() {
            return Err(Error::Parse(format!(
                "blank line in range blob (line {})",
                idx + 1
            )));
        }
        if let Some(rest) = line.strip_prefix("anchor ") {
            if anchor.is_some() {
                return Err(Error::Parse("duplicate `anchor` header".into()));
            }
            if rest.is_empty() {
                return Err(Error::Parse("empty `anchor` value".into()));
            }
            anchor = Some(rest.to_string());
            continue;
        }
        if let Some(rest) = line.strip_prefix("created ") {
            if created.is_some() {
                return Err(Error::Parse("duplicate `created` header".into()));
            }
            if rest.is_empty() {
                return Err(Error::Parse("empty `created` value".into()));
            }
            created = Some(rest.to_string());
            continue;
        }
        if let Some(rest) = line.strip_prefix("range ") {
            if range_line.is_some() {
                return Err(Error::Parse("duplicate `range` line".into()));
            }
            let (meta, path) = rest.split_once('\t').ok_or_else(|| {
                Error::Parse(format!(
                    "`range` line missing TAB before path (line {})",
                    idx + 1
                ))
            })?;
            if path.is_empty() {
                return Err(Error::Parse("`range` path is empty".into()));
            }
            let fields: Vec<&str> = meta.split(' ').collect();
            // Whole-file form: `range whole <blob>\t<path>` (blob may be
            // empty when the underlying tree entry is a gitlink with no
            // file content).
            if fields.first().copied() == Some("whole") {
                if fields.len() != 2 {
                    return Err(Error::Parse(format!(
                        "`range whole` requires 1 field after `whole` (line {})",
                        idx + 1
                    )));
                }
                let blob = fields[1].to_string();
                range_line = Some((0, 0, blob, path.to_string()));
                continue;
            }
            if fields.len() != 3 {
                return Err(Error::Parse(format!(
                    "`range` line must have 3 fields before TAB (line {})",
                    idx + 1
                )));
            }
            let start: u32 = fields[0]
                .parse()
                .map_err(|_| Error::Parse(format!("invalid start `{}`", fields[0])))?;
            let end: u32 = fields[1]
                .parse()
                .map_err(|_| Error::Parse(format!("invalid end `{}`", fields[1])))?;
            let blob = fields[2].to_string();
            if blob.is_empty() {
                return Err(Error::Parse("`range` has empty blob".into()));
            }
            range_line = Some((start, end, blob, path.to_string()));
            continue;
        }
        // Additive-extension tolerance: unknown `key value` lines pass.
        if line.split_once(' ').is_none_or(|(k, _)| k.is_empty()) {
            return Err(Error::Parse(format!(
                "malformed line `{}` in range blob",
                line
            )));
        }
    }

    let (start, end, blob, path) =
        range_line.ok_or_else(|| Error::Parse("range blob missing `range` line".to_string()))?;
    let extent = if start == 0 && end == 0 {
        RangeExtent::Whole
    } else {
        RangeExtent::Lines { start, end }
    };
    Ok(Range {
        anchor_sha: anchor.ok_or_else(|| Error::Parse("missing `anchor` header".into()))?,
        created_at: created.ok_or_else(|| Error::Parse("missing `created` header".into()))?,
        path,
        extent,
        blob,
    })
}

pub fn serialize_range(range: &Range) -> String {
    match range.extent {
        RangeExtent::Lines { start, end } => format!(
            "anchor {}\ncreated {}\nrange {} {} {}\t{}\n",
            range.anchor_sha, range.created_at, start, end, range.blob, range.path
        ),
        RangeExtent::Whole => format!(
            "anchor {}\ncreated {}\nrange whole {}\t{}\n",
            range.anchor_sha, range.created_at, range.blob, range.path
        ),
    }
}

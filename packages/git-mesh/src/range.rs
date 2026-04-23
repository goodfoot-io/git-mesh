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

use crate::git::{self, git_stdout, git_with_input, work_dir};
use crate::types::Range;
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

/// Create a Range record, write the blob, and create `refs/ranges/v1/<uuid>`.
pub fn create_range(
    repo: &gix::Repository,
    anchor_sha: &str,
    path: &str,
    start: u32,
    end: u32,
) -> Result<String> {
    validate_path(path)?;
    if start < 1 || end < start {
        return Err(Error::InvalidRange { start, end });
    }
    let wd = work_dir(repo)?;
    // Confirm anchor reachable before dereferencing a tree against it.
    if git_stdout(wd, ["rev-parse", "--verify", "--quiet", anchor_sha]).is_err() {
        return Err(Error::Unreachable {
            anchor_sha: anchor_sha.to_string(),
        });
    }
    let blob = git::path_blob_at(repo, anchor_sha, path)?;
    let line_count = git::blob_line_count(repo, &blob)?;
    if end > line_count {
        return Err(Error::InvalidRange { start, end });
    }
    let range = Range {
        anchor_sha: anchor_sha.to_string(),
        created_at: Utc::now().to_rfc3339(),
        path: path.to_string(),
        start,
        end,
        blob,
    };
    let blob_oid = git_with_input(wd, ["hash-object", "-w", "--stdin"], &serialize_range(&range))?;
    let id = Uuid::new_v4().to_string();
    git::update_ref_cas(repo, &range_ref_path(&id), &blob_oid, None)?;
    Ok(id)
}

pub fn read_range(repo: &gix::Repository, range_id: &str) -> Result<Range> {
    let wd = work_dir(repo)?;
    let oid = git_stdout(wd, ["rev-parse", &range_ref_path(range_id)])
        .map_err(|_| Error::RangeNotFound(range_id.to_string()))?;
    let raw = crate::git::git_stdout_raw(wd, ["cat-file", "-p", &oid])?;
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

    let (start, end, blob, path) = range_line.ok_or_else(|| {
        Error::Parse("range blob missing `range` line".to_string())
    })?;
    Ok(Range {
        anchor_sha: anchor.ok_or_else(|| Error::Parse("missing `anchor` header".into()))?,
        created_at: created.ok_or_else(|| Error::Parse("missing `created` header".into()))?,
        path,
        start,
        end,
        blob,
    })
}

pub fn serialize_range(range: &Range) -> String {
    format!(
        "anchor {}\ncreated {}\nrange {} {} {}\t{}\n",
        range.anchor_sha, range.created_at, range.start, range.end, range.blob, range.path
    )
}

//! Anchor blob serialization and memory records — see §3.1, §4.1, §6.1.
//!
//! An Anchor is a memory record embedded in `anchors` with a
//! commit-object-style text format:
//!
//! ```text
//! id <uuid>
//! commit <sha>
//! created <iso-8601>
//! extent <start> <end> <blob>\t<path>
//! ```

use crate::git::{self, RefUpdate, apply_ref_transaction, resolve_ref_oid_optional, work_dir};
use crate::types::{Anchor, AnchorExtent};
use crate::{Error, Result};
use chrono::Utc;
use uuid::Uuid;

fn validate_path(path: &str) -> Result<()> {
    if path.is_empty() {
        return Err(Error::Parse("anchor path must not be empty".into()));
    }
    if let Some(bad) = path.chars().find(|c| matches!(c, '\t' | '\n' | '\0')) {
        return Err(Error::Parse(format!(
            "anchor path contains unsupported control character `{}`",
            bad.escape_debug()
        )));
    }
    Ok(())
}

/// Create an Anchor record (line-anchor).
pub fn create_anchor(
    repo: &gix::Repository,
    anchor_sha: &str,
    path: &str,
    start: u32,
    end: u32,
) -> Result<String> {
    let (id, anchor) = create_anchor_with_extent(
        repo,
        anchor_sha,
        path,
        AnchorExtent::LineRange { start, end },
    )?;
    let blob_oid = git::write_blob_bytes(repo, serialize_anchor(&anchor).as_bytes())?;
    apply_ref_transaction(
        work_dir(repo)?,
        &[RefUpdate::Create {
            name: anchor_ref_path(&id),
            new_oid: blob_oid,
        }],
    )?;
    Ok(id)
}

/// Create an Anchor record at the given extent (line-anchor or whole-file).
pub fn create_anchor_with_extent(
    repo: &gix::Repository,
    anchor_sha: &str,
    path: &str,
    extent: AnchorExtent,
) -> Result<(String, Anchor)> {
    create_anchor_with_extent_inner(repo, anchor_sha, path, extent, true)
}

/// Variant used by the mesh commit pipeline (Slice 1). The pipeline has
/// already validated line bounds against the captured sidecar
/// `line_count`, which is the post-filter source of truth for
/// `filter=lfs` paths.
pub(crate) fn create_anchor_with_extent_skipping_blob_bounds(
    repo: &gix::Repository,
    anchor_sha: &str,
    path: &str,
    extent: AnchorExtent,
) -> Result<(String, Anchor)> {
    create_anchor_with_extent_inner(repo, anchor_sha, path, extent, false)
}

fn create_anchor_with_extent_inner(
    repo: &gix::Repository,
    anchor_sha: &str,
    path: &str,
    extent: AnchorExtent,
    check_blob_bounds: bool,
) -> Result<(String, Anchor)> {
    validate_path(path)?;
    if let AnchorExtent::LineRange { start, end } = extent
        && (start < 1 || end < start)
    {
        return Err(Error::InvalidAnchor { start, end });
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
        Err(_) if matches!(extent, AnchorExtent::WholeFile) => {
            gitlink_sha_at(repo, anchor_sha, path).unwrap_or_default()
        }
        Err(e) => return Err(e),
    };
    if check_blob_bounds
        && let AnchorExtent::LineRange { start, end } = extent
        && !blob.is_empty()
    {
        let line_count = git::blob_line_count(repo, &blob)?;
        if end > line_count {
            return Err(Error::InvalidAnchor { start, end });
        }
    }
    let anchor = Anchor {
        anchor_sha: anchor_sha.to_string(),
        created_at: Utc::now().to_rfc3339(),
        path: path.to_string(),
        extent,
        blob,
    };
    let id = Uuid::new_v4().to_string();
    Ok((id, anchor))
}

fn gitlink_sha_at(repo: &gix::Repository, commit_sha: &str, path: &str) -> Option<String> {
    let (_mode, oid) = git::tree_entry_at(repo, commit_sha, std::path::Path::new(path)).ok()??;
    Some(oid.to_string())
}

pub fn parse_anchor(text: &str) -> Result<Anchor> {
    if text.is_empty() || !text.ends_with('\n') {
        return Err(Error::Parse(
            "anchor blob must end with a trailing newline".into(),
        ));
    }
    let mut commit: Option<String> = None;
    let mut created: Option<String> = None;
    let mut extent_line: Option<(u32, u32, String, String)> = None;

    for (idx, line) in text.lines().enumerate() {
        if line.is_empty() {
            return Err(Error::Parse(format!(
                "blank line in anchor blob (line {})",
                idx + 1
            )));
        }
        if let Some(rest) = line.strip_prefix("commit ") {
            if commit.is_some() {
                return Err(Error::Parse("duplicate `commit` header".into()));
            }
            if rest.is_empty() {
                return Err(Error::Parse("empty `commit` value".into()));
            }
            commit = Some(rest.to_string());
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
        if let Some(rest) = line.strip_prefix("extent ") {
            if extent_line.is_some() {
                return Err(Error::Parse("duplicate `extent` line".into()));
            }
            let (meta, path) = rest.split_once('\t').ok_or_else(|| {
                Error::Parse(format!(
                    "`extent` line missing TAB before path (line {})",
                    idx + 1
                ))
            })?;
            if path.is_empty() {
                return Err(Error::Parse("`extent` path is empty".into()));
            }
            let fields: Vec<&str> = meta.split(' ').collect();
            // Whole-file form: `extent whole <blob>\t<path>` (blob may be
            // empty when the underlying tree entry is a gitlink with no
            // file content).
            if fields.first().copied() == Some("whole") {
                if fields.len() != 2 {
                    return Err(Error::Parse(format!(
                        "`extent whole` requires 1 field after `whole` (line {})",
                        idx + 1
                    )));
                }
                let blob = fields[1].to_string();
                extent_line = Some((0, 0, blob, path.to_string()));
                continue;
            }
            if fields.len() != 3 {
                return Err(Error::Parse(format!(
                    "`extent` line must have 3 fields before TAB (line {})",
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
                return Err(Error::Parse("`extent` has empty blob".into()));
            }
            extent_line = Some((start, end, blob, path.to_string()));
            continue;
        }
        // Additive-extension tolerance: unknown `key value` lines pass.
        if line.split_once(' ').is_none_or(|(k, _)| k.is_empty()) {
            return Err(Error::Parse(format!(
                "malformed line `{}` in anchor blob",
                line
            )));
        }
    }

    let (start, end, blob, path) =
        extent_line.ok_or_else(|| Error::Parse("anchor blob missing `extent` line".to_string()))?;
    let extent = if start == 0 && end == 0 {
        AnchorExtent::WholeFile
    } else {
        AnchorExtent::LineRange { start, end }
    };
    Ok(Anchor {
        anchor_sha: commit.ok_or_else(|| Error::Parse("missing `commit` header".into()))?,
        created_at: created.ok_or_else(|| Error::Parse("missing `created` header".into()))?,
        path,
        extent,
        blob,
    })
}

pub fn serialize_anchor(anchor: &Anchor) -> String {
    match anchor.extent {
        AnchorExtent::LineRange { start, end } => format!(
            "commit {}\ncreated {}\nextent {} {} {}\t{}\n",
            anchor.anchor_sha, anchor.created_at, start, end, anchor.blob, anchor.path
        ),
        AnchorExtent::WholeFile => format!(
            "commit {}\ncreated {}\nextent whole {}\t{}\n",
            anchor.anchor_sha, anchor.created_at, anchor.blob, anchor.path
        ),
    }
}

pub fn anchor_ref_path(anchor_id: &str) -> String {
    format!("refs/anchors/v1/{anchor_id}")
}

pub fn read_anchor(repo: &gix::Repository, anchor_id: &str) -> Result<Anchor> {
    if let Some(blob_oid) = resolve_ref_oid_optional(work_dir(repo)?, &anchor_ref_path(anchor_id))?
    {
        return parse_anchor(&git::read_git_text(repo, &blob_oid)?);
    }
    for name in crate::mesh::read::list_mesh_names(repo)? {
        let mesh = crate::mesh::read::read_mesh(repo, &name)?;
        if let Some((_id, anchor)) = mesh
            .anchors_v2
            .into_iter()
            .find(|(id, _anchor)| id == anchor_id)
        {
            return Ok(anchor);
        }
    }
    Err(Error::AnchorNotFound(anchor_id.to_string()))
}

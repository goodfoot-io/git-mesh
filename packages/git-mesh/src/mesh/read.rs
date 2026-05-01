//! Read-only mesh operations — §6.5, §6.6, §10.4.

use crate::git::{self, resolve_ref_oid_optional, work_dir};
use crate::types::{CopyDetection, Mesh, MeshConfig};
use crate::{Error, Result};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MeshCommitInfo {
    pub commit_oid: String,
    pub author_name: String,
    pub author_email: String,
    pub author_date: String,
    pub summary: String,
    pub message: String,
}

fn mesh_ref(name: &str) -> String {
    format!("refs/meshes/v1/{name}")
}

pub(crate) fn resolve_mesh_revision(
    repo: &gix::Repository,
    name: &str,
    commit_ish: Option<&str>,
) -> Result<String> {
    let wd = work_dir(repo)?;
    let mesh_ref = mesh_ref(name);
    let revision = match commit_ish {
        None => mesh_ref.clone(),
        Some("HEAD") => mesh_ref.clone(),
        Some(value) => {
            if let Some(suffix) = value.strip_prefix("HEAD") {
                format!("{mesh_ref}{suffix}")
            } else {
                value.to_string()
            }
        }
    };
    repo.rev_parse_single(revision.as_str())
        .map(|id| id.detach().to_string())
        .map_err(|_| {
            if let Some(value) = commit_ish
                && resolve_ref_oid_optional(wd, &mesh_ref)
                    .ok()
                    .flatten()
                    .is_some()
            {
                return Error::Git(format!("invalid mesh revision `{value}` for `{name}`"));
            }
            Error::MeshNotFound(name.to_string())
        })
}

pub fn list_mesh_names(repo: &gix::Repository) -> Result<Vec<String>> {
    let mut names = git::list_refs_stripped(repo, "refs/meshes/v1")?;
    names.sort();
    Ok(names)
}

pub(crate) fn list_mesh_refs(repo: &gix::Repository) -> Result<Vec<(String, String)>> {
    let mut refs = git::list_refs_stripped_with_oids(repo, "refs/meshes/v1")?;
    refs.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(refs)
}

pub fn read_mesh(repo: &gix::Repository, name: &str) -> Result<Mesh> {
    read_mesh_at(repo, name, None)
}

pub fn read_mesh_at(repo: &gix::Repository, name: &str, commit_ish: Option<&str>) -> Result<Mesh> {
    let commit_oid = resolve_mesh_revision(repo, name, commit_ish)?;
    read_mesh_from_commit(repo, name, &commit_oid)
}

const FOLLOW_SUBJECT_PREFIX: &str = "mesh: follow ";

/// Walk the parent chain of `tip_oid` backwards, skipping commits whose
/// subject starts with `"mesh: follow "`, and return the message of the first
/// commit that doesn't match.  Falls back to the tip message when the chain is
/// exhausted (degenerate root that is itself a follow commit).
fn why_walking_past_follows(repo: &gix::Repository, tip_oid: &str) -> Result<String> {
    let tip_meta = git::commit_meta(repo, tip_oid)?;
    if !tip_meta.summary.starts_with(FOLLOW_SUBJECT_PREFIX) {
        return Ok(tip_meta.message);
    }
    // Walk first parents until we find a non-follow commit.
    let tip_oid_parsed = tip_oid
        .parse::<gix::ObjectId>()
        .map_err(|e| Error::Git(format!("parse oid {tip_oid}: {e}")))?;
    let commit = repo
        .find_commit(tip_oid_parsed)
        .map_err(|e| Error::Git(format!("find commit {tip_oid}: {e}")))?;
    let mut parent_ids: Vec<String> = commit
        .parent_ids()
        .map(|id| id.detach().to_string())
        .collect();
    while let Some(parent_oid) = parent_ids.into_iter().next() {
        let meta = git::commit_meta(repo, &parent_oid)?;
        if !meta.summary.starts_with(FOLLOW_SUBJECT_PREFIX) {
            return Ok(meta.message);
        }
        let oid_parsed = parent_oid
            .parse::<gix::ObjectId>()
            .map_err(|e| Error::Git(format!("parse oid {parent_oid}: {e}")))?;
        let parent_commit = repo
            .find_commit(oid_parsed)
            .map_err(|e| Error::Git(format!("find commit {parent_oid}: {e}")))?;
        parent_ids = parent_commit
            .parent_ids()
            .map(|id| id.detach().to_string())
            .collect();
    }
    // Exhausted the chain — every commit in the mesh history carries the
    // follow-subject prefix, which is only possible if the writer guard in
    // `commit_mesh` was bypassed. Fail closed rather than leaking the marker
    // as the displayed why.
    Err(Error::Git(
        "mesh history contains only follow commits — no why to display".into(),
    ))
}

pub(crate) fn read_mesh_from_commit(
    repo: &gix::Repository,
    name: &str,
    commit_oid: &str,
) -> Result<Mesh> {
    let message = why_walking_past_follows(repo, commit_oid)?;
    let anchors_v2 = read_anchors_v2_blob(repo, commit_oid).unwrap_or_default();
    let config = read_config_blob(repo, commit_oid).unwrap_or_else(|_| default_config());
    let anchors = anchors_v2.iter().map(|(id, _anchor)| id.clone()).collect();
    Ok(Mesh {
        name: name.to_string(),
        anchors,
        anchors_v2,
        message,
        config,
    })
}

pub(crate) fn read_anchors_v2_blob(
    repo: &gix::Repository,
    commit_oid: &str,
) -> Result<Vec<(String, crate::types::Anchor)>> {
    let blob_oid = match git::path_blob_at(repo, commit_oid, "anchors") {
        Ok(oid) => oid,
        Err(_) => return Ok(Vec::new()),
    };
    let text = git::read_git_text(repo, &blob_oid)?;
    let mut out = Vec::new();
    for anchor_text in text.split("\n\n") {
        let anchor_text = anchor_text.trim();
        if !anchor_text.is_empty()
            && let Some(rest) = anchor_text.strip_prefix("id ")
            && let Some((id_str, anchor_body)) = rest.split_once('\n')
        {
            let mut formatted = anchor_body.to_string();
            if !formatted.ends_with('\n') {
                formatted.push('\n');
            }
            if let Ok(a) = crate::anchor::parse_anchor(&formatted) {
                out.push((id_str.to_string(), a));
            }
        }
    }
    Ok(out)
}

pub(crate) struct MeshListingRecord {
    pub message: String,
    pub anchors_v2: Vec<(String, crate::types::Anchor)>,
}

pub(crate) fn read_mesh_listing_at(
    repo: &gix::Repository,
    commit_oid: &str,
) -> Result<MeshListingRecord> {
    let message = git::commit_meta(repo, commit_oid)?.message;
    let anchors_v2 = read_anchors_v2_blob(repo, commit_oid).unwrap_or_default();
    Ok(MeshListingRecord {
        message,
        anchors_v2,
    })
}

fn default_config() -> MeshConfig {
    MeshConfig {
        copy_detection: crate::types::DEFAULT_COPY_DETECTION,
        ignore_whitespace: crate::types::DEFAULT_IGNORE_WHITESPACE,
        follow_moves: crate::types::DEFAULT_FOLLOW_MOVES,
    }
}

pub(crate) fn read_config_blob(repo: &gix::Repository, commit_oid: &str) -> Result<MeshConfig> {
    let blob_oid = git::path_blob_at(repo, commit_oid, "config")?;
    let text = git::read_git_text(repo, &blob_oid)?;
    parse_config_blob(&text)
}

pub(crate) fn parse_config_blob(text: &str) -> Result<MeshConfig> {
    let mut cfg = default_config();
    for line in text.lines() {
        if line.is_empty() {
            continue;
        }
        let (k, v) = line
            .split_once(' ')
            .ok_or_else(|| Error::Parse(format!("malformed config line `{line}`")))?;
        match k {
            "copy-detection" => {
                cfg.copy_detection = match v {
                    "off" => CopyDetection::Off,
                    "same-commit" => CopyDetection::SameCommit,
                    "any-file-in-commit" => CopyDetection::AnyFileInCommit,
                    "any-file-in-repo" => CopyDetection::AnyFileInRepo,
                    _ => return Err(Error::Parse(format!("invalid copy-detection `{v}`"))),
                };
            }
            "ignore-whitespace" => {
                cfg.ignore_whitespace = match v {
                    "true" => true,
                    "false" => false,
                    _ => return Err(Error::Parse(format!("invalid ignore-whitespace `{v}`"))),
                };
            }
            "follow-moves" => {
                cfg.follow_moves = match v {
                    "true" => true,
                    "false" => false,
                    _ => return Err(Error::Parse(format!("invalid follow-moves `{v}`"))),
                };
            }
            _ => {
                // Unknown keys tolerated.
            }
        }
    }
    Ok(cfg)
}

pub(crate) fn serialize_config_blob(cfg: &MeshConfig) -> String {
    format!(
        "copy-detection {}\nignore-whitespace {}\nfollow-moves {}\n",
        crate::staging::serialize_copy_detection(cfg.copy_detection),
        cfg.ignore_whitespace,
        cfg.follow_moves
    )
}

pub fn show_mesh(repo: &gix::Repository, name: &str) -> Result<Mesh> {
    read_mesh(repo, name)
}

pub fn show_mesh_at(repo: &gix::Repository, name: &str, commit_ish: Option<&str>) -> Result<Mesh> {
    read_mesh_at(repo, name, commit_ish)
}

pub fn mesh_commit_info(repo: &gix::Repository, name: &str) -> Result<MeshCommitInfo> {
    mesh_commit_info_at(repo, name, None)
}

pub fn mesh_commit_info_at(
    repo: &gix::Repository,
    name: &str,
    commit_ish: Option<&str>,
) -> Result<MeshCommitInfo> {
    let commit_oid = resolve_mesh_revision(repo, name, commit_ish)?;
    let meta = git::commit_meta(repo, &commit_oid)?;
    Ok(MeshCommitInfo {
        commit_oid,
        author_name: meta.author_name,
        author_email: meta.author_email,
        author_date: meta.author_date_rfc2822,
        summary: meta.summary,
        message: meta.message,
    })
}

pub fn mesh_log(
    repo: &gix::Repository,
    name: &str,
    limit: Option<usize>,
) -> Result<Vec<MeshCommitInfo>> {
    let wd = work_dir(repo)?;
    // Validate the ref exists first.
    let tip = resolve_ref_oid_optional(wd, &mesh_ref(name))?
        .ok_or_else(|| Error::MeshNotFound(name.into()))?;
    let oids = git::rev_walk_excluding(repo, &[&tip], &[], limit)?;
    oids.into_iter()
        .map(|oid| mesh_commit_info_at(repo, name, Some(&oid)))
        .collect()
}

pub fn is_ancestor_commit(repo: &gix::Repository, name: &str, ancestor: &str) -> Result<bool> {
    crate::git::is_ancestor(repo, ancestor, &mesh_ref(name))
}

pub fn resolve_commit_ish(repo: &gix::Repository, name: &str, commit_ish: &str) -> Result<String> {
    resolve_mesh_revision(repo, name, Some(commit_ish))
}

#[cfg(test)]
mod tests {
    use super::{parse_config_blob, serialize_config_blob};
    use crate::types::{CopyDetection, MeshConfig, DEFAULT_FOLLOW_MOVES};

    fn default_cfg() -> MeshConfig {
        MeshConfig {
            copy_detection: CopyDetection::SameCommit,
            ignore_whitespace: false,
            follow_moves: false,
        }
    }

    #[test]
    fn follow_moves_default_preserved_in_round_trip() {
        let cfg = default_cfg();
        let serialized = serialize_config_blob(&cfg);
        let parsed = parse_config_blob(&serialized).unwrap();
        assert_eq!(parsed.follow_moves, DEFAULT_FOLLOW_MOVES);
    }

    #[test]
    fn follow_moves_true_round_trips() {
        let mut cfg = default_cfg();
        cfg.follow_moves = true;
        let serialized = serialize_config_blob(&cfg);
        assert!(serialized.contains("follow-moves true"), "serialized={serialized}");
        let parsed = parse_config_blob(&serialized).unwrap();
        assert!(parsed.follow_moves);
    }

    #[test]
    fn follow_moves_false_round_trips() {
        let mut cfg = default_cfg();
        cfg.follow_moves = false;
        let serialized = serialize_config_blob(&cfg);
        assert!(serialized.contains("follow-moves false"), "serialized={serialized}");
        let parsed = parse_config_blob(&serialized).unwrap();
        assert!(!parsed.follow_moves);
    }

    #[test]
    fn follow_moves_invalid_value_returns_parse_error() {
        let result = parse_config_blob("follow-moves maybe\n");
        assert!(result.is_err(), "invalid follow-moves value must error");
        let msg = format!("{}", result.unwrap_err());
        assert!(msg.contains("follow-moves"), "error must mention the key; msg={msg}");
    }

    #[test]
    fn serialize_config_always_includes_follow_moves_line() {
        let cfg = default_cfg();
        let serialized = serialize_config_blob(&cfg);
        assert!(
            serialized.contains("follow-moves"),
            "serialized config must include follow-moves line; got={serialized}"
        );
    }
}

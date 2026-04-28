//! Read-only mesh operations — §6.5, §6.6, §10.4.

use crate::git::{self, git_show_file_lines, resolve_ref_oid_optional, work_dir};
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

pub fn read_mesh(repo: &gix::Repository, name: &str) -> Result<Mesh> {
    read_mesh_at(repo, name, None)
}

pub fn read_mesh_at(repo: &gix::Repository, name: &str, commit_ish: Option<&str>) -> Result<Mesh> {
    let wd = work_dir(repo)?;
    let commit_oid = resolve_mesh_revision(repo, name, commit_ish)?;
    let message = git::commit_meta(repo, &commit_oid)?.message;
    let anchors = git_show_file_lines(wd, &commit_oid, "anchors").unwrap_or_default();
    let config = read_config_blob(repo, &commit_oid).unwrap_or_else(|_| default_config());
    Ok(Mesh {
        name: name.to_string(),
        anchors,
        message,
        config,
    })
}

fn default_config() -> MeshConfig {
    MeshConfig {
        copy_detection: crate::types::DEFAULT_COPY_DETECTION,
        ignore_whitespace: crate::types::DEFAULT_IGNORE_WHITESPACE,
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
            _ => {
                // Unknown keys tolerated.
            }
        }
    }
    Ok(cfg)
}

pub(crate) fn serialize_config_blob(cfg: &MeshConfig) -> String {
    format!(
        "copy-detection {}\nignore-whitespace {}\n",
        crate::staging::serialize_copy_detection(cfg.copy_detection),
        cfg.ignore_whitespace
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

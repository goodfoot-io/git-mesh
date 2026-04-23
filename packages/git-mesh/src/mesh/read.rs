//! Read-only mesh operations — §6.5, §6.6, §10.4.

use crate::git::{
    git_show_file_lines, git_stdout, resolve_ref_oid_optional, work_dir,
};
use crate::types::{CopyDetection, Mesh, MeshConfig};
use crate::{Error, Result};
use std::path::Path;

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
    work_dir: &Path,
    name: &str,
    commit_ish: Option<&str>,
) -> Result<String> {
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
    git_stdout(work_dir, ["rev-parse", &revision])
        .map_err(|_| Error::MeshNotFound(name.to_string()))
}

pub fn list_mesh_names(repo: &gix::Repository) -> Result<Vec<String>> {
    let wd = work_dir(repo)?;
    let output = git_stdout(
        wd,
        [
            "for-each-ref",
            "--format=%(refname:strip=3)",
            "refs/meshes/v1",
        ],
    )?;
    let mut names: Vec<String> = output
        .lines()
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect();
    names.sort();
    Ok(names)
}

pub fn read_mesh(repo: &gix::Repository, name: &str) -> Result<Mesh> {
    read_mesh_at(repo, name, None)
}

pub fn read_mesh_at(
    repo: &gix::Repository,
    name: &str,
    commit_ish: Option<&str>,
) -> Result<Mesh> {
    let wd = work_dir(repo)?;
    let commit_oid = resolve_mesh_revision(wd, name, commit_ish)?;
    let message = git_stdout(wd, ["show", "-s", "--format=%B", &commit_oid])?;
    let ranges = git_show_file_lines(wd, &commit_oid, "ranges").unwrap_or_default();
    let config = read_config_blob(wd, &commit_oid).unwrap_or_else(|_| default_config());
    Ok(Mesh {
        name: name.to_string(),
        ranges,
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

pub(crate) fn read_config_blob(work_dir: &Path, commit_oid: &str) -> Result<MeshConfig> {
    let text = git_stdout(work_dir, ["show", &format!("{commit_oid}:config")])?;
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

pub fn show_mesh_at(
    repo: &gix::Repository,
    name: &str,
    commit_ish: Option<&str>,
) -> Result<Mesh> {
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
    let wd = work_dir(repo)?;
    let commit_oid = resolve_mesh_revision(wd, name, commit_ish)?;
    let author_name = git_stdout(wd, ["show", "-s", "--format=%an", &commit_oid])?;
    let author_email = git_stdout(wd, ["show", "-s", "--format=%ae", &commit_oid])?;
    let author_date = git_stdout(wd, ["show", "-s", "--format=%aD", &commit_oid])?;
    let summary = git_stdout(wd, ["show", "-s", "--format=%s", &commit_oid])?;
    let message = git_stdout(wd, ["show", "-s", "--format=%B", &commit_oid])?;
    Ok(MeshCommitInfo {
        commit_oid,
        author_name,
        author_email,
        author_date,
        summary,
        message,
    })
}

pub fn mesh_log(
    repo: &gix::Repository,
    name: &str,
    limit: Option<usize>,
) -> Result<Vec<MeshCommitInfo>> {
    let wd = work_dir(repo)?;
    // Validate the ref exists first.
    resolve_ref_oid_optional(wd, &mesh_ref(name))?
        .ok_or_else(|| Error::MeshNotFound(name.into()))?;
    let mut args = vec!["rev-list".to_string()];
    if let Some(limit) = limit {
        args.push(format!("--max-count={limit}"));
    }
    args.push(mesh_ref(name));
    let commits = git_stdout(wd, args.iter().map(String::as_str))?;
    commits
        .lines()
        .filter(|l| !l.is_empty())
        .map(|oid| mesh_commit_info_at(repo, name, Some(oid)))
        .collect()
}

pub fn is_ancestor_commit(
    repo: &gix::Repository,
    name: &str,
    ancestor: &str,
) -> Result<bool> {
    crate::git::is_ancestor(repo, ancestor, &mesh_ref(name))
}

pub fn resolve_commit_ish(
    repo: &gix::Repository,
    name: &str,
    commit_ish: &str,
) -> Result<String> {
    resolve_mesh_revision(work_dir(repo)?, name, Some(commit_ish))
}

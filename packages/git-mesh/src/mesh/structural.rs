//! Structural mesh operations — §6.8.

use crate::git::{
    apply_ref_transaction, git_stdout, git_stdout_with_identity, resolve_ref_oid_optional,
    work_dir, RefUpdate,
};
use crate::validation::validate_mesh_name;
use crate::{Error, Result};

fn mesh_ref(name: &str) -> String {
    format!("refs/meshes/v1/{name}")
}

pub fn delete_mesh(repo: &gix::Repository, name: &str) -> Result<()> {
    let wd = work_dir(repo)?;
    let current = resolve_ref_oid_optional(wd, &mesh_ref(name))?
        .ok_or_else(|| Error::MeshNotFound(name.into()))?;
    apply_ref_transaction(
        wd,
        &[RefUpdate::Delete {
            name: mesh_ref(name),
            expected_old_oid: current,
        }],
    )
}

pub fn rename_mesh(repo: &gix::Repository, old: &str, new: &str) -> Result<()> {
    validate_mesh_name(new)?;
    let wd = work_dir(repo)?;
    let old_ref = mesh_ref(old);
    let new_ref = mesh_ref(new);
    let old_oid = resolve_ref_oid_optional(wd, &old_ref)?
        .ok_or_else(|| Error::MeshNotFound(old.into()))?;
    if resolve_ref_oid_optional(wd, &new_ref)?.is_some() {
        return Err(Error::MeshAlreadyExists(new.into()));
    }
    apply_ref_transaction(
        wd,
        &[
            RefUpdate::Create {
                name: new_ref,
                new_oid: old_oid.clone(),
            },
            RefUpdate::Delete {
                name: old_ref,
                expected_old_oid: old_oid,
            },
        ],
    )
}

pub fn restore_mesh(repo: &gix::Repository, name: &str) -> Result<()> {
    // Clear staging only; do not touch the ref.
    crate::staging::clear_staging(repo, name)
}

pub fn revert_mesh(
    repo: &gix::Repository,
    name: &str,
    commit_ish: &str,
) -> Result<String> {
    let wd = work_dir(repo)?;
    let ref_name = mesh_ref(name);
    let target = super::read::resolve_commit_ish(repo, name, commit_ish)?;
    let current = resolve_ref_oid_optional(wd, &ref_name)?
        .ok_or_else(|| Error::MeshNotFound(name.into()))?;
    let tree_oid = git_stdout(wd, ["show", "-s", "--format=%T", &target])?;
    let message = git_stdout(wd, ["show", "-s", "--format=%B", &target])?;
    let args = [
        "commit-tree".to_string(),
        tree_oid,
        "-m".to_string(),
        message,
        "-p".to_string(),
        current.clone(),
    ];
    let new_commit = git_stdout_with_identity(wd, args.iter().map(String::as_str))?;
    apply_ref_transaction(
        wd,
        &[RefUpdate::Update {
            name: ref_name,
            new_oid: new_commit.clone(),
            expected_old_oid: current,
        }],
    )?;
    Ok(new_commit)
}

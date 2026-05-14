//! Structural mesh operations — §6.8.

use crate::git::{
    self, RefUpdate, apply_ref_transaction, create_commit, resolve_ref_oid_optional, work_dir,
};
use crate::mesh::catalog::{Catalog, CATALOG_REF};
use crate::staging;
use crate::validation::validate_mesh_name;
use crate::{Error, Result};

fn mesh_ref(name: &str) -> String {
    format!("refs/meshes/v1/{name}")
}

pub fn delete_mesh(repo: &gix::Repository, name: &str) -> Result<()> {
    let wd = work_dir(repo)?;
    let ref_name = mesh_ref(name);
    let current =
        resolve_ref_oid_optional(wd, &ref_name)?.ok_or_else(|| Error::MeshNotFound(name.into()))?;

    // Check staging before deletion — refuse if any staged work exists.
    let staging = staging::read_staging(repo, name)?;
    let staging_count = staging.adds.len()
        + staging.removes.len()
        + staging.configs.len()
        + staging.why.as_ref().map_or(0, |_| 1);
    if staging_count > 0 {
        return Err(Error::StagingResidueOnDelete {
            name: name.into(),
            count: staging_count,
        });
    }

    let mesh = super::read::read_mesh_at(repo, name, Some(&current))
        .or_else(|_| super::read::read_mesh(repo, name))?;

    // Update catalog when the catalog ref exists.
    let catalog_ref_oid = crate::git::resolve_ref_oid_optional_repo(repo, CATALOG_REF)?;
    if catalog_ref_oid.is_some() {
        let mut catalog = Catalog::load(repo)?;
        catalog.remove(name)?;
        crate::mesh::catalog::commit_catalog(
            repo,
            &catalog,
            &format!("mesh: delete {name}"),
            catalog_ref_oid.as_deref(),
        )?;
    }

    // Always delete per-mesh ref for backward compat.
    let mut updates = super::path_index::ref_updates_for_mesh(repo, name, &mesh.anchors_v2, &[])?;
    updates.push(RefUpdate::Delete {
        name: ref_name,
        expected_old_oid: current,
    });
    apply_ref_transaction(wd, &updates)
}

pub fn rename_mesh(repo: &gix::Repository, old: &str, new: &str) -> Result<()> {
    validate_mesh_name(new)?;
    let wd = work_dir(repo)?;
    let old_ref = mesh_ref(old);
    let new_ref = mesh_ref(new);
    let old_oid =
        resolve_ref_oid_optional(wd, &old_ref)?.ok_or_else(|| Error::MeshNotFound(old.into()))?;
    if resolve_ref_oid_optional(wd, &new_ref)?.is_some() {
        return Err(Error::MeshAlreadyExists(new.into()));
    }
    let mesh = super::read::read_mesh_at(repo, old, Some(&old_oid))
        .or_else(|_| super::read::read_mesh(repo, old))?;

    // Update catalog when the catalog ref exists.
    let catalog_ref_oid = crate::git::resolve_ref_oid_optional_repo(repo, CATALOG_REF)?;
    if catalog_ref_oid.is_some() {
        let mut catalog = Catalog::load(repo)?;
        catalog.remove(old)?;
        catalog.insert(new, &mesh)?;
        crate::mesh::catalog::commit_catalog(
            repo,
            &catalog,
            &format!("mesh: rename {old} -> {new}"),
            catalog_ref_oid.as_deref(),
        )?;
    }

    // Always update per-mesh refs for backward compat.
    let mut updates = super::path_index::ref_updates_for_rename(repo, old, new, &mesh.anchors_v2)?;
    updates.extend([
        RefUpdate::Create {
            name: new_ref,
            new_oid: old_oid.clone(),
        },
        RefUpdate::Delete {
            name: old_ref,
            expected_old_oid: old_oid,
        },
    ]);
    apply_ref_transaction(wd, &updates)
}

pub fn restore_mesh(repo: &gix::Repository, name: &str) -> Result<()> {
    // Clear staging only; do not touch the ref.
    crate::staging::clear_staging(repo, name)
}

pub fn revert_mesh(repo: &gix::Repository, name: &str, commit_ish: &str) -> Result<String> {
    let wd = work_dir(repo)?;
    let ref_name = mesh_ref(name);
    let target = super::read::resolve_commit_ish(repo, name, commit_ish)?;
    let current =
        resolve_ref_oid_optional(wd, &ref_name)?.ok_or_else(|| Error::MeshNotFound(name.into()))?;
    let tree_oid = git::commit_tree_oid(repo, &target)?;
    let message = git::commit_meta(repo, &target)?.message;
    let old_mesh = super::read::read_mesh_at(repo, name, Some(&current))
        .or_else(|_| super::read::read_mesh(repo, name))?;
    let new_mesh = super::read::read_mesh_at(repo, name, Some(&target))
        .or_else(|_| super::read::read_mesh(repo, name))?;
    let new_commit = create_commit(repo, &tree_oid, &message, std::slice::from_ref(&current))?;

    // Update catalog when the catalog ref exists.
    let catalog_ref_oid = crate::git::resolve_ref_oid_optional_repo(repo, CATALOG_REF)?;
    if catalog_ref_oid.is_some() {
        let mut catalog = Catalog::load(repo)?;
        catalog.insert(name, &new_mesh)?;
        crate::mesh::catalog::commit_catalog(
            repo,
            &catalog,
            &message,
            catalog_ref_oid.as_deref(),
        )?;
    }

    // Always update per-mesh ref for backward compat.
    let mut updates = super::path_index::ref_updates_for_mesh(
        repo,
        name,
        &old_mesh.anchors_v2,
        &new_mesh.anchors_v2,
    )?;
    updates.push(RefUpdate::Update {
        name: ref_name,
        new_oid: new_commit.clone(),
        expected_old_oid: current,
    });
    apply_ref_transaction(wd, &updates)?;
    Ok(new_commit)
}

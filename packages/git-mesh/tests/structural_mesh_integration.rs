//! Structural mesh operations (§6.8).

mod support;

use anyhow::Result;
use git_mesh::{
    append_add, commit_mesh, delete_mesh, list_mesh_names, read_mesh, rename_mesh, restore_mesh,
    revert_mesh, set_message,
};
use support::TestRepo;

fn seed(repo: &TestRepo, name: &str, msg: &str) -> Result<String> {
    let gix = repo.gix_repo()?;
    append_add(&gix, name, "file1.txt", 1, 5, None)?;
    set_message(&gix, name, msg)?;
    Ok(commit_mesh(&gix, name)?)
}

#[test]

fn delete_mesh_removes_ref() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed(&repo, "kill-me", "seed")?;
    delete_mesh(&repo.gix_repo()?, "kill-me")?;
    assert!(!repo.ref_exists("refs/meshes/v1/kill-me"));
    Ok(())
}

#[test]

fn delete_missing_mesh_errors() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let err = delete_mesh(&repo.gix_repo()?, "absent").unwrap_err();
    assert!(matches!(err, git_mesh::Error::MeshNotFound(_)));
    Ok(())
}

#[test]

fn rename_mesh_atomic() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed(&repo, "old-name", "seed")?;
    rename_mesh(&repo.gix_repo()?, "old-name", "new-name")?;
    assert!(!repo.ref_exists("refs/meshes/v1/old-name"));
    assert!(repo.ref_exists("refs/meshes/v1/new-name"));
    Ok(())
}

#[test]

fn rename_mesh_rejects_existing_target() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed(&repo, "a", "seed")?;
    seed(&repo, "b", "seed")?;
    let err = rename_mesh(&repo.gix_repo()?, "a", "b").unwrap_err();
    assert!(matches!(err, git_mesh::Error::MeshAlreadyExists(_)));
    Ok(())
}

#[test]

fn rename_mesh_rejects_reserved_name() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed(&repo, "ok", "seed")?;
    let err = rename_mesh(&repo.gix_repo()?, "ok", "delete").unwrap_err();
    assert!(matches!(err, git_mesh::Error::ReservedName(_)));
    Ok(())
}

#[test]

fn restore_mesh_clears_staging() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let gix = repo.gix_repo()?;
    append_add(&gix, "pending", "file1.txt", 1, 5, None)?;
    set_message(&gix, "pending", "draft")?;
    restore_mesh(&gix, "pending")?;
    // After restore, commit with empty staging should error.
    let err = commit_mesh(&gix, "pending").unwrap_err();
    assert!(matches!(err, git_mesh::Error::StagingEmpty(_)));
    Ok(())
}

#[test]

fn revert_mesh_fast_forwards_to_past_tree() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let gix = repo.gix_repo()?;
    append_add(&gix, "rev", "file1.txt", 1, 5, None)?;
    set_message(&gix, "rev", "v1")?;
    let v1 = commit_mesh(&gix, "rev")?;
    append_add(&gix, "rev", "file2.txt", 2, 4, None)?;
    set_message(&gix, "rev", "v2")?;
    commit_mesh(&gix, "rev")?;
    let new_tip = revert_mesh(&gix, "rev", &v1)?;
    assert_ne!(new_tip, v1, "§6.6: revert is fast-forward, not rewind");
    let m = read_mesh(&gix, "rev")?;
    assert_eq!(m.ranges.len(), 1, "tree content matches v1 state");
    Ok(())
}

#[test]

fn list_mesh_names_reflects_delete() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed(&repo, "a", "seed")?;
    seed(&repo, "b", "seed")?;
    delete_mesh(&repo.gix_repo()?, "a")?;
    let names = list_mesh_names(&repo.gix_repo()?)?;
    assert_eq!(names, vec!["b".to_string()]);
    Ok(())
}

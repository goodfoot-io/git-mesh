//! Library tests for mesh read paths (§6.5, §6.6, §10.4).

mod support;

use anyhow::Result;
use git_mesh::{
    append_add, commit_mesh, list_mesh_names, mesh_commit_info, mesh_commit_info_at, mesh_log,
    read_mesh, read_mesh_at, resolve_commit_ish, set_message, show_mesh,
};
use support::TestRepo;

fn seed_two_meshes(repo: &TestRepo) -> Result<()> {
    let gix = repo.gix_repo()?;
    append_add(&gix, "alpha", "file1.txt", 1, 5, None)?;
    set_message(&gix, "alpha", "alpha init")?;
    commit_mesh(&gix, "alpha")?;
    append_add(&gix, "beta", "file2.txt", 2, 6, None)?;
    set_message(&gix, "beta", "beta init")?;
    commit_mesh(&gix, "beta")?;
    Ok(())
}

#[test]

fn list_mesh_names_is_sorted() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed_two_meshes(&repo)?;
    let names = list_mesh_names(&repo.gix_repo()?)?;
    assert_eq!(names, vec!["alpha".to_string(), "beta".to_string()]);
    Ok(())
}

#[test]

fn list_mesh_names_empty_repo() -> Result<()> {
    let repo = TestRepo::seeded()?;
    assert!(list_mesh_names(&repo.gix_repo()?)?.is_empty());
    Ok(())
}

#[test]

fn read_mesh_returns_tip_state() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed_two_meshes(&repo)?;
    let m = read_mesh(&repo.gix_repo()?, "alpha")?;
    assert_eq!(m.name, "alpha");
    assert_eq!(m.ranges.len(), 1);
    assert!(m.message.contains("alpha init"));
    Ok(())
}

#[test]

fn read_mesh_missing_errors() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let err = read_mesh(&repo.gix_repo()?, "ghost").unwrap_err();
    assert!(matches!(err, git_mesh::Error::MeshNotFound(_)));
    Ok(())
}

#[test]

fn show_mesh_is_read_mesh_alias() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed_two_meshes(&repo)?;
    let gix = repo.gix_repo()?;
    assert_eq!(show_mesh(&gix, "alpha")?, read_mesh(&gix, "alpha")?);
    Ok(())
}

#[test]

fn read_mesh_at_walks_history() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let gix = repo.gix_repo()?;
    append_add(&gix, "hist", "file1.txt", 1, 5, None)?;
    set_message(&gix, "hist", "v1")?;
    let first = commit_mesh(&gix, "hist")?;
    append_add(&gix, "hist", "file2.txt", 3, 7, None)?;
    set_message(&gix, "hist", "v2")?;
    commit_mesh(&gix, "hist")?;
    let old = read_mesh_at(&gix, "hist", Some(&first))?;
    assert_eq!(old.ranges.len(), 1);
    let tip = read_mesh_at(&gix, "hist", None)?;
    assert_eq!(tip.ranges.len(), 2);
    Ok(())
}

#[test]

fn mesh_commit_info_fields_populated() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed_two_meshes(&repo)?;
    let info = mesh_commit_info(&repo.gix_repo()?, "alpha")?;
    assert_eq!(info.author_name, "Test User");
    assert_eq!(info.author_email, "test@example.com");
    assert_eq!(info.commit_oid.len(), 40);
    assert!(info.summary.contains("alpha init"));
    Ok(())
}

#[test]

fn mesh_commit_info_at_past_commit() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let gix = repo.gix_repo()?;
    append_add(&gix, "h", "file1.txt", 1, 5, None)?;
    set_message(&gix, "h", "v1")?;
    let first = commit_mesh(&gix, "h")?;
    append_add(&gix, "h", "file2.txt", 2, 4, None)?;
    set_message(&gix, "h", "v2")?;
    commit_mesh(&gix, "h")?;
    let past = mesh_commit_info_at(&gix, "h", Some(&first))?;
    assert!(past.summary.contains("v1"));
    Ok(())
}

#[test]

fn mesh_log_newest_first() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let gix = repo.gix_repo()?;
    append_add(&gix, "h", "file1.txt", 1, 5, None)?;
    set_message(&gix, "h", "v1")?;
    commit_mesh(&gix, "h")?;
    append_add(&gix, "h", "file2.txt", 2, 4, None)?;
    set_message(&gix, "h", "v2")?;
    commit_mesh(&gix, "h")?;
    let log = mesh_log(&gix, "h", None)?;
    assert_eq!(log.len(), 2);
    assert!(log[0].summary.contains("v2"));
    assert!(log[1].summary.contains("v1"));
    Ok(())
}

#[test]

fn mesh_log_respects_limit() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let gix = repo.gix_repo()?;
    for i in 1..=3u32 {
        append_add(&gix, "h", "file1.txt", i, i + 1, None)?;
        set_message(&gix, "h", &format!("v{i}"))?;
        commit_mesh(&gix, "h")?;
    }
    let log = mesh_log(&gix, "h", Some(2))?;
    assert_eq!(log.len(), 2);
    Ok(())
}

#[test]

fn resolve_commit_ish_returns_oid_on_ancestor() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let gix = repo.gix_repo()?;
    append_add(&gix, "h", "file1.txt", 1, 5, None)?;
    set_message(&gix, "h", "v1")?;
    let first = commit_mesh(&gix, "h")?;
    append_add(&gix, "h", "file2.txt", 2, 4, None)?;
    set_message(&gix, "h", "v2")?;
    commit_mesh(&gix, "h")?;
    let oid = resolve_commit_ish(&gix, "h", &first)?;
    assert_eq!(oid, first);
    Ok(())
}

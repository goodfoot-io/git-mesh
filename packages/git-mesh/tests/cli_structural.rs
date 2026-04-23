//! CLI: restore, revert, delete, mv, doctor (§6.7, §6.8).

mod support;

use anyhow::Result;
use git_mesh::validation::RESERVED_MESH_NAMES;
use support::{BareRepo, TestRepo};

fn seed(repo: &TestRepo, name: &str) -> Result<()> {
    repo.mesh_stdout(["add", name, "file1.txt#L1-L5"])?;
    repo.mesh_stdout(["message", name, "-m", "seed"])?;
    repo.mesh_stdout(["commit", name])?;
    Ok(())
}

#[test]

fn restore_clears_staging() -> Result<()> {
    let repo = TestRepo::seeded()?;
    repo.mesh_stdout(["add", "m", "file1.txt#L1-L5"])?;
    repo.mesh_stdout(["restore", "m"])?;
    let out = repo.run_mesh(["commit", "m"])?;
    assert!(!out.status.success(), "no-op commit should fail");
    Ok(())
}

#[test]

fn revert_creates_new_tip() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed(&repo, "rev")?;
    let first_oid = repo.git_stdout(["rev-parse", "refs/meshes/v1/rev"])?;
    repo.mesh_stdout(["add", "rev", "file2.txt#L1-L3"])?;
    repo.mesh_stdout(["message", "rev", "-m", "v2"])?;
    repo.mesh_stdout(["commit", "rev"])?;
    repo.mesh_stdout(["revert", "rev", &first_oid])?;
    let new_tip = repo.git_stdout(["rev-parse", "refs/meshes/v1/rev"])?;
    assert_ne!(new_tip, first_oid, "revert is fast-forward, not rewind");
    Ok(())
}

#[test]

fn delete_removes_ref() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed(&repo, "gone")?;
    repo.mesh_stdout(["delete", "gone"])?;
    assert!(!repo.ref_exists("refs/meshes/v1/gone"));
    Ok(())
}

#[test]

fn mv_renames_ref() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed(&repo, "oldn")?;
    repo.mesh_stdout(["mv", "oldn", "newn"])?;
    assert!(repo.ref_exists("refs/meshes/v1/newn"));
    assert!(!repo.ref_exists("refs/meshes/v1/oldn"));
    Ok(())
}

#[test]

fn mv_rejects_reserved_name() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed(&repo, "oldn")?;
    let out = repo.run_mesh(["mv", "oldn", "delete"])?;
    assert!(!out.status.success());
    Ok(())
}

#[test]

fn every_reserved_name_rejected_on_create() -> Result<()> {
    // §10.2 reserved list.
    let repo = TestRepo::seeded()?;
    for &name in RESERVED_MESH_NAMES {
        let out = repo.run_mesh(["add", name, "file1.txt#L1-L5"])?;
        assert!(!out.status.success(), "reserved name `{name}` was accepted");
    }
    Ok(())
}

#[test]

fn doctor_runs_clean_on_fresh_repo_with_hooks() -> Result<()> {
    let repo = TestRepo::seeded()?;
    // Install both suggested hooks + file-index so doctor is finding-free.
    install_hooks(&repo)?;
    // Force file-index creation via `ls`.
    repo.mesh_stdout(["ls"])?;
    let out = repo.run_mesh(["doctor"])?;
    assert_eq!(
        out.status.code(),
        Some(0),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    Ok(())
}

fn install_hooks(repo: &TestRepo) -> Result<()> {
    let hooks = repo.path().join(".git").join("hooks");
    std::fs::create_dir_all(&hooks)?;
    std::fs::write(hooks.join("post-commit"), "#!/bin/sh\ngit mesh commit\n")?;
    std::fs::write(hooks.join("pre-commit"), "#!/bin/sh\ngit mesh status --check\n")?;
    Ok(())
}

#[test]
fn doctor_flags_missing_hooks() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let out = repo.run_mesh(["doctor"])?;
    let s = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(s.contains("MissingPostCommitHook"), "stdout={s}");
    assert!(s.contains("MissingPreCommitHook"), "stdout={s}");
    assert_eq!(out.status.code(), Some(1));
    Ok(())
}

#[test]
fn doctor_flags_malformed_staging_line() -> Result<()> {
    let repo = TestRepo::seeded()?;
    install_hooks(&repo)?;
    let staging = repo.path().join(".git").join("mesh").join("staging");
    std::fs::create_dir_all(&staging)?;
    std::fs::write(staging.join("bad"), "garbage line here\n")?;
    let out = repo.run_mesh(["doctor"])?;
    let s = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(s.contains("StagingCorrupt"), "stdout={s}");
    assert_eq!(out.status.code(), Some(1));
    Ok(())
}

#[test]
fn doctor_flags_missing_sidecar() -> Result<()> {
    let repo = TestRepo::seeded()?;
    install_hooks(&repo)?;
    repo.mesh_stdout(["add", "m", "file1.txt#L1-L5"])?;
    // Remove the sidecar file to simulate corruption.
    let sidecar = repo
        .path()
        .join(".git")
        .join("mesh")
        .join("staging")
        .join("m.1");
    std::fs::remove_file(&sidecar)?;
    let out = repo.run_mesh(["doctor"])?;
    let s = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(s.contains("StagingCorrupt"), "stdout={s}");
    assert!(s.contains("missing sidecar"), "stdout={s}");
    Ok(())
}

#[test]
fn doctor_flags_orphan_sidecar() -> Result<()> {
    let repo = TestRepo::seeded()?;
    install_hooks(&repo)?;
    let staging = repo.path().join(".git").join("mesh").join("staging");
    std::fs::create_dir_all(&staging)?;
    std::fs::write(staging.join("ghost.1"), b"orphan")?;
    let out = repo.run_mesh(["doctor"])?;
    let s = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(s.contains("StagingCorrupt"), "stdout={s}");
    assert!(s.contains("orphan"), "stdout={s}");
    Ok(())
}

#[test]
fn doctor_self_heals_missing_file_index() -> Result<()> {
    let repo = TestRepo::seeded()?;
    install_hooks(&repo)?;
    seed(&repo, "m")?;
    // Delete the file index to force the self-heal path.
    let idx = repo.path().join(".git").join("mesh").join("file-index");
    if idx.exists() {
        std::fs::remove_file(&idx)?;
    }
    let out = repo.run_mesh(["doctor"])?;
    let s = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(s.contains("FileIndexMissing"), "stdout={s}");
    assert!(s.contains("FileIndexRebuilt"), "stdout={s}");
    assert!(idx.exists(), "self-heal should regenerate the index");
    Ok(())
}

#[test]
fn doctor_flags_dangling_range_ref() -> Result<()> {
    let repo = TestRepo::seeded()?;
    install_hooks(&repo)?;
    seed(&repo, "m")?;
    // Write a dummy range ref pointing at an existing blob so the ref is
    // syntactically valid. Easiest: reuse the commit sha of HEAD as the value.
    let head = repo.head_sha()?;
    repo.run_git([
        "update-ref",
        "refs/ranges/v1/dangling-test-id",
        &head,
    ])?;
    let out = repo.run_mesh(["doctor"])?;
    let s = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(s.contains("DanglingRangeRef"), "stdout={s}");
    Ok(())
}

#[test]

fn doctor_flags_missing_refspec() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let bare = BareRepo::new()?;
    repo.add_remote("origin", bare.path())?;
    // origin has no mesh refspec — doctor should report a finding.
    let out = repo.run_mesh(["doctor"])?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let combined = format!("{stdout}{stderr}");
    assert!(combined.contains("refspec") || combined.to_lowercase().contains("remote"));
    Ok(())
}

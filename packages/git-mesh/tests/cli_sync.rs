//! CLI: fetch, push (§7).

mod support;

use anyhow::Result;
use support::{BareRepo, TestRepo};

fn seed(repo: &TestRepo, name: &str) -> Result<()> {
    repo.mesh_stdout(["add", name, "file1.txt#L1-L5"])?;
    repo.mesh_stdout(["message", name, "-m", "seed"])?;
    repo.mesh_stdout(["commit", name])?;
    Ok(())
}

#[test]

fn push_with_missing_remote_errors() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed(&repo, "m")?;
    let out = repo.run_mesh(["push", "absent"])?;
    assert!(!out.status.success());
    Ok(())
}

#[test]

fn push_bootstraps_refspec_on_first_push() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let bare = BareRepo::new()?;
    repo.add_remote("origin", bare.path())?;
    seed(&repo, "m")?;
    repo.mesh_stdout(["push"])?; // default remote
    let fetch = repo.git_stdout(["config", "--get-all", "remote.origin.fetch"])?;
    assert!(fetch.contains("refs/meshes/"));
    Ok(())
}

#[test]

fn push_delivers_mesh_to_upstream() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let bare = BareRepo::new()?;
    repo.add_remote("origin", bare.path())?;
    seed(&repo, "m")?;
    repo.mesh_stdout(["push", "origin"])?;
    let out = std::process::Command::new("git")
        .current_dir(bare.path())
        .args(["for-each-ref", "--format=%(refname)", "refs/meshes/"])
        .output()?;
    assert!(String::from_utf8_lossy(&out.stdout).contains("refs/meshes/v1/m"));
    Ok(())
}

#[test]

fn fetch_delivers_mesh_from_upstream() -> Result<()> {
    let bare = BareRepo::new()?;
    let writer = TestRepo::seeded()?;
    writer.add_remote("origin", bare.path())?;
    seed(&writer, "shared")?;
    writer.mesh_stdout(["push", "origin"])?;

    let reader = TestRepo::seeded()?;
    reader.add_remote("origin", bare.path())?;
    reader.mesh_stdout(["fetch", "origin"])?;
    assert!(reader.ref_exists("refs/meshes/v1/shared"));
    Ok(())
}

#[test]

fn fetch_uses_default_remote() -> Result<()> {
    let bare = BareRepo::new()?;
    let writer = TestRepo::seeded()?;
    writer.add_remote("origin", bare.path())?;
    seed(&writer, "shared")?;
    writer.mesh_stdout(["push", "origin"])?;

    let reader = TestRepo::seeded()?;
    reader.add_remote("origin", bare.path())?;
    reader.mesh_stdout(["fetch"])?;
    assert!(reader.ref_exists("refs/meshes/v1/shared"));
    Ok(())
}

#[test]

fn fetch_honors_default_remote_config() -> Result<()> {
    let bare = BareRepo::new()?;
    let writer = TestRepo::seeded()?;
    writer.add_remote("upstream", bare.path())?;
    writer.run_git(["config", "mesh.defaultRemote", "upstream"])?;
    seed(&writer, "shared")?;
    writer.mesh_stdout(["push"])?;
    let out = std::process::Command::new("git")
        .current_dir(bare.path())
        .args(["for-each-ref", "--format=%(refname)", "refs/meshes/"])
        .output()?;
    assert!(String::from_utf8_lossy(&out.stdout).contains("refs/meshes/v1/shared"));
    Ok(())
}

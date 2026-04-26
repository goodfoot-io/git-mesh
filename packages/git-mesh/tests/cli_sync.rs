//! CLI: fetch, push (§7).

mod support;

use anyhow::Result;
use support::{BareRepo, TestRepo};

fn seed(repo: &TestRepo, name: &str) -> Result<()> {
    repo.mesh_stdout(["add", name, "file1.txt#L1-L5"])?;
    repo.mesh_stdout(["why", name, "-m", "seed"])?;
    repo.mesh_stdout(["commit", name])?;
    Ok(())
}

#[test]

fn push_with_missing_remote_errors() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed(&repo, "m")?;
    let out = repo.run_mesh(["push", "absent"])?;
    assert_eq!(
        out.status.code(),
        Some(1),
        "operational failure must exit 1, not 2 (clap usage)"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("remote not found: absent"),
        "stderr={stderr}"
    );
    Ok(())
}

#[test]
fn fetch_usage_error_exits_two() -> Result<()> {
    // Clap usage error (unknown flag) keeps exit 2 — the standard
    // POSIX convention used by `git`, `cargo`, etc.
    let repo = TestRepo::seeded()?;
    let out = repo.run_mesh(["fetch", "--bogus"])?;
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("unexpected argument") || stderr.contains("--bogus"),
        "stderr={stderr}"
    );
    Ok(())
}

#[test]
fn fetch_runtime_missing_remote_exits_one() -> Result<()> {
    // Runtime/operational failure (remote not configured) exits 1,
    // distinct from the clap usage exit-2.
    let repo = TestRepo::seeded()?;
    let out = repo.run_mesh(["fetch", "absent"])?;
    assert_eq!(out.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("remote not found: absent"),
        "stderr={stderr}"
    );
    Ok(())
}

#[test]
fn default_origin_missing_remote_errors_as_missing_remote() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let out = repo.run_mesh(["fetch"])?;
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("remote not found: origin"),
        "stderr={stderr}"
    );
    assert!(
        !stderr.contains("refspec missing"),
        "missing remote should not be reported as missing refspec: {stderr}"
    );
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

//! Sync tests (§7).

mod support;

use anyhow::Result;
use git_mesh::{
    append_add, commit_mesh, default_remote, ensure_refspec_configured, fetch_mesh_refs,
    push_mesh_refs, set_message,
};
use support::{BareRepo, TestRepo};

#[test]

fn default_remote_falls_back_to_origin() -> Result<()> {
    let repo = TestRepo::seeded()?;
    assert_eq!(default_remote(&repo.gix_repo()?)?, "origin");
    Ok(())
}

#[test]

fn default_remote_honours_config_override() -> Result<()> {
    let repo = TestRepo::seeded()?;
    repo.run_git(["config", "mesh.defaultRemote", "upstream"])?;
    assert_eq!(default_remote(&repo.gix_repo()?)?, "upstream");
    Ok(())
}

#[test]

fn ensure_refspec_configured_adds_both_refspecs() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let bare = BareRepo::new()?;
    repo.add_remote("origin", bare.path())?;
    ensure_refspec_configured(&repo.gix_repo()?, "origin")?;
    let fetch = repo.git_stdout(["config", "--get-all", "remote.origin.fetch"])?;
    assert!(fetch.contains("refs/meshes/"));
    assert!(fetch.contains("refs/ranges/"));
    Ok(())
}

#[test]

fn ensure_refspec_configured_is_idempotent() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let bare = BareRepo::new()?;
    repo.add_remote("origin", bare.path())?;
    ensure_refspec_configured(&repo.gix_repo()?, "origin")?;
    ensure_refspec_configured(&repo.gix_repo()?, "origin")?;
    let fetch = repo.git_stdout(["config", "--get-all", "remote.origin.fetch"])?;
    // Each refspec line appears exactly once.
    let mesh_count = fetch.matches("refs/meshes/").count();
    assert_eq!(mesh_count, 2, "one fetch + one push refspec per family");
    Ok(())
}

#[test]

fn ensure_refspec_errors_on_missing_remote() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let err = ensure_refspec_configured(&repo.gix_repo()?, "absent").unwrap_err();
    assert!(matches!(err, git_mesh::Error::RefspecMissing { .. }));
    Ok(())
}

#[test]

fn push_bootstraps_refspec_on_first_call() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let bare = BareRepo::new()?;
    repo.add_remote("origin", bare.path())?;
    let gix = repo.gix_repo()?;
    append_add(&gix, "m", "file1.txt", 1, 5, None)?;
    set_message(&gix, "m", "seed")?;
    commit_mesh(&gix, "m")?;
    push_mesh_refs(&gix, "origin")?;
    // Upstream should now have the mesh ref.
    let out = std::process::Command::new("git")
        .current_dir(bare.path())
        .args(["for-each-ref", "--format=%(refname)", "refs/meshes/"])
        .output()?;
    let refs = String::from_utf8_lossy(&out.stdout);
    assert!(refs.contains("refs/meshes/v1/m"));
    Ok(())
}

#[test]

fn fetch_round_trips_from_upstream() -> Result<()> {
    // Push from one clone, fetch into a second. Round-trips mesh and
    // range refs via the configured refspecs.
    let upstream_bare = BareRepo::new()?;
    let writer = TestRepo::seeded()?;
    writer.add_remote("origin", upstream_bare.path())?;
    let wg = writer.gix_repo()?;
    append_add(&wg, "shared", "file1.txt", 1, 5, None)?;
    set_message(&wg, "shared", "seed")?;
    commit_mesh(&wg, "shared")?;
    push_mesh_refs(&wg, "origin")?;

    let reader = TestRepo::seeded()?;
    reader.add_remote("origin", upstream_bare.path())?;
    fetch_mesh_refs(&reader.gix_repo()?, "origin")?;
    assert!(reader.ref_exists("refs/meshes/v1/shared"));
    Ok(())
}

//! Integration tests for Slices 5 (`--since`) and 6 (minor quality
//! issues) of `docs/git-mesh-review-plan.md`.

mod support;

use anyhow::Result;
use git_mesh::types::EngineOptions;
use git_mesh::{append_add, commit_mesh, resolve_mesh, set_why};
use std::str::FromStr;
use support::TestRepo;

// ---------------------------------------------------------------------------
// Slice 5 — `--since` filter.
// ---------------------------------------------------------------------------

#[test]
fn since_filters_ranges_anchored_before_cutoff() -> Result<()> {
    let repo = TestRepo::new()?;
    repo.write_file("a.txt", "alpha\n")?;
    let seed = repo.commit_all("seed")?;
    repo.write_file("a.txt", "beta\n")?;
    let later = repo.commit_all("later")?;

    let gix = repo.gix_repo()?;
    // mesh_old anchored at the seed commit.
    git_mesh::staging::append_add(&gix, "old", "a.txt", 1, 1, Some(&seed))?;
    set_why(&gix, "old", "old")?;
    commit_mesh(&gix, "old")?;
    // mesh_new anchored at the later commit.
    git_mesh::staging::append_add(&gix, "new", "a.txt", 1, 1, Some(&later))?;
    set_why(&gix, "new", "new")?;
    commit_mesh(&gix, "new")?;

    let later_oid = gix::ObjectId::from_str(&later)?;
    let opts = EngineOptions {
        since: Some(later_oid),
        ..EngineOptions::full()
    };
    let mr_old = resolve_mesh(&gix, "old", opts)?;
    let mr_new = resolve_mesh(&gix, "new", opts)?;
    assert!(
        mr_old.anchors.is_empty(),
        "old mesh should be filtered: {:?}",
        mr_old.anchors
    );
    assert_eq!(mr_new.anchors.len(), 1, "new mesh should pass the filter");
    Ok(())
}

#[test]
fn since_head_includes_anchors_at_head() -> Result<()> {
    let repo = TestRepo::new()?;
    repo.write_file("a.txt", "alpha\n")?;
    let head = repo.commit_all("seed")?;
    let gix = repo.gix_repo()?;
    git_mesh::staging::append_add(&gix, "m", "a.txt", 1, 1, Some(&head))?;
    set_why(&gix, "m", "x")?;
    commit_mesh(&gix, "m")?;
    let opts = EngineOptions {
        since: Some(gix::ObjectId::from_str(&head)?),
        ..EngineOptions::full()
    };
    let mr = resolve_mesh(&gix, "m", opts)?;
    assert_eq!(mr.anchors.len(), 1, "since == anchor should be inclusive");
    Ok(())
}

#[test]
fn since_bad_ref_errors_cleanly() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let out = repo.run_mesh(["stale", "--since", "definitely-not-a-ref"])?;
    assert!(!out.status.success(), "expected non-zero exit");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("--since") || err.contains("rev-parse"),
        "stderr should mention --since: {err}"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Slice 6b — content-blind binary detection.
// ---------------------------------------------------------------------------

#[test]
fn nul_bearing_file_rejects_line_range_add() -> Result<()> {
    let repo = TestRepo::new()?;
    // Write a "binary" file with no .gitattributes binary annotation.
    repo.write_file("data.bin", "")?;
    std::fs::write(repo.path().join("data.bin"), b"abc\0def\n")?;
    repo.commit_all("seed")?;
    let out = repo.run_mesh(["add", "m", "data.bin#L1-L1"])?;
    assert!(
        !out.status.success(),
        "line-anchor add on NUL file must fail"
    );
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("binary"),
        "expected binary rejection, got: {err}"
    );
    // Whole-file add should still succeed.
    let out2 = repo.run_mesh(["add", "m", "data.bin"])?;
    assert!(
        out2.status.success(),
        "whole-file add should accept NUL content: {}",
        String::from_utf8_lossy(&out2.stderr)
    );
    Ok(())
}

#[test]
fn non_utf8_file_rejects_line_range_add() -> Result<()> {
    let repo = TestRepo::new()?;
    std::fs::write(repo.path().join("image.bin"), b"\x89PNG\r\n\x1a\noldbinary")?;
    repo.run_git(["add", "image.bin"])?;
    repo.run_git(["commit", "-m", "seed"])?;

    let out = repo.run_mesh(["add", "m", "image.bin#L1-L1"])?;
    assert!(
        !out.status.success(),
        "line-anchor add on non-UTF-8 content must fail"
    );
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("binary"),
        "expected binary rejection, got: {err}"
    );

    let whole = repo.run_mesh(["add", "m", "image.bin"])?;
    assert!(
        whole.status.success(),
        "whole-file pin should accept non-UTF-8 content: {}",
        String::from_utf8_lossy(&whole.stderr)
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Slice 6d — reflog coverage for mesh refs.
// ---------------------------------------------------------------------------

#[test]
fn first_mesh_commit_sets_log_all_ref_updates_always() -> Result<()> {
    let repo = TestRepo::seeded()?;
    // Default config doesn't set core.logAllRefUpdates = always.
    let _ = repo.run_git(["config", "--unset", "core.logAllRefUpdates"]);
    let gix = repo.gix_repo()?;
    append_add(&gix, "m", "file1.txt", 1, 5, None)?;
    set_why(&gix, "m", "x")?;
    commit_mesh(&gix, "m")?;
    let val = repo.git_stdout(["config", "--local", "core.logAllRefUpdates"])?;
    assert_eq!(val, "always");
    // Reflog file exists for the mesh ref.
    let reflog = repo.path().join(".git/logs/refs/meshes/v1/m");
    assert!(reflog.exists(), "reflog should exist at {reflog:?}");
    Ok(())
}

// ---------------------------------------------------------------------------
// Slice 6e — `--at` ordering.
// ---------------------------------------------------------------------------

#[test]
fn at_ordering_produces_identical_anchor() -> Result<()> {
    let repo = TestRepo::new()?;
    repo.write_file("f.txt", "a\nb\nc\n")?;
    let seed = repo.commit_all("v1")?;
    repo.write_file("f.txt", "X\nY\nZ\n")?;
    repo.commit_all("v2")?;

    // Order A: anchor before --at.
    repo.run_mesh(["why", "ma", "-m", "x"])?
        .status
        .success()
        .then_some(())
        .unwrap();
    let out_a = repo.run_mesh(["add", "ma", "f.txt#L1-L1", "--at", &seed])?;
    assert!(
        out_a.status.success(),
        "{}",
        String::from_utf8_lossy(&out_a.stderr)
    );

    // Order B: --at before anchor.
    repo.run_mesh(["why", "mb", "-m", "x"])?
        .status
        .success()
        .then_some(())
        .unwrap();
    let out_b = repo.run_mesh(["add", "mb", "--at", &seed, "f.txt#L1-L1"])?;
    assert!(
        out_b.status.success(),
        "{}",
        String::from_utf8_lossy(&out_b.stderr)
    );

    let staging_a = std::fs::read_to_string(repo.path().join(".git/mesh/staging/ma"))?;
    let staging_b = std::fs::read_to_string(repo.path().join(".git/mesh/staging/mb"))?;
    // Strip the mesh name; the line content should be identical.
    assert_eq!(
        staging_a.trim(),
        staging_b.trim(),
        "anchors differ across orderings"
    );
    assert!(
        staging_a.contains(&seed),
        "staged anchor should be the resolved seed OID"
    );
    Ok(())
}

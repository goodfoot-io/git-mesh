//! Integration tests for `git mesh stale --auto-follow`.
//!
//! Covers the seven behavioural criteria from the Phase B scope:
//!   1. Verbatim move + `--auto-follow` → rewrites mesh, next run Fresh.
//!   2. Verbatim move + `follow-moves true` config → rewrites without flag.
//!   3. Changed sibling → entire mesh suppressed (no follow).
//!   4. Path rename → does not auto-follow (path changed).
//!   5. Blob mismatch (content changed inside span) → does not auto-follow.
//!   6. Commit message is exactly `mesh: follow N moved anchors`.
//!   7. Without flag and follow-moves=false → no rewrite (arrow only).

mod support;

use anyhow::Result;
use support::TestRepo;

/// Set up a repo with a single mesh anchoring `file1.txt#L1-L5`, then
/// commit a shift that prepends two lines (verbatim move to L3-L7).
fn setup_verbatim_move(repo: &TestRepo) -> Result<()> {
    repo.mesh_stdout(["add", "m", "file1.txt#L1-L5"])?;
    repo.mesh_stdout(["why", "m", "-m", "seed"])?;
    repo.mesh_stdout(["commit", "m"])?;
    // Prepend two lines — the anchored content moves verbatim to L3-L7.
    repo.write_file(
        "file1.txt",
        "prefix1\nprefix2\nline1\nline2\nline3\nline4\nline5\nline6\nline7\nline8\nline9\nline10\n",
    )?;
    repo.commit_all("shift")?;
    Ok(())
}

// ---------------------------------------------------------------------------
// 1. --auto-follow rewrites mesh; next run reports Fresh
// ---------------------------------------------------------------------------

#[test]
fn auto_follow_flag_rewrites_mesh_next_run_fresh() -> Result<()> {
    let repo = TestRepo::seeded()?;
    setup_verbatim_move(&repo)?;

    // First run with --auto-follow: should exit 0 (followed anchor
    // subtracted from stale count) and annotate the row.
    let out = repo.run_mesh(["stale", "m", "--auto-follow"])?;
    assert_eq!(
        out.status.code(),
        Some(0),
        "exit 0 after auto-follow; stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("anchor automatically updated"),
        "annotation missing; stdout={stdout}"
    );
    assert!(
        stdout.contains("→"),
        "arrow missing; stdout={stdout}"
    );

    // Second run (no flag): anchor should now be Fresh.
    let out2 = repo.run_mesh(["stale", "m"])?;
    assert_eq!(
        out2.status.code(),
        Some(0),
        "next run must exit 0 (Fresh); stdout={}",
        String::from_utf8_lossy(&out2.stdout)
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// 2. follow-moves=true in mesh config → rewrites without flag
// ---------------------------------------------------------------------------

#[test]
fn follow_moves_config_rewrites_without_flag() -> Result<()> {
    let repo = TestRepo::seeded()?;
    // Set follow-moves before seeding the anchor.
    repo.mesh_stdout(["add", "m", "file1.txt#L1-L5"])?;
    repo.mesh_stdout(["why", "m", "-m", "seed"])?;
    repo.mesh_stdout(["commit", "m"])?;
    // Enable follow-moves in config.
    repo.mesh_stdout(["config", "m", "follow-moves", "true"])?;
    // Verbatim move.
    repo.write_file(
        "file1.txt",
        "prefix1\nprefix2\nline1\nline2\nline3\nline4\nline5\nline6\nline7\nline8\nline9\nline10\n",
    )?;
    repo.commit_all("shift")?;

    // Run without --auto-follow; config should trigger follow.
    let out = repo.run_mesh(["stale", "m"])?;
    assert_eq!(
        out.status.code(),
        Some(0),
        "config-driven follow must exit 0; stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("anchor automatically updated"),
        "annotation missing; stdout={stdout}"
    );

    // Next run is Fresh.
    let out2 = repo.run_mesh(["stale", "m"])?;
    assert_eq!(out2.status.code(), Some(0), "next run must be Fresh");
    Ok(())
}

// ---------------------------------------------------------------------------
// 3. Changed sibling suppresses entire mesh
// ---------------------------------------------------------------------------

#[test]
fn changed_sibling_suppresses_auto_follow() -> Result<()> {
    let repo = TestRepo::seeded()?;
    // Two anchors: one will Move, the other will Change.
    repo.mesh_stdout(["add", "m", "file1.txt#L1-L5"])?;
    repo.mesh_stdout(["add", "m", "file1.txt#L6-L10"])?;
    repo.mesh_stdout(["why", "m", "-m", "seed"])?;
    repo.mesh_stdout(["commit", "m"])?;

    // Mutate L6-L10 (Changed) and prepend lines so L1-L5 moves.
    repo.write_file(
        "file1.txt",
        "prefix1\nprefix2\nline1\nline2\nline3\nline4\nline5\nXXX\nline8\nline9\nline10\nline11\nline12\n",
    )?;
    repo.commit_all("change sibling + shift")?;

    // auto-follow: should NOT rewrite because of Changed sibling.
    let out = repo.run_mesh(["stale", "m", "--auto-follow"])?;
    // Exit code 1: stale anchors remain unrewritten.
    assert_eq!(
        out.status.code(),
        Some(1),
        "Changed sibling must suppress follow; stdout={}",
        String::from_utf8_lossy(&out.stdout)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        !stdout.contains("anchor automatically updated"),
        "must not annotate when suppressed; stdout={stdout}"
    );

    // Next run: still stale (no rewrite occurred).
    let out2 = repo.run_mesh(["stale", "m", "--auto-follow"])?;
    assert_eq!(out2.status.code(), Some(1), "still stale after suppressed follow");
    Ok(())
}

// ---------------------------------------------------------------------------
// 4. Path rename → does not auto-follow
// ---------------------------------------------------------------------------

#[test]
fn path_rename_does_not_auto_follow() -> Result<()> {
    let repo = TestRepo::seeded()?;
    repo.mesh_stdout(["add", "m", "file1.txt#L1-L5"])?;
    repo.mesh_stdout(["why", "m", "-m", "seed"])?;
    repo.mesh_stdout(["commit", "m"])?;

    // Rename file1.txt → renamed.txt (path changed).
    repo.run_git(["mv", "file1.txt", "renamed.txt"])?;
    repo.commit_all("rename")?;

    let out = repo.run_mesh(["stale", "m", "--auto-follow"])?;
    // Path changed → guardrail blocks auto-follow → still Moved → exit 1.
    assert_eq!(
        out.status.code(),
        Some(1),
        "path rename must not auto-follow; stdout={}",
        String::from_utf8_lossy(&out.stdout)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        !stdout.contains("anchor automatically updated"),
        "must not annotate path-rename Moved; stdout={stdout}"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// 5. Blob mismatch → does not auto-follow
// ---------------------------------------------------------------------------

#[test]
fn blob_mismatch_does_not_auto_follow() -> Result<()> {
    let repo = TestRepo::seeded()?;
    repo.mesh_stdout(["add", "m", "file1.txt#L1-L5"])?;
    repo.mesh_stdout(["why", "m", "-m", "seed"])?;
    repo.mesh_stdout(["commit", "m"])?;

    // Prepend two lines AND modify one of the pinned lines.
    repo.write_file(
        "file1.txt",
        "prefix1\nprefix2\nMODIFIED\nline2\nline3\nline4\nline5\nline6\nline7\nline8\nline9\nline10\n",
    )?;
    repo.commit_all("shift + modify")?;

    let out = repo.run_mesh(["stale", "m", "--auto-follow"])?;
    // Blob differs → guardrail blocks → still stale → exit 1.
    assert_eq!(
        out.status.code(),
        Some(1),
        "blob mismatch must not auto-follow; stdout={}",
        String::from_utf8_lossy(&out.stdout)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        !stdout.contains("anchor automatically updated"),
        "must not annotate blob-mismatch; stdout={stdout}"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// 6. Commit message is exactly `mesh: follow N moved anchors`
// ---------------------------------------------------------------------------

#[test]
fn audit_trail_commit_message_format() -> Result<()> {
    let repo = TestRepo::seeded()?;
    setup_verbatim_move(&repo)?;

    repo.mesh_stdout(["stale", "m", "--auto-follow"])?;

    // Read the mesh tip commit message.
    let msg = repo.git_stdout([
        "log",
        "-1",
        "--format=%s",
        "refs/meshes/v1/m",
    ])?;
    assert_eq!(
        msg.trim(),
        "mesh: follow 1 moved anchors",
        "commit message mismatch: {msg}"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// 7. Without --auto-follow and follow-moves=false → arrow rendered, no rewrite
// ---------------------------------------------------------------------------

#[test]
fn no_flag_no_config_renders_arrow_only() -> Result<()> {
    let repo = TestRepo::seeded()?;
    setup_verbatim_move(&repo)?;

    // Plain stale (no --auto-follow, follow-moves defaults to false).
    let out = repo.run_mesh(["stale", "m"])?;
    assert_eq!(out.status.code(), Some(1), "still stale without flag");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("→"), "arrow must still render; stdout={stdout}");
    assert!(
        !stdout.contains("anchor automatically updated"),
        "must not annotate without flag; stdout={stdout}"
    );

    // Mesh ref tip must still have the original commit (no rewrite).
    let second = repo.run_mesh(["stale", "m"])?;
    assert_eq!(second.status.code(), Some(1), "still stale on second run");
    Ok(())
}

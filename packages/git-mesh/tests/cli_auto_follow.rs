//! Integration tests for `git mesh stale --auto-follow`.
//!
//! Covers the seven behavioural criteria from the Phase B scope:
//!   1. Verbatim move + `--auto-follow` → rewrites mesh, next run Fresh.
//!   2. Verbatim move + `follow-moves true` config → rewrites without flag.
//!   3. Changed sibling → entire mesh suppressed (no follow).
//!   4. Path rename → does not auto-follow (path changed).
//!   5. Blob mismatch (content changed inside span) → does not auto-follow.
//!   6. Original why is preserved in the new mesh commit (not overwritten).
//!   7. Without flag and follow-moves=false → no rewrite (arrow only).
//!   8. Whole-file anchor is not auto-followed.
//!   9. current.blob == None does not auto-follow.

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
// 6. Original why is preserved in the new mesh commit
// ---------------------------------------------------------------------------

#[test]
fn audit_trail_commit_message_format() -> Result<()> {
    let repo = TestRepo::seeded()?;
    // setup_verbatim_move writes why "seed" before the shift commit.
    setup_verbatim_move(&repo)?;

    // Read the mesh tip commit message before auto-follow.
    let original_msg = repo.git_stdout([
        "log",
        "-1",
        "--format=%B",
        "refs/meshes/v1/m",
    ])?;

    repo.mesh_stdout(["stale", "m", "--auto-follow"])?;

    // The raw mesh commit message must have the original why + Mesh-Follow trailer.
    let raw_msg = repo.git_stdout([
        "log",
        "-1",
        "--format=%B",
        "refs/meshes/v1/m",
    ])?;
    assert!(
        raw_msg.contains(original_msg.trim()),
        "auto-follow commit must contain the original why; raw_msg={raw_msg}"
    );
    assert!(
        raw_msg.contains("Mesh-Follow:"),
        "auto-follow commit must contain Mesh-Follow trailer; raw_msg={raw_msg}"
    );

    // git mesh show must return only the original why (no trailer leak).
    let show_out = repo.mesh_stdout(["show", "m"])?;
    assert!(
        show_out.contains(original_msg.trim()),
        "git mesh show must include original why; show_out={show_out}"
    );
    assert!(
        !show_out.contains("Mesh-Follow:"),
        "git mesh show must not expose the Mesh-Follow trailer; show_out={show_out}"
    );

    // git log --grep must find the follow commit via the trailer.
    let grep_out = repo.git_stdout([
        "log",
        "refs/meshes/v1/m",
        "--grep=Mesh-Follow",
        "--format=%H",
    ])?;
    assert!(
        !grep_out.trim().is_empty(),
        "git log --grep=Mesh-Follow must find the follow commit"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// 10. Subsequent normal mesh commit does not inherit the Mesh-Follow trailer
// ---------------------------------------------------------------------------

#[test]
fn subsequent_commit_does_not_inherit_trailer() -> Result<()> {
    let repo = TestRepo::seeded()?;
    setup_verbatim_move(&repo)?;

    // auto-follow writes the trailer into the mesh commit.
    repo.mesh_stdout(["stale", "m", "--auto-follow"])?;

    // Now re-anchor: add a new anchor and commit — no explicit `-m` so the
    // message is inherited from the current mesh tip (which has the trailer).
    repo.mesh_stdout(["add", "m", "file1.txt#L1-L2"])?;
    repo.mesh_stdout(["commit", "m"])?;

    // The new mesh commit must NOT contain the Mesh-Follow trailer.
    let new_msg = repo.git_stdout([
        "log",
        "-1",
        "--format=%B",
        "refs/meshes/v1/m",
    ])?;
    assert!(
        !new_msg.contains("Mesh-Follow:"),
        "normal mesh commit must not carry Mesh-Follow trailer; msg={new_msg}"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// 8. Whole-file anchor is not auto-followed
// ---------------------------------------------------------------------------

#[test]
fn whole_file_anchor_not_auto_followed() -> Result<()> {
    let repo = TestRepo::seeded()?;
    // Anchor the whole file (no line range).
    repo.mesh_stdout(["add", "m", "file1.txt"])?;
    repo.mesh_stdout(["why", "m", "-m", "seed"])?;
    repo.mesh_stdout(["commit", "m"])?;

    // Prepend lines — whole-file anchor would become Moved at the file level,
    // but whole-file anchors are excluded from auto-follow.
    repo.write_file(
        "file1.txt",
        "prefix1\nprefix2\nline1\nline2\nline3\nline4\nline5\nline6\nline7\nline8\nline9\nline10\n",
    )?;
    repo.commit_all("shift")?;

    let out = repo.run_mesh(["stale", "m", "--auto-follow"])?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        !stdout.contains("anchor automatically updated"),
        "whole-file anchor must not be auto-followed; stdout={stdout}"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// 9. current.blob == None (worktree-only shift) does not auto-follow
// ---------------------------------------------------------------------------

#[test]
fn worktree_only_move_does_not_auto_follow() -> Result<()> {
    let repo = TestRepo::seeded()?;
    repo.mesh_stdout(["add", "m", "file1.txt#L1-L5"])?;
    repo.mesh_stdout(["why", "m", "-m", "seed"])?;
    repo.mesh_stdout(["commit", "m"])?;

    // Prepend lines in the worktree only — do NOT commit or stage.
    // The anchor content moves verbatim to L3-L7 in the worktree.
    // The resolver will detect a Moved with current.blob == None (worktree
    // layer, no blob committed to git).
    repo.write_file(
        "file1.txt",
        "prefix1\nprefix2\nline1\nline2\nline3\nline4\nline5\nline6\nline7\nline8\nline9\nline10\n",
    )?;
    // Do NOT call commit_all — leave the change as a worktree-only modification.

    let out = repo.run_mesh(["stale", "m", "--auto-follow"])?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    // The anchor may report Moved (worktree source) but must not be rewritten
    // because current.blob is None — the blob is not committed to git.
    assert!(
        !stdout.contains("anchor automatically updated"),
        "worktree-only move (current.blob=None) must not be auto-followed; stdout={stdout}"
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

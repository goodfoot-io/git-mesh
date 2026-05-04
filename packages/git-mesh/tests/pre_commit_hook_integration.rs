//! Phase 4 acceptance tests for `git mesh pre-commit`.
//!
//! Each test maps to a bullet under `docs/stale-layers-plan.md`
//! §"Phase 4". The hook runs the engine with `LayerSet { worktree:
//! false, index: true, staged_mesh: true }`, filters to findings
//! touching the staged path set, and fails iff index drift is unacked
//! or a pending Add/Remove has SidecarMismatch.

mod support;

use anyhow::Result;
use git_mesh::{append_add, commit_mesh, set_why};
use support::TestRepo;

fn seed_line_range_mesh(repo: &TestRepo, mesh: &str) -> Result<()> {
    let gix = repo.gix_repo()?;
    append_add(&gix, mesh, "file1.txt", 1, 5, None)?;
    set_why(&gix, mesh, "seed")?;
    commit_mesh(&gix, mesh)?;
    Ok(())
}

/// Index drift on a pinned anchor, no acknowledgment → exit 1.
#[test]
fn index_drift_unacked_fails_commit() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed_line_range_mesh(&repo, "m")?;
    repo.write_file(
        "file1.txt",
        "lineONE\nline2\nline3\nline4\nline5\nline6\nline7\nline8\nline9\nline10\n",
    )?;
    repo.run_git(["add", "file1.txt"])?;
    let out = repo.run_mesh(["pre-commit"])?;
    assert_eq!(
        out.status.code(),
        Some(1),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    Ok(())
}

/// Index drift with a matching staged re-anchor → exit 0.
#[test]
fn index_drift_with_ack_passes() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed_line_range_mesh(&repo, "m")?;
    repo.write_file(
        "file1.txt",
        "lineONE\nline2\nline3\nline4\nline5\nline6\nline7\nline8\nline9\nline10\n",
    )?;
    repo.run_git(["add", "file1.txt"])?;
    // Staged re-anchor matching the live (== staged) bytes acks via
    // anchor_id (see `pending::apply_acknowledgment`).
    let _ = repo.run_mesh(["add", "m", "file1.txt#L1-L5"])?;
    let out = repo.run_mesh(["pre-commit"])?;
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    Ok(())
}

/// Worktree-only drift (path not in the in-flight commit) → exit 0.
/// Per plan §"Phase 4": worktree drift is NOT a pre-commit failure.
#[test]
fn worktree_only_drift_does_not_fail_commit() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed_line_range_mesh(&repo, "m")?;
    // Edit but do not stage — the worktree differs from HEAD/Index.
    repo.write_file(
        "file1.txt",
        "lineONE\nline2\nline3\nline4\nline5\nline6\nline7\nline8\nline9\nline10\n",
    )?;
    let out = repo.run_mesh(["pre-commit"])?;
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    Ok(())
}

/// Pending Add with `SidecarMismatch` on a staged path → exit 1.
#[test]
fn pending_add_sidecar_mismatch_fails_commit() -> Result<()> {
    let repo = TestRepo::seeded()?;
    // Stage a `git mesh add` against the current worktree.
    let _ = repo.run_mesh(["add", "m", "file2.txt#L1-L5"])?;
    // Now mutate file2.txt and stage it. The sidecar bytes (captured
    // at `add` time) no longer match the live blob → SidecarMismatch.
    repo.write_file(
        "file2.txt",
        "DIFF1\nDIFF2\nDIFF3\nDIFF4\nDIFF5\nline6\nline7\nline8\nline9\nline10\nline11\nline12\nline13\nline14\nline15\nline16\n",
    )?;
    repo.run_git(["add", "file2.txt"])?;
    let out = repo.run_mesh(["pre-commit"])?;
    assert_eq!(
        out.status.code(),
        Some(1),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("+ ADD    file2.txt#L1-L5"),
        "stdout={stdout}"
    );
    assert!(
        !stdout.contains("file2.txt L1-L5"),
        "pending add should use anchor-address syntax: {stdout}"
    );
    Ok(())
}

/// Pending Remove with concurrent index drift on the same anchor: the
/// anchor's index drift drives the gate (the remove acknowledges its own
/// sidecar capture and is silent without independent drift).
#[test]
fn pending_remove_with_index_drift_fails_commit() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed_line_range_mesh(&repo, "m")?;
    let _ = repo.run_mesh(["remove", "m", "file1.txt#L1-L5"])?;
    repo.write_file(
        "file1.txt",
        "lineONE\nline2\nline3\nline4\nline5\nline6\nline7\nline8\nline9\nline10\n",
    )?;
    repo.run_git(["add", "file1.txt"])?;

    let out = repo.run_mesh(["pre-commit"])?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        out.status.code(),
        Some(1),
        "stdout={stdout}, stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        stdout.contains("Changed") && stdout.contains("file1.txt"),
        "expected index drift on file1.txt to be reported; stdout={stdout}"
    );
    Ok(())
}

/// Pending `Message` with no other drift → exit 0. Messages never
/// drive exit code (plan §B3).
#[test]
fn pending_message_only_does_not_fail_commit() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed_line_range_mesh(&repo, "m")?;
    let _ = repo.run_mesh(["why", "m", "-m", "informational note"])?;
    // Touch an unrelated path so there is *something* staged (otherwise
    // the in-flight commit is empty and the kept-set is empty for any
    // reason).
    repo.write_file("file2.txt", "additional line\n")?;
    repo.run_git(["add", "file2.txt"])?;
    let out = repo.run_mesh(["pre-commit"])?;
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    Ok(())
}

/// Pre-existing HEAD drift on a meshed path that the in-flight commit
/// does NOT touch must still fail — `<fail-closed>` requires any drift
/// the hook can see in the staged tree to gate the commit.
#[test]
fn pre_existing_head_drift_on_unstaged_meshed_path_fails_commit() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed_line_range_mesh(&repo, "m")?;
    // Mutate file1.txt and COMMIT it (drift now lives in HEAD, not Index).
    repo.write_file(
        "file1.txt",
        "lineONE\nline2\nline3\nline4\nline5\nline6\nline7\nline8\nline9\nline10\n",
    )?;
    repo.run_git(["add", "file1.txt"])?;
    repo.run_git(["commit", "-m", "drift the meshed anchor"])?;
    // In-flight commit touches a different path entirely.
    repo.write_file("file2.txt", "additional\n")?;
    repo.run_git(["add", "file2.txt"])?;
    let out = repo.run_mesh(["pre-commit"])?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        out.status.code(),
        Some(1),
        "expected exit 1 on pre-existing HEAD drift; stdout={stdout}"
    );
    assert!(
        stdout.contains("drift in staged tree"),
        "expected new header text; stdout={stdout}"
    );
    assert!(
        stdout.contains("pre-existing"),
        "expected origin tag; stdout={stdout}"
    );
    assert!(
        stdout.contains("hint:"),
        "expected resolution hint; stdout={stdout}"
    );
    Ok(())
}

/// `--no-exit-code` keeps the hook informational: drift is still printed
/// but the commit is allowed through.
#[test]
fn no_exit_code_flag_allows_commit_with_drift() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed_line_range_mesh(&repo, "m")?;
    repo.write_file(
        "file1.txt",
        "lineONE\nline2\nline3\nline4\nline5\nline6\nline7\nline8\nline9\nline10\n",
    )?;
    repo.run_git(["add", "file1.txt"])?;
    let out = repo.run_mesh(["pre-commit", "--no-exit-code"])?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        out.status.code(),
        Some(0),
        "expected exit 0 with --no-exit-code; stdout={stdout}"
    );
    assert!(
        stdout.contains("drift in staged tree"),
        "drift should still be reported; stdout={stdout}"
    );
    Ok(())
}

/// Clean staged tree: no findings, no output, exit 0.
#[test]
fn clean_staged_tree_emits_no_output_and_exits_zero() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed_line_range_mesh(&repo, "m")?;
    // Stage an unrelated path with no mesh drift.
    repo.write_file("file2.txt", "additional\n")?;
    repo.run_git(["add", "file2.txt"])?;
    let out = repo.run_mesh(["pre-commit"])?;
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        out.stdout.is_empty(),
        "expected empty stdout; got={}",
        String::from_utf8_lossy(&out.stdout)
    );
    Ok(())
}

/// Index drift on a path NOT in the in-flight commit must not fail —
/// the filter restricts findings to the staged path set.
#[test]
fn unrelated_index_drift_does_not_fail_commit() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed_line_range_mesh(&repo, "m")?;
    // Drift staged in file1.txt (the pinned path) — but we're staging
    // a *different* path for commit. Wait: if file1.txt is staged, the
    // commit DOES touch it. To make this test meaningful we must NOT
    // stage file1.txt. Instead, stage only file2.txt while file1.txt
    // sits dirty in the worktree (no add).
    repo.write_file(
        "file1.txt",
        "lineONE\nline2\nline3\nline4\nline5\nline6\nline7\nline8\nline9\nline10\n",
    )?;
    // Stage only file2.txt.
    repo.write_file("file2.txt", "additional\n")?;
    repo.run_git(["add", "file2.txt"])?;
    let out = repo.run_mesh(["pre-commit"])?;
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    Ok(())
}

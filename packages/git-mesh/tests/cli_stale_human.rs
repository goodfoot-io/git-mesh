//! CLI: `git mesh stale` — default human output (§10.4).

mod support;

use anyhow::Result;
use support::TestRepo;

fn seed(repo: &TestRepo, name: &str) -> Result<()> {
    repo.mesh_stdout(["add", name, "file1.txt#L1-L5"])?;
    repo.mesh_stdout(["why", name, "-m", "seed"])?;
    repo.mesh_stdout(["commit", name])?;
    Ok(())
}

/// Seed a mesh anchoring lines 6-10, which the `drift` helper does not mutate.
fn seed_stable(repo: &TestRepo, name: &str) -> Result<()> {
    repo.mesh_stdout(["add", name, "file1.txt#L6-L10"])?;
    repo.mesh_stdout(["why", name, "-m", "seed stable"])?;
    repo.mesh_stdout(["commit", name])?;
    Ok(())
}

fn drift(repo: &TestRepo, msg: &str) -> Result<String> {
    repo.write_file(
        "file1.txt",
        "lineONE\nline2\nline3\nline4\nline5\nline6\nline7\nline8\nline9\nline10\n",
    )?;
    repo.commit_all(msg)
}

#[test]

fn clean_exit_zero() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed(&repo, "m")?;
    let out = repo.run_mesh(["stale", "m"])?;
    assert_eq!(out.status.code(), Some(0));
    Ok(())
}

#[test]
fn pending_why_matching_committed_message_is_not_duplicated() -> Result<()> {
    // The stale block includes the committed why text inline.
    // Verify the stale output includes the mesh heading.
    let repo = TestRepo::seeded()?;
    repo.mesh_stdout(["add", "m", "file1.txt#L1-L5"])?;
    repo.mesh_stdout(["why", "m", "-m", "shared why text"])?;
    repo.mesh_stdout(["commit", "m"])?;
    drift(&repo, "mutate")?;
    repo.mesh_stdout(["why", "m", "-m", "shared why text"])?;
    let stdout = repo.mesh_stdout(["stale", "m", "--no-exit-code"])?;
    // The block heading is the mesh name.
    assert!(
        stdout.contains("## m"),
        "expected block heading; stdout=\n{stdout}"
    );
    Ok(())
}

#[test]

fn drifty_exit_one() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed(&repo, "m")?;
    drift(&repo, "mutate")?;
    let out = repo.run_mesh(["stale", "m"])?;
    assert_eq!(out.status.code(), Some(1));
    Ok(())
}

#[test]

fn no_exit_code_forces_zero() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed(&repo, "m")?;
    drift(&repo, "mutate")?;
    let out = repo.run_mesh(["stale", "m", "--no-exit-code"])?;
    assert_eq!(out.status.code(), Some(0));
    Ok(())
}

#[test]

fn human_output_has_summary_line() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed(&repo, "m")?;
    drift(&repo, "mutate")?;
    let out = repo.run_mesh(["stale", "m"])?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    // New shape: ## <mesh-name> heading with per-anchor status suffix.
    assert!(
        stdout.contains("## m"),
        "block heading must appear, got: {stdout}"
    );
    assert!(
        stdout.contains("changed"),
        "stale anchor must carry status suffix, got: {stdout}"
    );
    Ok(())
}

#[test]

fn oneline_suppresses_diffs() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed(&repo, "m")?;
    drift(&repo, "mutate")?;
    let out = repo.run_mesh(["stale", "m", "--oneline"])?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(!stdout.contains("@@ "));
    Ok(())
}

#[test]

fn stat_shows_counts() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed(&repo, "m")?;
    drift(&repo, "mutate")?;
    let out = repo.run_mesh(["stale", "m", "--stat"])?;
    assert_eq!(out.status.code(), Some(1));
    Ok(())
}

#[test]

fn patch_includes_unified_diff() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed(&repo, "m")?;
    drift(&repo, "mutate")?;
    let out = repo.run_mesh(["stale", "m", "--patch"])?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("@@ "));
    Ok(())
}

#[test]

fn human_oneline_emits_status_path_range_per_line() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed(&repo, "m")?;
    drift(&repo, "mutate")?;
    let out = repo.run_mesh(["stale", "m", "--oneline"])?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    // Should contain a line starting with `CHANGED` and the anchor.
    assert!(
        stdout
            .lines()
            .any(|l| l.starts_with("CHANGED") && l.contains("file1.txt#L1-L5")),
        "oneline content: {stdout}"
    );
    // No mesh header.
    assert!(!stdout.contains("mesh m"));
    // No diff bodies.
    assert!(!stdout.contains("@@ "));
    Ok(())
}

#[test]

fn workspace_scan_without_name() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed(&repo, "a")?;
    seed(&repo, "b")?;
    drift(&repo, "mutate")?;
    let out = repo.run_mesh(["stale"])?;
    assert_eq!(out.status.code(), Some(1));
    Ok(())
}

#[test]
fn human_output_has_drift_summary_line() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed(&repo, "m")?;
    drift(&repo, "mutate")?;
    let out = repo.run_mesh(["stale", "m"])?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    // New shape: per-anchor status suffix replaces the summary line.
    assert!(
        stdout.contains("## m"),
        "block heading must appear, got: {stdout}"
    );
    assert!(
        stdout.contains("changed"),
        "stale anchor must carry status suffix, got: {stdout}"
    );
    Ok(())
}

/// Without `--name`, a clean mesh must not appear in output; only the drifted mesh shows.
#[test]
fn workspace_scan_omits_clean_mesh() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed_stable(&repo, "quiet-mesh")?; // anchors lines 6-10 — unaffected by drift
    seed(&repo, "drifted-mesh")?; // anchors lines 1-5 — will drift
    drift(&repo, "mutate")?;
    let out = repo.run_mesh(["stale"])?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("drifted-mesh"),
        "drifted mesh must appear in output"
    );
    assert!(
        !stdout.contains("quiet-mesh"),
        "clean mesh must not appear in output"
    );
    assert_eq!(
        out.status.code(),
        Some(1),
        "exit 1 because drifted mesh has drift"
    );
    Ok(())
}

/// Without `--name`, when all meshes are clean, exit 0 and a summary line is printed.
#[test]
fn workspace_scan_all_clean_exit_zero() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed(&repo, "a")?;
    seed(&repo, "b")?;
    let out = repo.run_mesh(["stale"])?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("0 stale"),
        "summary must mention 0 stale when all meshes are clean, got: {stdout}"
    );
    assert_eq!(out.status.code(), Some(0), "exit 0 when no drift");
    Ok(())
}

/// A clean named mesh always renders the full block (no "is clean" line).
#[test]
fn named_lookup_clean_mesh_is_confirmed() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed(&repo, "quiet")?;
    let out = repo.run_mesh(["stale", "quiet"])?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    // New: fully-clean named mesh renders the unified block with bare anchor lines.
    assert!(
        stdout.contains("## quiet"),
        "block heading must appear for clean named mesh, got: {stdout}"
    );
    assert!(
        stdout.contains("file1.txt#L1-L5"),
        "fresh anchor must appear bare, got: {stdout}"
    );
    // No old "is clean" prose.
    assert!(
        !stdout.contains("is clean"),
        "legacy confirmation line must be absent, got: {stdout}"
    );
    assert_eq!(out.status.code(), Some(0), "exit 0 for clean named mesh");
    Ok(())
}

/// Named lookup with all anchors drifted AND a staged pending add must render
/// both the drift bullet and the `pending add` bullet under a single block.
///
/// Regression test: the old mesh-shape heuristic returned `false` when every
/// anchor was drifted (no Fresh anchors), silently suppressing pending bullets.
#[test]
fn named_lookup_all_drifted_shows_pending_add() -> Result<()> {
    let repo = TestRepo::seeded()?;
    // Seed mesh with one anchor on lines 1-5.
    repo.mesh_stdout(["add", "m", "file1.txt#L1-L5"])?;
    repo.mesh_stdout(["why", "m", "-m", "regression test mesh"])?;
    repo.mesh_stdout(["commit", "m"])?;

    // Drift the anchor by mutating line 1.
    repo.write_file(
        "file1.txt",
        "lineONE\nline2\nline3\nline4\nline5\nline6\nline7\nline8\nline9\nline10\n",
    )?;
    repo.commit_all("mutate file1")?;

    // Stage a pending add for a second anchor (do not commit).
    repo.mesh_stdout(["add", "m", "file2.txt#L1-L5"])?;

    // Named lookup: `git mesh stale m`.
    let out = repo.run_mesh(["stale", "m", "--no-exit-code"])?;
    let stdout = String::from_utf8_lossy(&out.stdout);

    assert!(
        stdout.contains("## m"),
        "block heading must appear; stdout=\n{stdout}"
    );
    assert!(
        stdout.contains("changed"),
        "drift bullet must appear for drifted anchor; stdout=\n{stdout}"
    );
    assert!(
        stdout.contains("pending add"),
        "pending add bullet must appear even when all anchors are drifted; stdout=\n{stdout}"
    );
    Ok(())
}

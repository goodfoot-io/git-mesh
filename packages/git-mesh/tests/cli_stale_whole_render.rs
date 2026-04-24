//! Whole-file pins render as `(whole)` across all renderers, not as
//! `#L0-L0` or raw `0   0`. Regression coverage for the rendering bug
//! called out in `docs/manual-validation-of-git-mesh-follow-up.md`.

mod support;

use anyhow::Result;
use support::TestRepo;

/// Seed a mesh containing a whole-file pin on a binary asset and drift
/// the worktree so it shows as CHANGED.
fn seed_whole_file_drift(repo: &TestRepo) -> Result<()> {
    std::fs::write(repo.path().join("hero.png"), [0u8, 1, 2, 3, 4, 5, 6, 7])?;
    repo.commit_all("seed")?;
    let _ = repo.run_mesh(["add", "m", "hero.png"])?;
    repo.mesh_stdout(["why", "m", "-m", "describes hero"])?;
    repo.mesh_stdout(["commit", "m"])?;
    // Drift the worktree.
    std::fs::write(repo.path().join("hero.png"), [9u8, 9, 9, 9])?;
    Ok(())
}

/// Seed a mesh with a line-range pin and drift it (regression guard:
/// line ranges must still render as `#L1-L5`).
fn seed_line_range_drift(repo: &TestRepo) -> Result<()> {
    repo.mesh_stdout(["add", "m", "file1.txt#L1-L5"])?;
    repo.mesh_stdout(["why", "m", "-m", "seed"])?;
    repo.mesh_stdout(["commit", "m"])?;
    repo.write_file(
        "file1.txt",
        "lineONE\nline2\nline3\nline4\nline5\nline6\nline7\nline8\nline9\nline10\n",
    )?;
    Ok(())
}

#[test]
fn whole_pin_human_renders_whole() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed_whole_file_drift(&repo)?;
    let out = repo.run_mesh(["stale", "m"])?;
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("hero.png  (whole)"),
        "expected `hero.png  (whole)`, got:\n{text}"
    );
    assert!(
        !text.contains("#L0-L0"),
        "did not expect `#L0-L0` in human output:\n{text}"
    );
    Ok(())
}

#[test]
fn whole_pin_oneline_renders_whole() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed_whole_file_drift(&repo)?;
    let out = repo.run_mesh(["stale", "m", "--oneline"])?;
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("(whole)"),
        "oneline missing `(whole)`:\n{text}"
    );
    assert!(!text.contains("#L0-L0"));
    Ok(())
}

#[test]
fn whole_pin_porcelain_uses_whole_marker() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed_whole_file_drift(&repo)?;
    let out = repo.run_mesh(["stale", "m", "--format=porcelain"])?;
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("(whole)"),
        "porcelain missing `(whole)`:\n{text}"
    );
    assert!(
        !text.contains("\t0\t0\t"),
        "porcelain should not carry literal `0\\t0\\t` columns for whole pins:\n{text}"
    );
    Ok(())
}

#[test]
fn whole_pin_junit_name_uses_whole() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed_whole_file_drift(&repo)?;
    let out = repo.run_mesh(["stale", "m", "--format=junit"])?;
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("(whole)"), "junit missing `(whole)`:\n{text}");
    assert!(!text.contains("#L0-L0"));
    Ok(())
}

#[test]
fn whole_pin_github_actions_omits_line_zero() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed_whole_file_drift(&repo)?;
    let out = repo.run_mesh(["stale", "m", "--format=github-actions"])?;
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        !text.contains("line=0"),
        "github-actions should omit `line=0` for whole pins:\n{text}"
    );
    assert!(text.contains("file=hero.png"));
    Ok(())
}

#[test]
fn whole_pin_show_renders_whole() -> Result<()> {
    let repo = TestRepo::seeded()?;
    std::fs::write(repo.path().join("hero.png"), [0u8, 1, 2, 3, 4, 5, 6, 7])?;
    repo.commit_all("seed")?;
    let _ = repo.run_mesh(["add", "m", "hero.png"])?;
    repo.mesh_stdout(["why", "m", "-m", "describes hero"])?;
    repo.mesh_stdout(["commit", "m"])?;
    let text = repo.mesh_stdout(["m"])?;
    assert!(
        text.contains("hero.png  (whole)"),
        "show output missing `hero.png  (whole)`:\n{text}"
    );
    assert!(!text.contains("#L0-L0"));
    Ok(())
}

#[test]
fn line_range_pin_still_renders_line_address() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed_line_range_drift(&repo)?;
    let out = repo.run_mesh(["stale", "m"])?;
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("file1.txt#L1-L5"),
        "line-range pin must still render `#L1-L5`:\n{text}"
    );
    assert!(!text.contains("(whole)"));
    Ok(())
}

//! Slice 8 renderer × layer-combination smoke tests.
//!
//! Per `docs/stale-layers-slices.md` slice 8: the renderers consume
//! `Finding` / `PendingFinding` end-to-end. These tests exercise each
//! `--format` against representative layer toggles to catch shape
//! regressions cheaply. `tests/cli_stale_human.rs` and
//! `tests/cli_stale_machine.rs` continue to host older / phase-pending
//! snapshot expectations.

mod support;

use anyhow::Result;
use serde_json::Value;
use support::TestRepo;

fn seed(repo: &TestRepo, name: &str) -> Result<()> {
    repo.mesh_stdout(["add", name, "file1.txt#L1-L5"])?;
    repo.mesh_stdout(["why", name, "-m", "seed"])?;
    repo.mesh_stdout(["commit", name])?;
    Ok(())
}

fn drift_in_head(repo: &TestRepo) -> Result<String> {
    repo.write_file(
        "file1.txt",
        "lineONE\nline2\nline3\nline4\nline5\nline6\nline7\nline8\nline9\nline10\n",
    )?;
    repo.commit_all("mutate")
}

#[test]
fn json_envelope_has_schema_version_and_findings() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed(&repo, "m")?;
    drift_in_head(&repo)?;
    let out = repo.run_mesh(["stale", "m", "--format=json"])?;
    let v: Value = serde_json::from_slice(&out.stdout)?;
    assert_eq!(v["schema_version"], 1);
    assert!(v["findings"].is_array(), "envelope: {v}");
    assert!(v["pending"].is_array(), "envelope: {v}");
    let first = &v["findings"][0];
    assert_eq!(first["status"]["code"], "CHANGED");
    assert_eq!(first["mesh"], "m");
    assert!(first["range_id"].is_string());
    assert!(first["anchored"]["path"].is_string());
    Ok(())
}

#[test]
fn json_head_only_findings_carry_source_head() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed(&repo, "m")?;
    drift_in_head(&repo)?;
    // HEAD-only via --no-worktree --no-index --no-staged-mesh.
    let out = repo.run_mesh([
        "stale",
        "m",
        "--no-worktree",
        "--no-index",
        "--no-staged-mesh",
        "--format=json",
    ])?;
    let v: Value = serde_json::from_slice(&out.stdout)?;
    let first = &v["findings"][0];
    assert_eq!(first["source"], "HEAD");
    Ok(())
}

#[test]
fn porcelain_head_only_omits_src_column() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed(&repo, "m")?;
    drift_in_head(&repo)?;
    let out = repo.run_mesh([
        "stale",
        "m",
        "--no-worktree",
        "--no-index",
        "--no-staged-mesh",
        "--format=porcelain",
    ])?;
    let text = String::from_utf8_lossy(&out.stdout);
    let line = text
        .lines()
        .find(|l| l.starts_with("CHANGED"))
        .unwrap_or("");
    // 6 columns: STATUS \t mesh \t path \t s \t e \t anchor
    assert_eq!(
        line.matches('\t').count(),
        5,
        "HEAD-only porcelain has no src column: {line}"
    );
    Ok(())
}

#[test]
fn porcelain_layered_includes_src_column() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed(&repo, "m")?;
    drift_in_head(&repo)?;
    let out = repo.run_mesh(["stale", "m", "--format=porcelain"])?;
    let text = String::from_utf8_lossy(&out.stdout);
    let line = text
        .lines()
        .find(|l| l.starts_with("CHANGED"))
        .unwrap_or("");
    // 7 columns when src column is on.
    assert_eq!(
        line.matches('\t').count(),
        6,
        "layered porcelain has src column: {line}"
    );
    Ok(())
}

#[test]
fn human_layered_emits_src_marker() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed(&repo, "m")?;
    drift_in_head(&repo)?;
    let out = repo.run_mesh(["stale", "m"])?;
    let text = String::from_utf8_lossy(&out.stdout);
    // Layered (default) renderer prefixes findings with the src marker.
    assert!(
        text.lines()
            .any(|l| l.trim_start().starts_with("H ") && l.contains("CHANGED")),
        "expected src-marker prefix on finding line: {text}"
    );
    Ok(())
}

#[test]
fn junit_has_testsuite_and_testcase_tags() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed(&repo, "m")?;
    drift_in_head(&repo)?;
    let out = repo.run_mesh(["stale", "m", "--format=junit"])?;
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(s.contains("<testsuite"));
    assert!(s.contains("<testcase"));
    assert!(s.contains("CHANGED"));
    Ok(())
}

#[test]
fn github_actions_emits_annotation_with_path() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed(&repo, "m")?;
    drift_in_head(&repo)?;
    let out = repo.run_mesh(["stale", "m", "--format=github-actions"])?;
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(s.contains("file=file1.txt"), "annotation: {s}");
    assert!(s.contains("CHANGED"));
    Ok(())
}

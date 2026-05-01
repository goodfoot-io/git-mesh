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
use std::process::Command;
use support::TestRepo;

fn seed(repo: &TestRepo, name: &str) -> Result<()> {
    repo.mesh_stdout(["add", name, "file1.txt#L1-L5"])?;
    repo.mesh_stdout(["why", name, "-m", "seed"])?;
    repo.mesh_stdout(["commit", name])?;
    Ok(())
}

fn seed_stable(repo: &TestRepo, name: &str) -> Result<()> {
    repo.mesh_stdout(["add", name, "file1.txt#L6-L10"])?;
    repo.mesh_stdout(["why", name, "-m", "stable seed"])?;
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
    assert_eq!(v["schema_version"], 2);
    assert!(v["findings"].is_array(), "envelope: {v}");
    assert!(v["pending"].is_array(), "envelope: {v}");
    let first = &v["findings"][0];
    assert_eq!(first["status"]["code"], "CHANGED");
    assert_eq!(first["mesh"], "m");
    assert!(first["anchor_id"].is_null());
    assert!(first["anchored"]["path"].is_string());
    Ok(())
}

#[test]
fn discovery_json_filters_clean_meshes_before_rendering() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed(&repo, "drifty")?;
    seed_stable(&repo, "quiet")?;
    drift_in_head(&repo)?;

    let out = repo.run_mesh(["stale", "--format=json"])?;
    assert_eq!(out.status.code(), Some(1));
    let v: Value = serde_json::from_slice(&out.stdout)?;
    let findings = v["findings"].as_array().expect("findings array");
    assert!(
        findings.iter().any(|f| f["mesh"] == "drifty"),
        "drifty finding missing: {v}"
    );
    assert!(
        findings.iter().all(|f| f["mesh"] != "quiet"),
        "clean mesh leaked into JSON discovery output: {v}"
    );
    Ok(())
}

#[test]
fn discovery_clean_head_pinned_mesh_uses_fast_path() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed(&repo, "fresh")?;

    let out = Command::new(env!("CARGO_BIN_EXE_git-mesh"))
        .current_dir(repo.path())
        .env("GIT_MESH_PERF", "1")
        .arg("stale")
        .output()?;

    assert_eq!(out.status.code(), Some(0));
    let stdout = String::from_utf8(out.stdout)?;
    let stderr = String::from_utf8(out.stderr)?;
    assert!(stdout.trim().is_empty(), "stdout={stdout}");
    assert!(
        stderr.contains("git-mesh perf: resolver.resolve-stale-meshes"),
        "expected discovery resolver span: {stderr}"
    );
    assert!(
        !stderr.contains("git-mesh perf: resolver.resolve-anchors"),
        "clean HEAD-pinned discovery should skip per-anchor resolution: {stderr}"
    );
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
    // 5 columns: STATUS \t mesh \t path \t s \t e
    assert_eq!(
        line.matches('\t').count(),
        4,
        "HEAD-only porcelain has no src column: {line}"
    );
    Ok(())
}

#[test]
fn discovery_porcelain_filters_clean_meshes_before_rendering() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed(&repo, "drifty")?;
    seed_stable(&repo, "quiet")?;
    drift_in_head(&repo)?;

    let out = repo.run_mesh(["stale", "--format=porcelain"])?;
    assert_eq!(out.status.code(), Some(1));
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("drifty"), "stdout={text}");
    assert!(!text.contains("quiet"), "stdout={text}");
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
    // 6 columns when src column is on.
    assert_eq!(
        line.matches('\t').count(),
        5,
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
        text.contains("(Changed in worktree)"),
        "expected src-marker on finding line: {text}"
    );
    Ok(())
}

#[test]
fn discovery_human_includes_staging_only_mesh() -> Result<()> {
    let repo = TestRepo::seeded()?;
    repo.mesh_stdout(["add", "new-mesh", "file1.txt#L1-L5"])?;

    let out = repo.run_mesh(["stale"])?;
    assert_eq!(out.status.code(), Some(0));
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("Mesh new-mesh"), "stdout={text}");
    assert!(
        text.contains("file1.txt#L1-L5 (Pending add)"),
        "stdout={text}"
    );
    Ok(())
}

#[test]
fn discovery_json_includes_clean_mesh_with_pending_metadata() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed(&repo, "clean-with-pending")?;
    repo.mesh_stdout(["why", "clean-with-pending", "-m", "updated reason"])?;

    let out = repo.run_mesh(["stale", "--format=json"])?;
    assert_eq!(out.status.code(), Some(0));
    let v: Value = serde_json::from_slice(&out.stdout)?;
    assert!(
        v["findings"].as_array().is_some_and(Vec::is_empty),
        "metadata-only pending must not create findings: {v}"
    );
    let pending = v["pending"].as_array().expect("pending array");
    assert_eq!(pending.len(), 1, "pending metadata entry: {v}");
    assert_eq!(pending[0]["mesh"], "clean-with-pending");
    assert_eq!(pending[0]["kind"], "why");
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

#[test]
fn human_pending_ops_render_range_addresses() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed(&repo, "m")?;
    repo.mesh_stdout(["add", "m", "file2.txt#L1-L5"])?;
    repo.mesh_stdout(["rm", "m", "file1.txt#L1-L5"])?;

    let out = repo.mesh_stdout(["stale", "m", "--no-exit-code"])?;
    assert!(
        out.contains("file2.txt#L1-L5 (Pending add)"),
        "stdout={out}"
    );
    assert!(
        out.contains("file1.txt#L1-L5 (Pending remove)"),
        "stdout={out}"
    );
    assert!(
        !out.contains("file2.txt L1-L5") && !out.contains("file1.txt L1-L5"),
        "pending ops should use anchor-address syntax: {out}"
    );
    assert!(
        !out.contains("()"),
        "empty anchor-id parentheses should not be rendered: {out}"
    );
    Ok(())
}

#[test]
fn human_stat_mode_prints_change_counts() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed(&repo, "m")?;
    drift_in_head(&repo)?;
    let out = repo.run_mesh(["stale", "m", "--stat"])?;
    assert_eq!(out.status.code(), Some(1));
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("file1.txt#L1-L5 | +1 -1"), "stdout={text}");
    Ok(())
}

#[test]
fn human_patch_mode_prints_unified_diff() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed(&repo, "m")?;
    drift_in_head(&repo)?;
    let out = repo.run_mesh(["stale", "m", "--patch"])?;
    assert_eq!(out.status.code(), Some(1));
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("--- file1.txt#L1-L5 (anchored)"),
        "stdout={text}"
    );
    assert!(text.contains("+++ file1.txt#L1-L5"), "stdout={text}");
    assert!(text.contains("@@"), "stdout={text}");
    Ok(())
}

#[test]
fn named_stale_shows_pending_ops_for_new_mesh() -> Result<()> {
    let repo = TestRepo::seeded()?;
    repo.mesh_stdout(["add", "new-mesh", "file1.txt#L1-L5"])?;
    let out = repo.mesh_stdout(["stale", "new-mesh", "--no-exit-code"])?;
    assert!(out.contains("Mesh new-mesh"), "stdout={out}");
    assert!(
        out.contains("file1.txt#L1-L5 (Pending add)"),
        "stdout={out}"
    );
    Ok(())
}

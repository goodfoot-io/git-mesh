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
    repo.mesh_stdout(["remove", "m", "file1.txt#L1-L5"])?;

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

// ---------------------------------------------------------------------------
// Phase A: always-on Moved arrow + moved_to JSON field.
// ---------------------------------------------------------------------------

/// Helper: seed a line-range anchor on file1.txt#L1-L5 and commit.
fn seed_line_range(repo: &TestRepo, name: &str) -> Result<()> {
    repo.mesh_stdout(["add", name, "file1.txt#L1-L5"])?;
    repo.mesh_stdout(["why", name, "-m", "seed"])?;
    repo.mesh_stdout(["commit", name])?;
    Ok(())
}

/// Helper: shift lines down (prepend 2 lines) so the range moves.
fn shift_lines(repo: &TestRepo) -> Result<()> {
    repo.write_file(
        "file1.txt",
        "extra1\nextra2\nline1\nline2\nline3\nline4\nline5\nline6\nline7\nline8\nline9\nline10\n",
    )?;
    repo.commit_all("shift")?;
    Ok(())
}

#[test]
fn human_moved_row_shows_arrow_with_destination() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed_line_range(&repo, "m")?;
    shift_lines(&repo)?;
    let out = repo.run_mesh(["stale", "m"])?;
    assert_eq!(out.status.code(), Some(1));
    let text = String::from_utf8_lossy(&out.stdout);
    // The bullet should include "→" followed by the new location.
    assert!(
        text.contains("file1.txt#L1-L5") && text.contains('→'),
        "expected arrow in Moved bullet; stdout={text}"
    );
    Ok(())
}

#[test]
fn human_fresh_sibling_row_has_no_trailing_parenthesis() -> Result<()> {
    // In a partially-stale mesh, Fresh siblings must render as bare
    // `- <path>` with no status word. (Fully-fresh meshes produce no
    // output at all — see workspace_scan_all_clean_exit_zero.)
    let repo = TestRepo::seeded()?;
    repo.mesh_stdout(["add", "m", "file1.txt#L1-L5", "file2.txt#L1-L5"])?;
    repo.mesh_stdout(["why", "m", "-m", "seed"])?;
    repo.mesh_stdout(["commit", "m"])?;
    // Drift only file1.txt so file2.txt#L1-L5 stays Fresh.
    repo.write_file(
        "file1.txt",
        "edit1\nedit2\nedit3\nedit4\nedit5\nline6\nline7\nline8\nline9\nline10\n",
    )?;
    let out = repo.run_mesh(["stale", "m"])?;
    let text = String::from_utf8_lossy(&out.stdout);
    let line = text
        .lines()
        .find(|l| l.starts_with("- file2.txt"))
        .unwrap_or_else(|| panic!("no fresh-sibling bullet in stdout={text}"));
    assert!(
        !line.contains('('),
        "Fresh bullet must not have trailing parenthesis; line={line}"
    );
    Ok(())
}

#[test]
fn json_moved_finding_has_moved_to_field() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed_line_range(&repo, "m")?;
    shift_lines(&repo)?;
    let out = repo.run_mesh(["stale", "m", "--format=json"])?;
    assert_eq!(out.status.code(), Some(1));
    let v: Value = serde_json::from_slice(&out.stdout).expect("valid json");
    let findings = v["findings"].as_array().expect("findings array");
    let moved = findings
        .iter()
        .find(|f| f["status"]["code"] == "MOVED")
        .expect("at least one MOVED finding");
    assert!(
        moved["moved_to"].is_object(),
        "MOVED finding must have moved_to object; finding={moved}"
    );
    assert!(
        moved["moved_to"]["path"].is_string(),
        "moved_to must have path; finding={moved}"
    );
    assert!(
        moved["moved_to"]["extent"].is_object(),
        "moved_to must have extent; finding={moved}"
    );
    Ok(())
}

#[test]
fn json_changed_finding_has_no_moved_to_field() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed(&repo, "m")?;
    drift_in_head(&repo)?;
    let out = repo.run_mesh(["stale", "m", "--format=json"])?;
    assert_eq!(out.status.code(), Some(1));
    let v: Value = serde_json::from_slice(&out.stdout).expect("valid json");
    let findings = v["findings"].as_array().expect("findings array");
    let changed = findings
        .iter()
        .find(|f| f["status"]["code"] == "CHANGED")
        .expect("at least one CHANGED finding");
    assert!(
        changed["moved_to"].is_null(),
        "non-MOVED finding must not have moved_to; finding={changed}"
    );
    Ok(())
}

// ── multi-arg positional resolution ──────────────────────────────────────

#[test]
fn missing_file_arg_exits_one_with_diagnostic() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let out = repo.run_mesh(["stale", "nonexistent-file"])?;
    assert_eq!(out.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("file not found: 'nonexistent-file'"),
        "expected file-not-found diagnostic, got: {stderr}"
    );
    Ok(())
}

#[test]
fn multiple_missing_file_args_reports_each() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let out = repo.run_mesh(["stale", "bad1", "bad2"])?;
    assert_eq!(out.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("file not found: 'bad1'"),
        "expected diagnostic for bad1, got: {stderr}"
    );
    assert!(
        stderr.contains("file not found: 'bad2'"),
        "expected diagnostic for bad2, got: {stderr}"
    );
    Ok(())
}

#[test]
fn existing_file_with_no_mesh_does_not_error() -> Result<()> {
    let repo = TestRepo::seeded()?;
    // file2.txt exists in the seeded worktree but no mesh tracks it. Stale
    // should exit 0 silently — "no mesh involves this file" is not an error.
    let out = repo.run_mesh(["stale", "file2.txt"])?;
    assert_eq!(
        out.status.code(),
        Some(0),
        "expected exit 0 for existing-but-unanchored file, stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("file not found"),
        "must not surface file-not-found for an existing file, got: {stderr}"
    );
    Ok(())
}

#[test]
fn file_path_arg_resolves_mesh_via_path_index() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed(&repo, "m")?;
    drift_in_head(&repo)?;
    // Resolve by file path (not mesh name) through the path index.
    let out = repo.run_mesh(["stale", "file1.txt", "--format=json"])?;
    assert_eq!(
        out.status.code(),
        Some(1),
        "stale mesh should exit 1, stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: Value = serde_json::from_slice(&out.stdout).expect("valid json");
    assert_eq!(
        v["mesh"], "m",
        "file path arg should resolve to mesh 'm' via path index, got: {v}"
    );
    Ok(())
}

#[test]
fn mixed_mesh_name_and_path_args_are_deduplicated() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed(&repo, "m")?;
    drift_in_head(&repo)?;
    // Pass both the mesh name and the file path — both resolve to mesh "m".
    let out = repo.run_mesh(["stale", "m", "file1.txt", "--format=json"])?;
    assert_eq!(
        out.status.code(),
        Some(1),
        "stale mesh should exit 1, stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: Value = serde_json::from_slice(&out.stdout).expect("valid json");
    assert_eq!(
        v["mesh"], "m",
        "deduplicated args should produce one mesh output, got: {v}"
    );
    let findings = v["findings"].as_array().expect("findings array");
    let changed = findings
        .iter()
        .filter(|f| f["status"]["code"] == "CHANGED")
        .count();
    assert!(
        changed > 0,
        "should have at least one CHANGED finding from mesh 'm'"
    );
    Ok(())
}

#[test]
fn all_args_resolve_to_clean_mesh_exits_zero() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed(&repo, "clean-mesh")?;
    // Mesh is clean (no drift). Stale by mesh name should exit 0.
    let out = repo.run_mesh(["stale", "clean-mesh"])?;
    assert_eq!(out.status.code(), Some(0));
    Ok(())
}

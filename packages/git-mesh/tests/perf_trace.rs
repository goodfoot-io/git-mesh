//! Integration tests for `--perf-trace <path>` CSV emitter.

mod support;

use anyhow::Result;
use support::TestRepo;

fn seed_and_drift(repo: &TestRepo, mesh: &str) -> Result<()> {
    repo.mesh_stdout(["add", mesh, "file1.txt#L1-L5"])?;
    repo.mesh_stdout(["why", mesh, "-m", "seed"])?;
    repo.mesh_stdout(["commit", mesh])?;
    repo.write_file(
        "file1.txt",
        "lineONE\nline2\nline3\nline4\nline5\nline6\nline7\nline8\nline9\nline10\n",
    )?;
    repo.commit_all("drift")?;
    Ok(())
}

/// `--perf-trace` emits one CSV data row per anchor (plus header).
#[test]
fn perf_trace_emits_row_per_anchor() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed_and_drift(&repo, "m")?;

    let trace_path = repo.path().join("trace.csv");
    let out = repo.run_mesh(["stale", "--perf-trace", trace_path.to_str().unwrap()])?;
    assert_eq!(out.status.code(), Some(1), "drift should exit 1");

    let csv = std::fs::read_to_string(&trace_path)?;
    let lines: Vec<&str> = csv.lines().collect();
    assert!(lines.len() >= 2, "expected header + at least one data row; got: {csv}");
    assert_eq!(lines[0], "mesh,anchor_id,anchor_sha,path,wall_us,fast_path,status");
    Ok(())
}

/// All seven CSV columns are present and non-empty.
#[test]
fn perf_trace_columns_match_schema() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed_and_drift(&repo, "m")?;

    let trace_path = repo.path().join("trace.csv");
    repo.run_mesh(["stale", "--perf-trace", trace_path.to_str().unwrap()])?;

    let csv = std::fs::read_to_string(&trace_path)?;
    let mut lines = csv.lines();
    let header = lines.next().expect("header row");
    assert_eq!(header, "mesh,anchor_id,anchor_sha,path,wall_us,fast_path,status");

    let row = lines.next().expect("at least one data row");
    let cols: Vec<&str> = row.splitn(7, ',').collect();
    assert_eq!(cols.len(), 7, "expected 7 columns in row: {row}");
    assert!(!cols[0].is_empty(), "mesh must be non-empty");
    assert!(!cols[1].is_empty(), "anchor_id must be non-empty");
    assert!(!cols[2].is_empty(), "anchor_sha must be non-empty");
    assert!(!cols[3].is_empty(), "path must be non-empty");
    // wall_us is a u128, so it can be 0 on fast hardware; just parse it.
    cols[4].parse::<u128>().expect("wall_us must be a number");
    assert!(cols[5] == "true" || cols[5] == "false", "fast_path must be bool: {}", cols[5]);
    assert!(!cols[6].is_empty(), "status must be non-empty");
    Ok(())
}

/// The `fast_path` column is `true` when the anchor was served by the
/// per-anchor `clean_head_fast_path`. The fast-path fires when the anchor
/// blob matches HEAD's current blob but anchor_sha != HEAD (i.e. HEAD moved
/// forward with an unrelated commit so the mesh-level skip does not apply).
#[test]
fn perf_trace_fast_path_flag_reflects_counter() -> Result<()> {
    let repo = TestRepo::seeded()?;
    // Stage and commit the mesh; anchor_sha == HEAD at this point.
    repo.mesh_stdout(["add", "m", "file1.txt#L1-L5"])?;
    repo.mesh_stdout(["why", "m", "-m", "seed"])?;
    repo.mesh_stdout(["commit", "m"])?;
    // Advance HEAD with an unrelated commit so anchor_sha != HEAD but
    // file1.txt is unchanged. Now the mesh-level skip does NOT fire
    // (anchor_sha != head_sha) and the per-anchor fast-path does.
    repo.commit_file("unrelated.txt", "x\n", "unrelated")?;

    let trace_path = repo.path().join("trace.csv");
    repo.run_mesh(["stale", "--perf-trace", trace_path.to_str().unwrap()])?;

    let csv = std::fs::read_to_string(&trace_path)?;
    let rows: Vec<&str> = csv.lines().skip(1).collect();
    assert!(!rows.is_empty(), "expected at least one data row; csv:\n{csv}");
    let any_fast = rows.iter().any(|r| {
        let cols: Vec<&str> = r.splitn(7, ',').collect();
        cols.get(5).copied() == Some("true")
    });
    assert!(any_fast, "expected at least one fast_path=true row; csv:\n{csv}");
    Ok(())
}

/// A clean pin-tracked mesh (anchor_sha == HEAD) that is skipped by the
/// mesh-level fast path still appears in the CSV when `--perf-trace` is set.
#[test]
fn perf_trace_includes_clean_pinned_mesh() -> Result<()> {
    let repo = TestRepo::seeded()?;
    // Create a mesh and commit it; anchor_sha == HEAD so the mesh-level skip
    // would normally elide it from `stale_meshes_inner`.
    repo.mesh_stdout(["add", "clean-mesh", "file1.txt#L1-L5"])?;
    repo.mesh_stdout(["why", "clean-mesh", "-m", "clean mesh"])?;
    repo.mesh_stdout(["commit", "clean-mesh"])?;

    let trace_path = repo.path().join("trace.csv");
    let out = repo.run_mesh(["stale", "--perf-trace", trace_path.to_str().unwrap()])?;
    assert_eq!(out.status.code(), Some(0), "clean mesh should exit 0");

    let csv = std::fs::read_to_string(&trace_path)?;
    let data_rows: Vec<&str> = csv.lines().skip(1).collect();
    assert!(
        !data_rows.is_empty(),
        "expected at least one CSV row for the clean mesh anchor; csv:\n{csv}"
    );
    let any_clean_mesh = data_rows.iter().any(|r| r.starts_with("clean-mesh,"));
    assert!(
        any_clean_mesh,
        "expected a row with mesh=clean-mesh in the CSV; csv:\n{csv}"
    );
    Ok(())
}

/// `--perf-trace` conflicts with positional path args: must exit non-zero
/// with a clear error message.
#[test]
fn perf_trace_rejects_positional_paths() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed_and_drift(&repo, "m")?;

    let trace_path = repo.path().join("trace.csv");
    let out = repo.run_mesh(["stale", "m", "--perf-trace", trace_path.to_str().unwrap()])?;
    assert_ne!(out.status.code(), Some(0), "should fail with positional paths + --perf-trace");
    let stderr = String::from_utf8_lossy(&out.stderr);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let combined = format!("{stderr}{stdout}");
    assert!(
        combined.contains("--perf-trace") || combined.contains("full scan"),
        "expected error mentioning --perf-trace or full scan; got: {combined}"
    );
    Ok(())
}

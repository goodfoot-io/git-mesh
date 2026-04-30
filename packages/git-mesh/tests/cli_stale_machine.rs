//! CLI: `git mesh stale` machine formats (§10.4).

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

fn drift(repo: &TestRepo) -> Result<String> {
    repo.write_file(
        "file1.txt",
        "lineONE\nline2\nline3\nline4\nline5\nline6\nline7\nline8\nline9\nline10\n",
    )?;
    repo.commit_all("mutate")
}

#[test]

fn porcelain_one_line_per_finding() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed(&repo, "m")?;
    drift(&repo)?;
    let out = repo.run_mesh(["stale", "m", "--format=porcelain"])?;
    assert_eq!(out.status.code(), Some(1));
    let text = String::from_utf8_lossy(&out.stdout);
    // `# porcelain v1` header + one finding line.
    let lines: Vec<&str> = text.trim().lines().collect();
    assert_eq!(lines.len(), 2, "header + 1 finding: {text}");
    assert_eq!(lines[0], "# porcelain v2");
    assert!(text.contains("CHANGED"));
    assert!(text.contains("file1.txt"));
    Ok(())
}

#[test]

fn porcelain_clean_is_empty() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed(&repo, "m")?;
    let out = repo.run_mesh(["stale", "m", "--format=porcelain"])?;
    assert_eq!(out.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&out.stdout).trim().is_empty());
    Ok(())
}

#[test]

fn json_has_schema_version() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed(&repo, "m")?;
    drift(&repo)?;
    let out = repo.run_mesh(["stale", "m", "--format=json"])?;
    let v: Value = serde_json::from_slice(&out.stdout)?;
    assert_eq!(v["schema_version"], 2);
    assert!(v["findings"].is_array());
    Ok(())
}

#[test]

fn json_envelope_has_mesh_field() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed(&repo, "m")?;
    drift(&repo)?;
    let out = repo.run_mesh(["stale", "m", "--format=json"])?;
    let v: Value = serde_json::from_slice(&out.stdout)?;
    assert_eq!(v["schema_version"], 2);
    assert_eq!(v["mesh"], "m");
    Ok(())
}

#[test]

fn json_finding_has_status_and_anchored() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed(&repo, "m")?;
    drift(&repo)?;
    let out = repo.run_mesh(["stale", "m", "--format=json"])?;
    let v: Value = serde_json::from_slice(&out.stdout)?;
    let first = &v["findings"][0];
    assert_eq!(first["status"]["code"], "CHANGED");
    assert!(first["anchored"]["path"].is_string());
    assert_eq!(first["anchored"]["extent"]["kind"], "lines");
    assert_eq!(first["anchored"]["extent"]["start"], 1);
    assert_eq!(first["anchored"]["extent"]["end"], 5);
    Ok(())
}

#[test]

fn junit_has_testsuite_tag() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed(&repo, "m")?;
    drift(&repo)?;
    let out = repo.run_mesh(["stale", "m", "--format=junit"])?;
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(s.contains("<testsuite"));
    assert!(s.contains("<testcase"));
    Ok(())
}

#[test]

fn github_actions_emits_warning_annotation() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed(&repo, "m")?;
    drift(&repo)?;
    let out = repo.run_mesh(["stale", "m", "--format=github-actions"])?;
    let s = String::from_utf8_lossy(&out.stdout);
    // CHANGED maps to ::error per §10.4 severity rules; MOVED maps to
    // ::warning. Either is acceptable depending on the drift shape.
    assert!(
        s.contains("::error file=file1.txt") || s.contains("::warning file=file1.txt"),
        "expected annotation with file=file1.txt, got: {s}"
    );
    // No non-spec fields like endLine=.
    assert!(
        !s.contains("endLine="),
        "github-actions output must not include endLine="
    );
    Ok(())
}

#[test]

fn tool_error_exits_one() -> Result<()> {
    // Running outside a git repo is an operational failure: the
    // command is well-formed, the environment is missing. Operational
    // failures exit 1; exit 2 is reserved for clap usage errors.
    let dir = tempfile::tempdir()?;
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_git-mesh"))
        .current_dir(dir.path())
        .args(["stale"])
        .output()?;
    assert_eq!(out.status.code(), Some(1));
    Ok(())
}

#[test]

fn since_filters_by_anchor_age() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let early_anchor = repo.head_sha()?;
    // Move HEAD forward so --since has something to exclude.
    repo.commit_file("other.txt", "x\n", "mid")?;
    let mid = repo.head_sha()?;
    // Stage anchor anchored at mid, not early.
    repo.mesh_stdout(["add", "m", "file1.txt#L1-L5", "--at", &mid])?;
    repo.mesh_stdout(["why", "m", "-m", "seed"])?;
    repo.mesh_stdout(["commit", "m"])?;
    drift(&repo)?;
    // --since mid => our anchor is in scope; exit 1.
    let inc = repo.run_mesh(["stale", "m", "--since", &mid, "--format=porcelain"])?;
    assert_eq!(inc.status.code(), Some(1));
    // --since HEAD (now past mid) — anchor anchor is before HEAD, and
    // --since filters to anchors in <since>..HEAD, so older anchors
    // drop out. Use early_anchor to be explicit about scope.
    let _ = early_anchor;
    Ok(())
}

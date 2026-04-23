//! CLI: `git mesh`, `git mesh <name>`, `git mesh ls`.

mod support;

use anyhow::Result;
use support::TestRepo;

fn seed(repo: &TestRepo, name: &str) -> Result<()> {
    repo.mesh_stdout(["add", name, "file1.txt#L1-L5"])?;
    repo.mesh_stdout(["message", name, "-m", "seed"])?;
    repo.mesh_stdout(["commit", name])?;
    Ok(())
}

#[test]

fn bare_mesh_lists_all_meshes() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed(&repo, "alpha")?;
    seed(&repo, "beta")?;
    let out = repo.mesh_stdout::<[&str; 0], &str>([])?;
    assert!(out.contains("alpha"));
    assert!(out.contains("beta"));
    Ok(())
}

#[test]

fn show_by_name_has_required_lines() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed(&repo, "alpha")?;
    let out = repo.mesh_stdout(["alpha"])?;
    // §10.4 required header lines.
    assert!(out.starts_with("mesh alpha"));
    assert!(out.contains("commit "));
    assert!(out.contains("Author:"));
    assert!(out.contains("Date:"));
    assert!(out.contains("Ranges ("));
    assert!(out.contains("file1.txt#L1-L5"));
    Ok(())
}

#[test]

fn show_oneline_drops_header() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed(&repo, "alpha")?;
    let out = repo.mesh_stdout(["alpha", "--oneline"])?;
    assert!(!out.contains("Author:"));
    assert!(out.contains("file1.txt#L1-L5"));
    Ok(())
}

#[test]

fn show_no_abbrev_shows_full_sha() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed(&repo, "alpha")?;
    let out = repo.mesh_stdout(["alpha", "--no-abbrev"])?;
    // Look for a 40-char hex token on a Ranges line.
    let has_40 = out
        .lines()
        .any(|l| l.split_whitespace().any(|w| w.len() == 40 && w.chars().all(|c| c.is_ascii_hexdigit())));
    assert!(has_40, "--no-abbrev should emit a 40-char sha");
    Ok(())
}

#[test]

fn show_at_walks_history() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed(&repo, "h")?;
    // Second commit adds a second range.
    repo.mesh_stdout(["add", "h", "file2.txt#L1-L3"])?;
    repo.mesh_stdout(["message", "h", "-m", "v2"])?;
    repo.mesh_stdout(["commit", "h"])?;
    let tip_oid = repo.git_stdout(["rev-parse", "refs/meshes/v1/h~1"])?;
    let out = repo.mesh_stdout(["h", "--at", &tip_oid])?;
    assert!(out.contains("file1.txt"));
    assert!(!out.contains("file2.txt"));
    Ok(())
}

#[test]

fn show_log_walks_newest_first() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed(&repo, "h")?;
    repo.mesh_stdout(["add", "h", "file2.txt#L1-L3"])?;
    repo.mesh_stdout(["message", "h", "-m", "v2"])?;
    repo.mesh_stdout(["commit", "h"])?;
    let out = repo.mesh_stdout(["h", "--log"])?;
    let v2_pos = out.find("v2").expect("v2 in log");
    let seed_pos = out.find("seed").expect("seed in log");
    assert!(v2_pos < seed_pos);
    Ok(())
}

#[test]

fn show_log_limit_caps_output() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed(&repo, "h")?;
    repo.mesh_stdout(["add", "h", "file2.txt#L1-L3"])?;
    repo.mesh_stdout(["message", "h", "-m", "v2"])?;
    repo.mesh_stdout(["commit", "h"])?;
    let out = repo.mesh_stdout(["h", "--log", "--limit", "1"])?;
    assert!(out.contains("v2"));
    assert!(!out.contains("seed"));
    Ok(())
}

#[test]

fn show_missing_mesh_errors() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let out = repo.run_mesh(["ghost"])?;
    assert!(!out.status.success());
    Ok(())
}

#[test]

fn ls_all_lists_every_file_with_ranges() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed(&repo, "m")?;
    let out = repo.mesh_stdout(["ls"])?;
    assert!(out.contains("file1.txt"));
    Ok(())
}

#[test]

fn ls_by_path_filters() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed(&repo, "m")?;
    let out = repo.mesh_stdout(["ls", "file1.txt"])?;
    assert!(out.contains("file1.txt"));
    assert!(!out.contains("file2.txt"));
    Ok(())
}

#[test]
fn show_format_commit_placeholders() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed(&repo, "m")?;
    let out = repo.mesh_stdout(["m", "--format=%s|%an"])?;
    assert!(out.starts_with("seed|Test User"), "out={out}");
    Ok(())
}

#[test]
fn show_format_ranges_placeholder() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed(&repo, "m")?;
    let out = repo.mesh_stdout(["m", "--format=%(ranges)"])?;
    assert!(out.contains("file1.txt#L1-L5"), "out={out}");
    Ok(())
}

#[test]
fn show_format_ranges_count() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed(&repo, "m")?;
    let out = repo.mesh_stdout(["m", "--format=count=%(ranges:count)"])?;
    assert!(out.trim() == "count=1", "out={out}");
    Ok(())
}

#[test]
fn show_format_config_placeholder() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed(&repo, "m")?;
    let out = repo.mesh_stdout(["m", "--format=cd=%(config:copy-detection)"])?;
    assert!(out.trim() == "cd=same-commit", "out={out}");
    // Unknown config key → empty.
    let out = repo.mesh_stdout(["m", "--format=x=%(config:nope)y"])?;
    assert!(out.trim() == "x=y", "out={out}");
    Ok(())
}

#[test]
fn show_format_combined_placeholders() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed(&repo, "m")?;
    let out = repo.mesh_stdout(["m", "--format=%s has %(ranges:count) range(s)"])?;
    assert!(out.starts_with("seed has 1 range(s)"), "out={out}");
    Ok(())
}

#[test]

fn ls_by_path_range_filters() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed(&repo, "m")?;
    let out = repo.mesh_stdout(["ls", "file1.txt#L1-L3"])?;
    assert!(out.contains("file1.txt"));
    Ok(())
}

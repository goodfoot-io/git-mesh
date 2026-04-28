//! Integration tests for `git mesh show --format`.
//!
//! Exercises per-anchor expansion, commit-level placeholders, and
//! unknown-placeholder rejection (exit 2).

mod support;

use anyhow::Result;
use support::TestRepo;

/// Seed a repo with mesh "m" that has two ranges: a line range and a whole file.
fn seed_multi_range(repo: &TestRepo) -> Result<()> {
    repo.mesh_stdout(["add", "m", "file1.txt#L1-L5"])?;
    repo.mesh_stdout(["add", "m", "file2.txt"])?;
    repo.mesh_stdout(["why", "m", "-m", "the why sentence"])?;
    repo.mesh_stdout(["commit", "m"])?;
    Ok(())
}

#[test]
fn format_big_p_produces_one_line_per_range() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed_multi_range(&repo)?;
    let out = repo.mesh_stdout(["show", "m", "--format", "%P"])?;
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), 2, "expected one line per anchor, got: {out:?}");
    // One line for the line-anchor path, one for the whole-file path.
    assert!(
        lines.iter().any(|l| l.contains("file1.txt#L")),
        "expected file1.txt line range in output: {out:?}"
    );
    assert!(
        lines.contains(&"file2.txt"),
        "expected bare file2.txt in output: {out:?}"
    );
    Ok(())
}

#[test]
fn format_anchor_and_path_matches_oneline_shape() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed_multi_range(&repo)?;
    // `--format "%a %P"` should produce lines like `<full-sha> <path>[#Ls-Le]`
    let out = repo.mesh_stdout(["show", "m", "--format", "%a %P"])?;
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), 2, "expected two lines: {out:?}");
    for line in &lines {
        // Each line: <40-char sha> <space> <path...>
        let mut parts = line.splitn(2, ' ');
        let sha = parts.next().unwrap_or("");
        let path = parts.next().unwrap_or("");
        assert_eq!(sha.len(), 40, "anchor sha should be 40 chars: {line:?}");
        assert!(!path.is_empty(), "path should be non-empty: {line:?}");
    }
    Ok(())
}

#[test]
fn format_subject_is_per_commit_not_per_range() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed_multi_range(&repo)?;
    // %s has no anchor token → one line total
    let out = repo.mesh_stdout(["show", "m", "--format", "%s"])?;
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(
        lines.len(),
        1,
        "commit-only format should give one line: {out:?}"
    );
    assert_eq!(lines[0], "the why sentence");
    Ok(())
}

#[test]
fn format_unknown_placeholder_exits_2_with_message() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed_multi_range(&repo)?;
    let out = repo.run_mesh(["show", "m", "--format", "%xx %s"])?;
    assert_eq!(
        out.status.code(),
        Some(2),
        "expected exit code 2 for unknown placeholder"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("supported:"),
        "stderr should mention supported placeholders: {stderr:?}"
    );
    Ok(())
}

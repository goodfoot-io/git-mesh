//! Integration tests for `git mesh ls` — block format, porcelain, search,
//! pagination, path filter, staged/pending markers.

mod support;

use anyhow::Result;
use std::io::Write;
use std::process::{Command, Stdio};
use support::TestRepo;

/// Commit a mesh with a single anchor and why text.
fn commit_mesh(repo: &TestRepo, name: &str, anchor: &str, why: &str) -> Result<()> {
    repo.mesh_stdout(["add", name, anchor])?;
    repo.mesh_stdout(["why", name, "-m", why])?;
    repo.mesh_stdout(["commit", name])?;
    Ok(())
}

fn mesh_stdout_with_stdin(repo: &TestRepo, args: &[&str], stdin: &str) -> Result<String> {
    let mut child = Command::new(env!("CARGO_BIN_EXE_git-mesh"))
        .current_dir(repo.path())
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    child
        .stdin
        .as_mut()
        .expect("child stdin should be piped")
        .write_all(stdin.as_bytes())?;
    let out = child.wait_with_output()?;
    anyhow::ensure!(
        out.status.success(),
        "git-mesh failed (code {:?}): {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    Ok(String::from_utf8(out.stdout)?)
}

// ---------------------------------------------------------------------------
// Bare git mesh → short help
// ---------------------------------------------------------------------------

#[test]
fn bare_git_mesh_prints_help_exit_zero() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let out = repo.run_mesh::<[&str; 0], &str>([])?;
    assert_eq!(out.status.code(), Some(0));
    let stdout = String::from_utf8(out.stdout)?;
    assert!(
        stdout.contains("Usage:"),
        "expected Usage: in bare output, got: {stdout}"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Empty repo
// ---------------------------------------------------------------------------

#[test]
fn ls_empty_repo_prints_no_meshes() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let out = repo.mesh_stdout(["ls"])?;
    assert_eq!(out.trim(), "no meshes");
    Ok(())
}

// ---------------------------------------------------------------------------
// Block format
// ---------------------------------------------------------------------------

#[test]
fn ls_one_committed_mesh_block_format() -> Result<()> {
    let repo = TestRepo::seeded()?;
    commit_mesh(
        &repo,
        "alpha",
        "file1.txt#L1-L5",
        "the parser honors the spec",
    )?;
    let out = repo.mesh_stdout(["ls"])?;
    // Header line
    assert!(
        out.contains("alpha:"),
        "expected 'alpha:' header, got: {out}"
    );
    // Why indented
    assert!(
        out.contains("the parser honors the spec"),
        "expected why, got: {out}"
    );
    // Anchor line
    assert!(
        out.contains("- file1.txt#L1-L5"),
        "expected anchor line, got: {out}"
    );
    // No state marker for committed
    assert!(
        !out.contains("(staged)") && !out.contains("(pending)"),
        "unexpected state marker, got: {out}"
    );
    Ok(())
}

#[test]
fn ls_alphabetical_order() -> Result<()> {
    let repo = TestRepo::seeded()?;
    commit_mesh(&repo, "zebra", "file1.txt#L1-L3", "z why")?;
    commit_mesh(&repo, "alpha", "file1.txt#L4-L5", "a why")?;
    let out = repo.mesh_stdout(["ls"])?;
    let alpha_pos = out.find("alpha:").expect("alpha: in output");
    let zebra_pos = out.find("zebra:").expect("zebra: in output");
    assert!(alpha_pos < zebra_pos, "alpha should come before zebra");
    Ok(())
}

#[test]
fn ls_staged_marker_on_committed_mesh_with_staged_ops() -> Result<()> {
    let repo = TestRepo::seeded()?;
    commit_mesh(
        &repo,
        "alpha",
        "file1.txt#L1-L5",
        "the spec governs the parser",
    )?;
    // Stage an additional add without committing.
    repo.mesh_stdout(["add", "alpha", "file2.txt#L1-L3"])?;
    let out = repo.mesh_stdout(["ls"])?;
    assert!(
        out.contains("alpha (staged):"),
        "expected 'alpha (staged):' header, got: {out}"
    );
    Ok(())
}

#[test]
fn ls_pending_marker_on_staging_only_mesh() -> Result<()> {
    let repo = TestRepo::seeded()?;
    // Stage but do NOT commit.
    repo.mesh_stdout(["add", "pending-mesh", "file1.txt#L1-L5"])?;
    repo.mesh_stdout(["why", "pending-mesh", "-m", "pending relationship"])?;
    let out = repo.mesh_stdout(["ls"])?;
    assert!(
        out.contains("pending-mesh (pending):"),
        "expected '(pending)' marker, got: {out}"
    );
    assert!(
        out.contains("pending relationship"),
        "expected why text, got: {out}"
    );
    Ok(())
}

#[test]
fn ls_multiline_why_renders_all_lines_indented() -> Result<()> {
    let repo = TestRepo::seeded()?;
    repo.mesh_stdout(["add", "multi", "file1.txt#L1-L5"])?;
    // Use -m with newline embedded.
    repo.mesh_stdout(["why", "multi", "-m", "line one\nline two\nline three"])?;
    repo.mesh_stdout(["commit", "multi"])?;
    let out = repo.mesh_stdout(["ls"])?;
    assert!(out.contains("line one"), "line one: {out}");
    assert!(out.contains("line two"), "line two: {out}");
    assert!(out.contains("line three"), "line three: {out}");
    Ok(())
}

#[test]
fn ls_whole_file_anchor_renders_whole_label() -> Result<()> {
    let repo = TestRepo::seeded()?;
    // Whole-file anchor (no #L anchor).
    commit_mesh(&repo, "wf", "file1.txt", "whole file relationship")?;
    let out = repo.mesh_stdout(["ls"])?;
    assert!(
        out.contains("- file1.txt"),
        "expected whole-file anchor line, got: {out}"
    );
    assert!(
        !out.contains("(whole)"),
        "human ls should drop `(whole)` decoration: {out}"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Path filter
// ---------------------------------------------------------------------------

#[test]
fn ls_path_filter_includes_matching_mesh() -> Result<()> {
    let repo = TestRepo::seeded()?;
    commit_mesh(&repo, "alpha", "file1.txt#L1-L5", "alpha why")?;
    commit_mesh(&repo, "beta", "file2.txt#L1-L3", "beta why")?;
    let out = repo.mesh_stdout(["ls", "file1.txt"])?;
    assert!(out.contains("alpha:"), "alpha should appear: {out}");
    assert!(!out.contains("beta:"), "beta should not appear: {out}");
    Ok(())
}

#[test]
fn ls_path_filter_renders_full_anchor_list() -> Result<()> {
    let repo = TestRepo::seeded()?;
    // alpha has two anchors; filter by file1.txt.
    repo.mesh_stdout(["add", "alpha", "file1.txt#L1-L5"])?;
    repo.mesh_stdout(["add", "alpha", "file2.txt#L1-L3"])?;
    repo.mesh_stdout(["why", "alpha", "-m", "dual anchor"])?;
    repo.mesh_stdout(["commit", "alpha"])?;
    let out = repo.mesh_stdout(["ls", "file1.txt"])?;
    // Both anchors should appear, not just the matching one.
    assert!(out.contains("- file1.txt#L1-L5"), "file1 anchor: {out}");
    assert!(out.contains("- file2.txt#L1-L3"), "file2 anchor: {out}");
    Ok(())
}

#[test]
fn ls_path_range_filter_overlaps_correctly() -> Result<()> {
    let repo = TestRepo::seeded()?;
    commit_mesh(&repo, "overlap", "file1.txt#L3-L7", "overlap why")?;
    commit_mesh(&repo, "nooverlap", "file1.txt#L8-L10", "no-overlap why")?;
    // Query L5-L6, should match overlap (L3-L7) but not nooverlap (L8-L10).
    let out = repo.mesh_stdout(["ls", "file1.txt#L5-L6"])?;
    assert!(out.contains("overlap:"), "expected overlap: {out}");
    assert!(
        !out.contains("nooverlap:"),
        "nooverlap should not appear: {out}"
    );
    Ok(())
}

#[test]
fn ls_whole_file_anchor_matches_any_range_query() -> Result<()> {
    let repo = TestRepo::seeded()?;
    commit_mesh(&repo, "wf", "file1.txt", "whole file")?;
    // A anchor query on the same path should match the whole-file anchor.
    let out = repo.mesh_stdout(["ls", "file1.txt#L1-L5"])?;
    assert!(out.contains("wf:"), "expected wf: {out}");
    Ok(())
}

// ---------------------------------------------------------------------------
// Porcelain
// ---------------------------------------------------------------------------

#[test]
fn ls_porcelain_emits_tab_separated_rows() -> Result<()> {
    let repo = TestRepo::seeded()?;
    commit_mesh(&repo, "alpha", "file1.txt#L1-L5", "alpha why")?;
    let out = repo.mesh_stdout(["ls", "--porcelain"])?;
    // Should be: name\tpath\tstart-end
    assert!(
        out.contains("alpha\tfile1.txt\t1-5"),
        "expected porcelain row, got: {out}"
    );
    Ok(())
}

#[test]
fn ls_porcelain_whole_file_is_zero_zero() -> Result<()> {
    let repo = TestRepo::seeded()?;
    commit_mesh(&repo, "wf", "file1.txt", "whole")?;
    let out = repo.mesh_stdout(["ls", "--porcelain"])?;
    assert!(
        out.contains("wf\tfile1.txt\t0-0"),
        "expected 0-0 for whole-file, got: {out}"
    );
    Ok(())
}

#[test]
fn ls_porcelain_pending_mesh_appears() -> Result<()> {
    let repo = TestRepo::seeded()?;
    repo.mesh_stdout(["add", "pend", "file1.txt#L1-L5"])?;
    repo.mesh_stdout(["why", "pend", "-m", "pending"])?;
    let out = repo.mesh_stdout(["ls", "--porcelain"])?;
    assert!(
        out.contains("pend\tfile1.txt\t1-5"),
        "expected pending mesh in porcelain, got: {out}"
    );
    Ok(())
}

#[test]
fn ls_filtered_porcelain_uses_authoritative_path_index() -> Result<()> {
    let repo = TestRepo::seeded()?;
    commit_mesh(&repo, "alpha", "file1.txt#L1-L5", "alpha why")?;
    commit_mesh(&repo, "beta", "file2.txt#L1-L3", "beta why")?;

    let index_refs = repo.list_refs("refs/meshes-index/v1/path/")?;
    assert!(
        !index_refs.is_empty(),
        "commit should write path-index refs"
    );

    let out = repo.mesh_stdout(["ls", "file1.txt#L3-L4", "--porcelain"])?;
    assert!(
        out.contains("alpha\tfile1.txt\t1-5"),
        "alpha should match: {out}"
    );
    assert!(!out.contains("beta\t"), "beta should not match: {out}");
    Ok(())
}

#[test]
fn ls_filtered_porcelain_renders_full_anchor_list() -> Result<()> {
    let repo = TestRepo::seeded()?;
    repo.mesh_stdout(["add", "alpha", "file1.txt#L1-L5"])?;
    repo.mesh_stdout(["add", "alpha", "file2.txt#L1-L3"])?;
    repo.mesh_stdout(["why", "alpha", "-m", "dual anchor"])?;
    repo.mesh_stdout(["commit", "alpha"])?;

    let out = repo.mesh_stdout(["ls", "file1.txt", "--porcelain"])?;

    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(
        lines,
        vec!["alpha\tfile1.txt\t1-5", "alpha\tfile2.txt\t1-3"]
    );
    Ok(())
}

#[test]
fn ls_filtered_porcelain_path_index_tracks_rename_and_delete() -> Result<()> {
    let repo = TestRepo::seeded()?;
    commit_mesh(&repo, "alpha", "file1.txt#L1-L5", "alpha why")?;

    // Rename alpha -> renamed.
    repo.mesh_stdout(["mv", "alpha", "renamed"])?;

    // Path-index lookup follows the rename.
    let after_rename = repo.mesh_stdout(["ls", "file1.txt#L3-L4", "--porcelain"])?;
    assert_eq!(after_rename.trim(), "renamed\tfile1.txt\t1-5");

    // Delete the mesh.
    repo.mesh_stdout(["delete", "renamed"])?;

    // Path-index lookup now returns empty (zero-match → exit 1 per card spec).
    // mesh_stdout checks success(), so use run_mesh to capture exit code.
    let deleted_out = repo.run_mesh(["ls", "file1.txt#L3-L4", "--porcelain"])?;
    assert_eq!(deleted_out.status.code(), Some(1));
    assert_eq!(
        String::from_utf8_lossy(&deleted_out.stderr).trim(),
        "git mesh ls: no mesh or file found for 'file1.txt#L3-L4'"
    );
    Ok(())
}

#[test]
fn ls_batch_porcelain_emits_hit_rows() -> Result<()> {
    let repo = TestRepo::seeded()?;
    commit_mesh(&repo, "alpha", "file1.txt#L1-L5", "alpha why")?;
    commit_mesh(&repo, "beta", "file2.txt#L1-L3", "beta why")?;

    let out = mesh_stdout_with_stdin(&repo, &["ls", "--batch", "--porcelain"], "file1.txt\n")?;

    assert!(
        out.contains("alpha\tfile1.txt\t1-5"),
        "alpha should match: {out}"
    );
    assert!(!out.contains("beta\t"), "beta should not match: {out}");
    Ok(())
}

#[test]
fn ls_batch_porcelain_emits_no_meshes_for_miss() -> Result<()> {
    let repo = TestRepo::seeded()?;
    commit_mesh(&repo, "alpha", "file1.txt#L1-L5", "alpha why")?;

    let out = mesh_stdout_with_stdin(&repo, &["ls", "--batch", "--porcelain"], "missing.txt\n")?;

    assert_eq!(out.trim(), "no meshes");
    Ok(())
}

#[test]
fn ls_batch_porcelain_handles_multiple_queries() -> Result<()> {
    let repo = TestRepo::seeded()?;
    commit_mesh(&repo, "alpha", "file1.txt#L1-L5", "alpha why")?;
    commit_mesh(&repo, "beta", "file2.txt#L1-L3", "beta why")?;

    let out = mesh_stdout_with_stdin(
        &repo,
        &["ls", "--batch", "--porcelain"],
        "file1.txt#L3-L4\nmissing.txt\nfile2.txt\n",
    )?;

    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(
        lines,
        vec!["alpha\tfile1.txt\t1-5", "no meshes", "beta\tfile2.txt\t1-3"]
    );
    Ok(())
}

#[test]
fn ls_batch_porcelain_includes_staged_only_meshes_across_queries() -> Result<()> {
    let repo = TestRepo::seeded()?;
    repo.mesh_stdout(["add", "pending-one", "file1.txt#L1-L2"])?;
    repo.mesh_stdout(["add", "pending-two", "file2.txt#L1-L3"])?;

    let out = mesh_stdout_with_stdin(
        &repo,
        &["ls", "--batch", "--porcelain"],
        "file1.txt#L1-L1\nfile2.txt#L2-L2\n",
    )?;

    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(
        lines,
        vec!["pending-one\tfile1.txt\t1-2", "pending-two\tfile2.txt\t1-3"]
    );
    Ok(())
}

#[test]
fn ls_batch_porcelain_includes_staged_adds_on_committed_meshes() -> Result<()> {
    let repo = TestRepo::seeded()?;
    commit_mesh(&repo, "alpha", "file1.txt#L1-L5", "alpha why")?;
    repo.mesh_stdout(["add", "alpha", "file2.txt#L1-L3"])?;

    let out = mesh_stdout_with_stdin(
        &repo,
        &["ls", "--batch", "--porcelain"],
        "file1.txt#L3-L4\nfile2.txt#L2-L2\n",
    )?;

    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(
        lines,
        vec!["alpha\tfile1.txt\t1-5", "alpha\tfile2.txt\t1-3"]
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Search
// ---------------------------------------------------------------------------

#[test]
fn ls_search_matches_name() -> Result<()> {
    let repo = TestRepo::seeded()?;
    commit_mesh(&repo, "alpha", "file1.txt#L1-L5", "some why")?;
    commit_mesh(&repo, "beta", "file2.txt#L1-L3", "other why")?;
    let out = repo.mesh_stdout(["ls", "--search", "alpha"])?;
    assert!(out.contains("alpha:"), "alpha should match: {out}");
    assert!(!out.contains("beta:"), "beta should not match: {out}");
    Ok(())
}

#[test]
fn ls_search_matches_why_line() -> Result<()> {
    let repo = TestRepo::seeded()?;
    commit_mesh(
        &repo,
        "alpha",
        "file1.txt#L1-L5",
        "the parser honors the spec",
    )?;
    commit_mesh(&repo, "beta", "file2.txt#L1-L3", "unrelated relationship")?;
    let out = repo.mesh_stdout(["ls", "--search", "parser"])?;
    assert!(out.contains("alpha:"), "alpha should match via why: {out}");
    assert!(!out.contains("beta:"), "beta should not match: {out}");
    Ok(())
}

#[test]
fn ls_search_matches_anchor_address() -> Result<()> {
    let repo = TestRepo::seeded()?;
    commit_mesh(&repo, "alpha", "file1.txt#L1-L5", "alpha why")?;
    commit_mesh(&repo, "beta", "file2.txt#L1-L3", "beta why")?;
    let out = repo.mesh_stdout(["ls", "--search", "file2"])?;
    assert!(!out.contains("alpha:"), "alpha should not match: {out}");
    assert!(out.contains("beta:"), "beta should match via anchor: {out}");
    Ok(())
}

#[test]
fn ls_search_case_insensitive_by_default() -> Result<()> {
    let repo = TestRepo::seeded()?;
    commit_mesh(
        &repo,
        "alpha",
        "file1.txt#L1-L5",
        "The Parser Honors The Spec",
    )?;
    let out = repo.mesh_stdout(["ls", "--search", "parser"])?;
    assert!(
        out.contains("alpha:"),
        "case-insensitive match expected: {out}"
    );
    Ok(())
}

#[test]
fn ls_search_case_sensitive_with_flag() -> Result<()> {
    let repo = TestRepo::seeded()?;
    commit_mesh(
        &repo,
        "alpha",
        "file1.txt#L1-L5",
        "The Parser Honors The Spec",
    )?;
    // With (?-i), lowercase "parser" should NOT match "Parser"
    let out = repo.mesh_stdout(["ls", "--search", "(?-i)parser"])?;
    assert_eq!(
        out.trim(),
        "no meshes",
        "case-sensitive should not match: {out}"
    );
    Ok(())
}

#[test]
fn ls_search_invalid_regex_exits_two() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let out = repo.run_mesh(["ls", "--search", "[invalid"])?;
    assert_eq!(
        out.status.code(),
        Some(2),
        "expected exit 2 for invalid regex"
    );
    let stderr = String::from_utf8(out.stderr)?;
    assert!(
        stderr.contains("git-mesh: invalid --search pattern"),
        "expected error message, got: {stderr}"
    );
    assert!(
        stderr.contains("[invalid"),
        "expected pattern in error, got: {stderr}"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Pagination
// ---------------------------------------------------------------------------

#[test]
fn ls_offset_skips_first_meshes() -> Result<()> {
    let repo = TestRepo::seeded()?;
    commit_mesh(&repo, "alpha", "file1.txt#L1-L3", "alpha why")?;
    commit_mesh(&repo, "beta", "file1.txt#L4-L5", "beta why")?;
    // Offset 1 skips alpha (alphabetically first).
    let out = repo.mesh_stdout(["ls", "--offset", "1"])?;
    assert!(!out.contains("alpha:"), "alpha should be skipped: {out}");
    assert!(out.contains("beta:"), "beta should appear: {out}");
    Ok(())
}

#[test]
fn ls_limit_caps_output() -> Result<()> {
    let repo = TestRepo::seeded()?;
    commit_mesh(&repo, "alpha", "file1.txt#L1-L3", "alpha why")?;
    commit_mesh(&repo, "beta", "file1.txt#L4-L5", "beta why")?;
    let out = repo.mesh_stdout(["ls", "--limit", "1"])?;
    assert!(out.contains("alpha:"), "alpha should appear: {out}");
    assert!(!out.contains("beta:"), "beta should be capped: {out}");
    Ok(())
}

#[test]
fn ls_offset_and_limit_select_second_mesh() -> Result<()> {
    let repo = TestRepo::seeded()?;
    commit_mesh(&repo, "alpha", "file1.txt#L1-L3", "alpha why")?;
    commit_mesh(&repo, "beta", "file1.txt#L4-L5", "beta why")?;
    commit_mesh(&repo, "gamma", "file2.txt#L1-L3", "gamma why")?;
    let out = repo.mesh_stdout(["ls", "--offset", "1", "--limit", "1"])?;
    assert!(!out.contains("alpha:"), "alpha skipped: {out}");
    assert!(out.contains("beta:"), "beta selected: {out}");
    assert!(!out.contains("gamma:"), "gamma capped: {out}");
    Ok(())
}

#[test]
fn ls_porcelain_pagination_emits_selected_mesh_rows() -> Result<()> {
    let repo = TestRepo::seeded()?;
    repo.mesh_stdout(["add", "alpha", "file1.txt#L1-L3"])?;
    repo.mesh_stdout(["add", "alpha", "file2.txt#L1-L3"])?;
    repo.mesh_stdout(["why", "alpha", "-m", "alpha why"])?;
    repo.mesh_stdout(["commit", "alpha"])?;
    commit_mesh(&repo, "beta", "file2.txt#L4-L5", "beta why")?;
    // --offset 1 selects beta in porcelain.
    let out = repo.mesh_stdout(["ls", "--porcelain", "--offset", "1"])?;
    assert!(out.contains("beta\t"), "expected beta rows: {out}");
    assert!(!out.contains("alpha\t"), "alpha should be skipped: {out}");
    Ok(())
}

#[test]
fn ls_pagination_after_path_filter() -> Result<()> {
    let repo = TestRepo::seeded()?;
    commit_mesh(&repo, "alpha", "file1.txt#L1-L3", "alpha")?;
    commit_mesh(&repo, "beta", "file1.txt#L4-L5", "beta")?;
    commit_mesh(&repo, "gamma", "file2.txt#L1-L3", "gamma")?;
    // Filter by file1.txt (alpha and beta), then offset 1 → beta only.
    let out = repo.mesh_stdout(["ls", "file1.txt", "--offset", "1"])?;
    assert!(!out.contains("alpha:"), "alpha skipped: {out}");
    assert!(out.contains("beta:"), "beta selected: {out}");
    assert!(!out.contains("gamma:"), "gamma not in filter: {out}");
    Ok(())
}

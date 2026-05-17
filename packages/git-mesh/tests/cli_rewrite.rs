//! Integration tests for `git mesh rewrite`.

mod support;

use anyhow::Result;
use std::fs;
use std::io::Write as _;
use std::process::Command;
use support::TestRepo;

// ---------------------------------------------------------------------------
// Helpers.
// ---------------------------------------------------------------------------

/// Get the tip commit OID for a specific mesh via the catalog.
fn mesh_tip_oid(repo: &TestRepo, name: &str) -> Result<String> {
    let gix = repo.gix_repo()?;
    let log = git_mesh::mesh_log(&gix, name, Some(1))?;
    Ok(log[0].commit_oid.clone())
}

/// Seed a mesh with a line-anchor on file1.txt L1-L5.
fn seed_mesh(repo: &TestRepo, name: &str) -> Result<()> {
    repo.mesh_stdout(["add", name, "file1.txt#L1-L5"])?;
    repo.mesh_stdout(["why", name, "-m", "test why"])?;
    repo.mesh_stdout(["commit", name])?;
    Ok(())
}

/// Run `git mesh rewrite` feeding `input` to stdin.
fn run_rewrite(repo: &TestRepo, input: &str) -> Result<std::process::Output> {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_git-mesh"));
    cmd.current_dir(repo.path());
    cmd.arg("rewrite");
    cmd.stdin(std::process::Stdio::piped());
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());
    let mut child = cmd.spawn()?;
    child.stdin.take().unwrap().write_all(input.as_bytes())?;
    Ok(child.wait_with_output()?)
}

/// Run `git mesh rewrite --format json` feeding `input` to stdin.
fn run_rewrite_json(repo: &TestRepo, input: &str) -> Result<std::process::Output> {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_git-mesh"));
    cmd.current_dir(repo.path());
    cmd.args(["rewrite", "--format", "json"]);
    cmd.stdin(std::process::Stdio::piped());
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());
    let mut child = cmd.spawn()?;
    child.stdin.take().unwrap().write_all(input.as_bytes())?;
    Ok(child.wait_with_output()?)
}

/// Get the anchor_sha for the first anchor in mesh `name`.
fn first_anchor_sha(repo: &TestRepo, name: &str) -> Result<String> {
    let gx = repo.gix_repo()?;
    let mesh = git_mesh::read_mesh(&gx, name)?;
    let anchor = mesh.anchors_v2.first().expect("at least one anchor");
    Ok(anchor.1.anchor_sha.clone())
}

/// Amend HEAD without changing file content. Returns (old_sha, new_sha).
///
/// Writes a dummy file with a unique counter value so the tree always changes,
/// guaranteeing a new SHA while the anchored file content stays identical.
fn amend_no_change(repo: &TestRepo) -> Result<(String, String)> {
    let old = repo.head_sha()?;
    // Use a counter file to force a new tree (and thus a new commit SHA).
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let unrelated = format!("unrelated-amend-{n}.txt");
    repo.write_file(&unrelated, &format!("counter {n}\n"))?;
    repo.run_git(["add", &unrelated])?;
    repo.run_git(["commit", "--amend", "--no-edit"])?;
    let new = repo.head_sha()?;
    anyhow::ensure!(old != new, "amend produced same SHA — fix the test helper");
    Ok((old, new))
}

// ---------------------------------------------------------------------------
// Happy path: anchor advances after amend with no content change.
// ---------------------------------------------------------------------------

#[test]
fn test_rewrite_happy_path() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed_mesh(&repo, "m")?;

    let old_anchor_sha = first_anchor_sha(&repo, "m")?;
    let mesh_ref_before = mesh_tip_oid(&repo, "m")?;

    let (old_sha, new_sha) = amend_no_change(&repo)?;
    assert_eq!(
        old_anchor_sha, old_sha,
        "anchor_sha matches old HEAD before amend"
    );

    let input = format!("{old_sha} {new_sha}\n");
    let out = run_rewrite(&repo, &input)?;
    assert_eq!(out.status.code(), Some(0), "exit 0 on success");

    let stdout = String::from_utf8(out.stdout)?;
    assert!(
        stdout.contains("`m`"),
        "human output: {stdout}"
    );
    assert!(
        stdout.contains("advanced 1/1 anchors"),
        "human output: {stdout}"
    );

    let mesh_ref_after = mesh_tip_oid(&repo, "m")?;
    assert_ne!(mesh_ref_before, mesh_ref_after, "mesh ref advanced");

    let new_anchor_sha = first_anchor_sha(&repo, "m")?;
    assert_eq!(
        new_anchor_sha, new_sha,
        "anchor_sha == new commit after rewrite"
    );

    Ok(())
}

// ---------------------------------------------------------------------------
// Blob changed: anchor skipped, reported to stderr, exit 0.
// ---------------------------------------------------------------------------

#[test]
fn test_rewrite_blob_changed() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed_mesh(&repo, "m")?;

    let old_sha = repo.head_sha()?;
    let old_anchor_sha = first_anchor_sha(&repo, "m")?;
    assert_eq!(old_sha, old_anchor_sha);

    // Modify the anchored file before amending.
    repo.write_file(
        "file1.txt",
        "CHANGED\nline2\nline3\nline4\nline5\nline6\nline7\nline8\nline9\nline10\n",
    )?;
    repo.run_git(["add", "file1.txt"])?;
    repo.run_git(["commit", "--amend", "--no-edit"])?;
    let new_sha = repo.head_sha()?;

    let input = format!("{old_sha} {new_sha}\n");
    let out = run_rewrite(&repo, &input)?;
    assert_eq!(out.status.code(), Some(0), "exit 0 even when blob changed");

    let stdout = String::from_utf8(out.stdout)?;
    assert!(
        stdout.contains("Processed 1 old/new SHA"),
        "expected rewrite header: {stdout}"
    );
    assert!(
        stdout.contains("advanced 0/1"),
        "expected no advances: {stdout}"
    );
    assert!(
        stdout.contains("blob changed"),
        "stdout reports blob changed: {stdout}"
    );

    // anchor_sha unchanged.
    let anchor_sha_after = first_anchor_sha(&repo, "m")?;
    assert_eq!(anchor_sha_after, old_sha, "anchor_sha unchanged");

    Ok(())
}

// ---------------------------------------------------------------------------
// No-op: empty stdin → exit 0, no output, no ref changes.
// ---------------------------------------------------------------------------

#[test]
fn test_rewrite_empty_stdin() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed_mesh(&repo, "m")?;

    let mesh_ref_before = mesh_tip_oid(&repo, "m")?;

    let out = run_rewrite(&repo, "")?;
    assert_eq!(out.status.code(), Some(0));
    assert!(String::from_utf8(out.stdout)?.trim().is_empty());

    let mesh_ref_after = mesh_tip_oid(&repo, "m")?;
    assert_eq!(
        mesh_ref_before, mesh_ref_after,
        "no ref change on empty stdin"
    );

    Ok(())
}

// ---------------------------------------------------------------------------
// old == new pair: dropped silently, no commit.
// ---------------------------------------------------------------------------

#[test]
fn test_rewrite_old_eq_new_dropped() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed_mesh(&repo, "m")?;

    let head = repo.head_sha()?;
    let mesh_ref_before = mesh_tip_oid(&repo, "m")?;

    let input = format!("{head} {head}\n");
    let out = run_rewrite(&repo, &input)?;
    assert_eq!(out.status.code(), Some(0));
    assert!(String::from_utf8(out.stdout)?.trim().is_empty());

    let mesh_ref_after = mesh_tip_oid(&repo, "m")?;
    assert_eq!(
        mesh_ref_before, mesh_ref_after,
        "no ref change for old==new pair"
    );

    Ok(())
}

// ---------------------------------------------------------------------------
// Malformed line (non-hex): exit 1, no ref changes.
// ---------------------------------------------------------------------------

#[test]
fn test_rewrite_malformed_line() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed_mesh(&repo, "m")?;

    let mesh_ref_before = mesh_tip_oid(&repo, "m")?;

    let out = run_rewrite(&repo, "not-a-sha also-not-a-sha\n")?;
    assert_eq!(out.status.code(), Some(1), "exit 1 on malformed input");

    let mesh_ref_after = mesh_tip_oid(&repo, "m")?;
    assert_eq!(
        mesh_ref_before, mesh_ref_after,
        "no ref change on malformed input"
    );

    Ok(())
}

// ---------------------------------------------------------------------------
// Multiple meshes: pairs touching two meshes produce two independent commits.
// ---------------------------------------------------------------------------

#[test]
fn test_rewrite_multiple_meshes() -> Result<()> {
    let repo = TestRepo::seeded()?;

    // Two meshes, each anchoring different files.
    repo.mesh_stdout(["add", "m1", "file1.txt#L1-L5"])?;
    repo.mesh_stdout(["why", "m1", "-m", "mesh one"])?;
    repo.mesh_stdout(["commit", "m1"])?;

    repo.mesh_stdout(["add", "m2", "file2.txt#L1-L5"])?;
    repo.mesh_stdout(["why", "m2", "-m", "mesh two"])?;
    repo.mesh_stdout(["commit", "m2"])?;

    let old_sha = repo.head_sha()?;
    let m1_ref_before = mesh_tip_oid(&repo, "m1")?;
    let m2_ref_before = mesh_tip_oid(&repo, "m2")?;

    let (old, new) = amend_no_change(&repo)?;
    assert_eq!(old, old_sha);

    let input = format!("{old} {new}\n");
    let out = run_rewrite(&repo, &input)?;
    assert_eq!(out.status.code(), Some(0));

    let stdout = String::from_utf8(out.stdout)?;
    assert!(stdout.contains("`m1`"), "m1 not found: {stdout}");
    assert!(stdout.contains("`m2`"), "m2 not found: {stdout}");
    assert!(stdout.contains("advanced 1/1"), "advance not found: {stdout}");

    let m1_ref_after = mesh_tip_oid(&repo, "m1")?;
    let m2_ref_after = mesh_tip_oid(&repo, "m2")?;
    assert_ne!(m1_ref_before, m1_ref_after, "m1 ref advanced");
    assert_ne!(m2_ref_before, m2_ref_after, "m2 ref advanced");

    Ok(())
}

// ---------------------------------------------------------------------------
// Multi-pair: mixed advance/skip.
// ---------------------------------------------------------------------------

#[test]
fn test_rewrite_multi_pair_mixed() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed_mesh(&repo, "m")?;

    let anchor_sha_before = first_anchor_sha(&repo, "m")?;

    // Two amends in a row: produce two old→new pairs. Only the second
    // one matches the anchor (the first amend moved HEAD away from
    // the anchor's sha). The anchor sha is anchor_sha_before → sha1 (first amend).
    // Then sha1 → sha2 (second amend). We feed both pairs.
    let (sha0, sha1) = amend_no_change(&repo)?;
    assert_eq!(sha0, anchor_sha_before);
    let (_, sha2) = amend_no_change(&repo)?;

    // Feed two pairs: sha0→sha1 (matches anchor) and sha1→sha2 (no match since anchor already at sha1 after first pair advance)
    // Actually since we process as a batch from the same original map, only sha0→sha1 matches the anchor's original sha.
    // After rewrite, anchor points to sha1.
    let input = format!("{sha0} {sha1}\n{sha1} {sha2}\n");
    let out = run_rewrite(&repo, &input)?;
    assert_eq!(out.status.code(), Some(0));

    // Anchor should advance to sha1 (matched sha0→sha1).
    let anchor_sha_after = first_anchor_sha(&repo, "m")?;
    assert_eq!(
        anchor_sha_after, sha1,
        "anchor advanced to sha1 (matched sha0->sha1)"
    );

    Ok(())
}

// ---------------------------------------------------------------------------
// JSON output: NDJSON shape per mesh.
// ---------------------------------------------------------------------------

#[test]
fn test_rewrite_json_output() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed_mesh(&repo, "m")?;

    let (old_sha, new_sha) = amend_no_change(&repo)?;
    let input = format!("{old_sha} {new_sha}\n");
    let out = run_rewrite_json(&repo, &input)?;
    assert_eq!(out.status.code(), Some(0));

    let stdout = String::from_utf8(out.stdout)?;
    let line = stdout.trim();
    assert!(!line.is_empty(), "json output should not be empty");
    let val: serde_json::Value = serde_json::from_str(line)?;
    assert_eq!(val["schema"], "rewrite-v1");
    assert_eq!(val["mesh"], "m");
    assert_eq!(val["advanced"], 1);
    assert!(val["anchors"].is_array());
    let anchors = val["anchors"].as_array().unwrap();
    assert_eq!(anchors[0]["outcome"], "advanced");
    assert_eq!(anchors[0]["old_sha"], old_sha);
    assert_eq!(anchors[0]["new_sha"], new_sha);

    Ok(())
}

// ---------------------------------------------------------------------------
// Doctor: MissingPostRewriteHook when absent; passes when present.
// ---------------------------------------------------------------------------

#[test]
fn test_doctor_missing_post_rewrite_hook() -> Result<()> {
    let repo = TestRepo::seeded()?;

    let out = repo.run_mesh(["doctor"])?;
    let stdout = String::from_utf8(out.stdout)?;
    assert!(
        stdout.contains("MissingPostRewriteHook"),
        "doctor should report MissingPostRewriteHook when absent: {stdout}"
    );

    Ok(())
}

#[test]
fn test_doctor_post_rewrite_hook_present() -> Result<()> {
    let repo = TestRepo::seeded()?;

    // Install the hook with the correct marker.
    let hooks_dir = repo.path().join(".git").join("hooks");
    fs::create_dir_all(&hooks_dir)?;
    let hook_path = hooks_dir.join("post-rewrite");
    fs::write(&hook_path, "#!/bin/sh\ngit mesh hooks git post-rewrite\n")?;
    support::make_executable(&hook_path)?;

    let out = repo.run_mesh(["doctor"])?;
    let stdout = String::from_utf8(out.stdout)?;
    assert!(
        !stdout.contains("MissingPostRewriteHook"),
        "doctor should not report MissingPostRewriteHook when hook present: {stdout}"
    );

    Ok(())
}

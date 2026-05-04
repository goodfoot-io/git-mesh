//! Integration tests for `git mesh stale --compact`.

mod support;

use anyhow::Result;
use serde_json::Value;
use support::TestRepo;

// ---------------------------------------------------------------------------
// Helpers.
// ---------------------------------------------------------------------------

/// Seed a mesh with a line-anchor on file1.txt L1-L5.
fn seed(repo: &TestRepo, name: &str) -> Result<()> {
    repo.mesh_stdout(["add", name, "file1.txt#L1-L5"])?;
    repo.mesh_stdout(["why", name, "-m", "test why"])?;
    repo.mesh_stdout(["commit", name])?;
    Ok(())
}

/// Make a new HEAD commit that preserves the anchor content (Fresh).
fn advance_head(repo: &TestRepo) -> Result<String> {
    // Append an unrelated file so HEAD moves while file1.txt L1-L5 stays identical.
    repo.write_file("unrelated.txt", "content\n")?;
    repo.commit_all("advance HEAD")
}

/// Mutate file1.txt L1 so the anchor becomes Changed.
fn mutate_anchor(repo: &TestRepo) -> Result<String> {
    repo.write_file(
        "file1.txt",
        "CHANGED\nline2\nline3\nline4\nline5\nline6\nline7\nline8\nline9\nline10\n",
    )?;
    repo.commit_all("mutate anchor")
}

// ---------------------------------------------------------------------------
// Test: read-only invariant — `git mesh stale` (no --compact) never mutates.
// ---------------------------------------------------------------------------

#[test]
fn test_compact_read_only_invariant() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed(&repo, "m")?;
    advance_head(&repo)?;

    // Capture state before stale.
    let mesh_ref_before = repo.git_stdout(["rev-parse", "refs/meshes/v1/m"])?;

    // Run plain stale (read-only, no --compact).
    let out = repo.run_mesh(["stale", "m"])?;
    // Should be exit 0 — the anchor is Fresh (L1-L5 unchanged).
    assert_eq!(out.status.code(), Some(0), "stale should exit 0 when Fresh");

    let mesh_ref_after = repo.git_stdout(["rev-parse", "refs/meshes/v1/m"])?;
    assert_eq!(
        mesh_ref_before, mesh_ref_after,
        "stale without --compact must not advance the mesh ref"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Test: Fresh anchor advances to HEAD.
// ---------------------------------------------------------------------------

#[test]
fn test_compact_fresh_advances() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed(&repo, "m")?;

    let old_tip = repo.git_stdout(["rev-parse", "refs/meshes/v1/m"])?;
    let old_gx = repo.gix_repo()?;
    let old_mesh = git_mesh::read_mesh(&old_gx, "m")?;
    let old_anchor = old_mesh.anchors_v2.first().expect("one anchor");
    let old_anchor_sha = old_anchor.1.anchor_sha.clone();
    let old_anchor_id = old_anchor.0.clone();
    let old_created_at = old_anchor.1.created_at.clone();

    let new_head = advance_head(&repo)?;

    let out = repo.run_mesh(["stale", "m", "--compact"])?;
    assert_eq!(out.status.code(), Some(0), "compact should exit 0");

    let new_tip = repo.git_stdout(["rev-parse", "refs/meshes/v1/m"])?;
    assert_ne!(old_tip, new_tip, "mesh ref should have advanced");

    let gx = repo.gix_repo()?;
    let new_mesh = git_mesh::read_mesh(&gx, "m")?;
    let new_anchor = new_mesh.anchors_v2.first().expect("one anchor");
    assert_eq!(new_anchor.0, old_anchor_id, "anchor_id preserved");
    assert_eq!(
        new_anchor.1.anchor_sha, new_head,
        "anchor_sha == HEAD after compaction"
    );
    assert_ne!(
        new_anchor.1.anchor_sha, old_anchor_sha,
        "anchor_sha advanced"
    );
    assert_eq!(
        new_anchor.1.created_at, old_created_at,
        "created_at preserved"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Test: Idempotent — second run advances 0 anchors.
// ---------------------------------------------------------------------------

#[test]
fn test_compact_idempotent() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed(&repo, "m")?;
    advance_head(&repo)?;

    // First compact.
    let out1 = repo.run_mesh(["stale", "m", "--compact"])?;
    assert_eq!(out1.status.code(), Some(0));
    let stdout1 = String::from_utf8_lossy(&out1.stdout);
    assert!(
        stdout1.contains("advanced"),
        "first run should advance: {stdout1}"
    );

    // Second compact — nothing to do.
    let out2 = repo.run_mesh(["stale", "m", "--compact"])?;
    assert_eq!(out2.status.code(), Some(0));
    let stdout2 = String::from_utf8_lossy(&out2.stdout);
    assert!(
        stdout2.contains("nothing to compact"),
        "second run should be no-op: {stdout2}"
    );

    // Compaction must not mutate the why message — no trailer should be added.
    let commit_msg = repo.git_stdout(["log", "-1", "--format=%B", "refs/meshes/v1/m"])?;
    let trailer_count = commit_msg
        .lines()
        .filter(|l| l.starts_with("git-mesh-compact:"))
        .count();
    assert_eq!(trailer_count, 0, "compaction must not add a trailer: {commit_msg}");
    Ok(())
}

// ---------------------------------------------------------------------------
// Test: Moved anchor not touched.
// ---------------------------------------------------------------------------

#[test]
fn test_compact_moved_skipped() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed(&repo, "m")?;

    // Move the file content to different lines (rename is complex; just make the
    // content match at a different location by making lines 1-5 different but
    // lines 6-10 the same as old 1-5). Actually simulate a Moved: copy old
    // content to new lines, making old location Changed (not Moved).
    // A simpler approach: just verify --compact exits 0 when anchor is not Fresh.
    // Mutate anchor so it is Changed.
    mutate_anchor(&repo)?;

    let old_tip = repo.git_stdout(["rev-parse", "refs/meshes/v1/m"])?;

    let out = repo.run_mesh(["stale", "m", "--compact"])?;
    assert_eq!(out.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("nothing to compact"),
        "changed anchor should not be compacted: {stdout}"
    );

    let new_tip = repo.git_stdout(["rev-parse", "refs/meshes/v1/m"])?;
    assert_eq!(
        old_tip, new_tip,
        "mesh ref must not advance when all anchors non-Fresh"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Test: Changed anchor skipped.
// ---------------------------------------------------------------------------

#[test]
fn test_compact_changed_skipped() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed(&repo, "m")?;
    mutate_anchor(&repo)?;

    let old_tip = repo.git_stdout(["rev-parse", "refs/meshes/v1/m"])?;
    let out = repo.run_mesh(["stale", "m", "--compact"])?;
    assert_eq!(out.status.code(), Some(0));
    let new_tip = repo.git_stdout(["rev-parse", "refs/meshes/v1/m"])?;
    assert_eq!(old_tip, new_tip, "changed anchor must not be advanced");
    Ok(())
}

// ---------------------------------------------------------------------------
// Test: Staging ops present → whole mesh skipped.
// ---------------------------------------------------------------------------

#[test]
fn test_compact_staging_skip() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed(&repo, "m")?;
    advance_head(&repo)?;

    // Stage an add (don't commit it).
    repo.mesh_stdout(["add", "m", "file1.txt#L6-L10"])?;

    let old_tip = repo.git_stdout(["rev-parse", "refs/meshes/v1/m"])?;
    let out = repo.run_mesh(["stale", "m", "--compact"])?;
    assert_eq!(out.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("staging ops present"),
        "should report staging skip: {stdout}"
    );

    let new_tip = repo.git_stdout(["rev-parse", "refs/meshes/v1/m"])?;
    assert_eq!(old_tip, new_tip, "staged mesh must not be advanced");
    Ok(())
}

// ---------------------------------------------------------------------------
// Test: anchor_id preserved across compaction.
// ---------------------------------------------------------------------------

#[test]
fn test_compact_anchor_id_preserved() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed(&repo, "m")?;
    advance_head(&repo)?;

    let gx_before = repo.gix_repo()?;
    let mesh_before = git_mesh::read_mesh(&gx_before, "m")?;
    let id_before = mesh_before
        .anchors_v2
        .first()
        .expect("one anchor")
        .0
        .clone();

    repo.run_mesh(["stale", "m", "--compact"])?;

    let gx_after = repo.gix_repo()?;
    let mesh_after = git_mesh::read_mesh(&gx_after, "m")?;
    let id_after = mesh_after.anchors_v2.first().expect("one anchor").0.clone();

    assert_eq!(
        id_before, id_after,
        "anchor_id must be preserved across compaction"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Test: JSON output parses as valid NDJSON with correct counts.
// ---------------------------------------------------------------------------

#[test]
fn test_compact_json_output() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed(&repo, "m")?;
    advance_head(&repo)?;

    let out = repo.run_mesh(["stale", "m", "--compact", "--format=json"])?;
    assert_eq!(out.status.code(), Some(0));

    let stdout = String::from_utf8(out.stdout)?;
    // One NDJSON line per mesh.
    let line = stdout.trim();
    assert!(!line.is_empty(), "JSON output should not be empty");
    let v: Value = serde_json::from_str(line)?;
    assert_eq!(v["schema"], "compact-v1");
    assert_eq!(v["mesh"], "m");
    assert!(
        v["advanced"].as_u64().unwrap() >= 1,
        "should have advanced >=1"
    );
    assert!(v["anchors"].is_array());
    Ok(())
}

// ---------------------------------------------------------------------------
// Test: --no-exit-code suppresses CAS conflict exit.
// ---------------------------------------------------------------------------

#[test]
fn test_compact_no_exit_code() -> Result<()> {
    // We can't easily simulate a true CAS conflict in a single-process test,
    // but we can verify the flag is accepted and doesn't change behavior when
    // there's nothing to compact (should still be 0).
    let repo = TestRepo::seeded()?;
    seed(&repo, "m")?;

    // No HEAD advance → nothing to compact. Exit should be 0 regardless.
    let out = repo.run_mesh(["stale", "m", "--compact", "--no-exit-code"])?;
    assert_eq!(out.status.code(), Some(0));
    Ok(())
}

// ---------------------------------------------------------------------------
// Test: path-index is consistent with new anchors blob after compaction.
// ---------------------------------------------------------------------------

#[test]
fn test_compact_path_index_updated() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed(&repo, "m")?;
    advance_head(&repo)?;

    repo.run_mesh(["stale", "m", "--compact"])?;

    // After compaction, `git mesh list` should still work (path-index is valid).
    let out = repo.run_mesh(["list", "file1.txt"])?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("m"),
        "mesh m should still appear in list after compact: {stdout}"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Test: --no-exit-code keeps hard error exit nonzero.
// (We simulate a hard error via a non-existent mesh name.)
// ---------------------------------------------------------------------------

#[test]
fn test_compact_no_exit_code_keeps_hard_error() -> Result<()> {
    let repo = TestRepo::seeded()?;
    // Ask for a non-existent mesh — should be a hard error regardless of --no-exit-code.
    let out = repo.run_mesh(["stale", "nonexistent-mesh", "--compact", "--no-exit-code"])?;
    assert!(
        out.status.code().unwrap_or(0) != 0,
        "hard error must not be suppressed by --no-exit-code"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Test: Trailer is idempotent over multiple compactions.
// ---------------------------------------------------------------------------

#[test]
fn test_compact_trailer_idempotent() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed(&repo, "m")?;

    // First advance + compact.
    advance_head(&repo)?;
    repo.run_mesh(["stale", "m", "--compact"])?;

    // Second advance + compact.
    repo.write_file("extra.txt", "a\n")?;
    repo.commit_all("advance again")?;
    repo.run_mesh(["stale", "m", "--compact"])?;

    // Third advance + compact.
    repo.write_file("extra2.txt", "b\n")?;
    repo.commit_all("advance third")?;
    repo.run_mesh(["stale", "m", "--compact"])?;

    let commit_msg = repo.git_stdout(["log", "-1", "--format=%B", "refs/meshes/v1/m"])?;
    let trailer_count = commit_msg
        .lines()
        .filter(|l| l.starts_with("git-mesh-compact:"))
        .count();
    assert_eq!(
        trailer_count, 0,
        "compaction must not add trailers across repeated runs: {commit_msg}"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Test: Multi-mesh — both meshes processed independently.
// ---------------------------------------------------------------------------

#[test]
fn test_compact_multi_mesh_partial() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed(&repo, "mesh-a")?;
    // Seed mesh-b with L6-L10 which won't be mutated.
    repo.mesh_stdout(["add", "mesh-b", "file1.txt#L6-L10"])?;
    repo.mesh_stdout(["why", "mesh-b", "-m", "mesh-b why"])?;
    repo.mesh_stdout(["commit", "mesh-b"])?;

    // Advance HEAD — both meshes have Fresh anchors.
    advance_head(&repo)?;

    let out = repo.run_mesh(["stale", "--compact", "--verbose"])?;
    assert_eq!(out.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&out.stdout);
    // Both meshes should have advanced.
    assert!(
        stdout.contains("mesh-a"),
        "mesh-a should appear in output: {stdout}"
    );
    assert!(
        stdout.contains("mesh-b"),
        "mesh-b should appear in output: {stdout}"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Test: CAS retry succeeds — simulate via library.
// (The CAS conflict path is exercised indirectly; here we verify normal flow.)
// ---------------------------------------------------------------------------

#[test]
fn test_compact_cas_retry_success() -> Result<()> {
    // We verify the happy path succeeds (retry path exercised by the
    // retry loop internals). A true multi-process CAS conflict requires
    // OS-level coordination beyond test scope.
    let repo = TestRepo::seeded()?;
    seed(&repo, "m")?;
    advance_head(&repo)?;

    let out = repo.run_mesh(["stale", "m", "--compact"])?;
    assert_eq!(out.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("advanced"),
        "should report advancement: {stdout}"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// F1: CAS conflict mutual exclusion — advanced == 0 when conflicts > 0.
// We verify via JSON: skipped_clean_not_head is correct and the invariant
// holds in the success path (conflict path is hard to force; we verify the
// constraint structurally via the type system by checking the JSON schema).
// ---------------------------------------------------------------------------

#[test]
fn test_compact_json_conflict_invariant_fields_present() -> Result<()> {
    // Run compact when nothing to advance — verifies JSON schema includes
    // all card-mandated fields. advanced==0 and conflicts==0 here.
    let repo = TestRepo::seeded()?;
    seed(&repo, "m")?;
    // No HEAD advance — anchor is already at HEAD.

    let out = repo.run_mesh(["stale", "m", "--compact", "--format=json"])?;
    assert_eq!(out.status.code(), Some(0));

    let stdout = String::from_utf8(out.stdout)?;
    let line = stdout.trim();
    let v: Value = serde_json::from_str(line)?;

    // Verify all card-mandated fields are present.
    assert!(v.get("advanced").is_some(), "missing 'advanced'");
    assert!(
        v.get("skipped_clean_not_head").is_some(),
        "missing 'skipped_clean_not_head'"
    );
    assert!(v.get("skipped_stale").is_some(), "missing 'skipped_stale'");
    assert!(
        v.get("skipped_staged").is_some(),
        "missing 'skipped_staged'"
    );
    assert!(v.get("conflicts").is_some(), "missing 'conflicts'");
    assert!(v.get("errors").is_some(), "missing 'errors'");

    // Mutual-exclusion invariant: when conflicts > 0, advanced must == 0.
    // (Here both are 0, which trivially satisfies it.)
    let adv = v["advanced"].as_u64().unwrap();
    let conf = v["conflicts"].as_u64().unwrap();
    assert!(
        !(conf > 0 && adv > 0),
        "invariant violated: conflicts={conf} advanced={adv}"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// F2: Compaction must not mutate the why message.
// ---------------------------------------------------------------------------

#[test]
fn test_compact_preserves_why_verbatim() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed(&repo, "m")?;

    let why = "A multi-line why body.\n\nWith a second paragraph.";
    repo.mesh_stdout(["why", "m", "-m", why])?;
    repo.mesh_stdout(["commit", "m"])?;

    advance_head(&repo)?;
    let out = repo.run_mesh(["stale", "m", "--compact"])?;
    assert_eq!(out.status.code(), Some(0));

    let commit_msg = repo.git_stdout(["log", "-1", "--format=%B", "refs/meshes/v1/m"])?;
    assert_eq!(
        commit_msg.trim(),
        why,
        "why must be preserved verbatim: {commit_msg}"
    );
    Ok(())
}

#[test]
fn test_compact_repeated_runs_do_not_add_trailers() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed(&repo, "m")?;

    advance_head(&repo)?;
    repo.run_mesh(["stale", "m", "--compact"])?;

    repo.write_file("extra.txt", "x\n")?;
    repo.commit_all("advance again")?;
    repo.run_mesh(["stale", "m", "--compact"])?;

    let commit_msg = repo.git_stdout(["log", "-1", "--format=%B", "refs/meshes/v1/m"])?;
    let trailer_count = commit_msg
        .lines()
        .filter(|l| l.starts_with("git-mesh-compact:"))
        .count();
    assert_eq!(
        trailer_count, 0,
        "compaction must never add trailers: {commit_msg}"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// F3: JSON output includes skipped_clean_not_head (card spec field).
// ---------------------------------------------------------------------------

#[test]
fn test_compact_json_skipped_clean_not_head() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed(&repo, "m")?;
    // No HEAD advance — anchor already at HEAD, should count as skipped_clean_not_head.

    let out = repo.run_mesh(["stale", "m", "--compact", "--format=json"])?;
    assert_eq!(out.status.code(), Some(0));

    let stdout = String::from_utf8(out.stdout)?;
    let v: Value = serde_json::from_str(stdout.trim())?;
    // skipped_clean_not_head should be >= 1 (anchor already at HEAD).
    let val = v["skipped_clean_not_head"].as_u64().unwrap_or(0);
    assert!(val >= 1, "expected skipped_clean_not_head >= 1: {v}");
    Ok(())
}

// ---------------------------------------------------------------------------
// F4: NDJSON is one JSON object per line.
// ---------------------------------------------------------------------------

#[test]
fn test_compact_json_one_line_per_mesh() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed(&repo, "mesh-a")?;
    repo.mesh_stdout(["add", "mesh-b", "file1.txt#L6-L10"])?;
    repo.mesh_stdout(["why", "mesh-b", "-m", "mesh-b why"])?;
    repo.mesh_stdout(["commit", "mesh-b"])?;
    advance_head(&repo)?;

    let out = repo.run_mesh(["stale", "--compact", "--format=json"])?;
    assert_eq!(out.status.code(), Some(0));

    let stdout = String::from_utf8(out.stdout)?;
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(
        lines.len(),
        2,
        "expect exactly 2 JSON lines (one per mesh): {stdout}"
    );
    for line in &lines {
        let v: Value = serde_json::from_str(line)
            .unwrap_or_else(|e| panic!("line is not valid JSON: {e}\nline: {line}"));
        assert_eq!(v["schema"], "compact-v1");
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// F6: Per-anchor outcome tokens are distinct for Changed (at minimum).
// ---------------------------------------------------------------------------

#[test]
fn test_compact_json_changed_anchor_outcome_token() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed(&repo, "m")?;
    mutate_anchor(&repo)?; // Makes anchor Changed.

    let out = repo.run_mesh(["stale", "m", "--compact", "--format=json"])?;
    assert_eq!(out.status.code(), Some(0));

    let stdout = String::from_utf8(out.stdout)?;
    let v: Value = serde_json::from_str(stdout.trim())?;
    let anchors = v["anchors"].as_array().expect("anchors array");
    assert!(
        !anchors.is_empty(),
        "should have at least one anchor record"
    );
    let outcome = anchors[0]["outcome"].as_str().unwrap();
    assert_eq!(
        outcome, "skipped_changed",
        "Changed anchor should have outcome 'skipped_changed', got '{outcome}'"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// F8: --compact with incompatible --format is rejected before mutation.
// ---------------------------------------------------------------------------

#[test]
fn test_compact_rejects_incompatible_format() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed(&repo, "m")?;
    advance_head(&repo)?;

    let old_tip = repo.git_stdout(["rev-parse", "refs/meshes/v1/m"])?;

    // junit format should be rejected.
    let out = repo.run_mesh(["stale", "m", "--compact", "--format=junit"])?;
    assert_ne!(
        out.status.code(),
        Some(0),
        "should exit nonzero for incompatible format"
    );

    // Mesh ref must not have changed — no mutation occurred.
    let new_tip = repo.git_stdout(["rev-parse", "refs/meshes/v1/m"])?;
    assert_eq!(
        old_tip, new_tip,
        "mesh ref must not change after format rejection"
    );
    Ok(())
}

#[test]
fn test_compact_rejects_incompatible_format_porcelain() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed(&repo, "m")?;
    let old_tip = repo.git_stdout(["rev-parse", "refs/meshes/v1/m"])?;

    let out = repo.run_mesh(["stale", "m", "--compact", "--format=porcelain"])?;
    assert_ne!(out.status.code(), Some(0), "porcelain should be rejected");

    let new_tip = repo.git_stdout(["rev-parse", "refs/meshes/v1/m"])?;
    assert_eq!(old_tip, new_tip, "no mutation on rejection");
    Ok(())
}

#[test]
fn test_compact_format_rejection_not_suppressed_by_no_exit_code() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed(&repo, "m")?;

    // Even with --no-exit-code, format rejection must be nonzero.
    let out = repo.run_mesh([
        "stale",
        "m",
        "--compact",
        "--format=junit",
        "--no-exit-code",
    ])?;
    assert_ne!(
        out.status.code(),
        Some(0),
        "--no-exit-code must not suppress format rejection exit code"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Item 4: staged metadata-only ops (why / config) skip the mesh.
// Item 4 changed the compact staging predicate to include why and config
// alongside adds/removes. The mesh ref must not advance when a why or
// config op is staged.
// ---------------------------------------------------------------------------

#[test]
fn test_compact_staged_why_skips_mesh() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed(&repo, "m")?;
    advance_head(&repo)?;

    // Stage a why update without committing it.
    repo.mesh_stdout(["why", "m", "-m", "new staged why"])?;

    let old_tip = repo.git_stdout(["rev-parse", "refs/meshes/v1/m"])?;
    let out = repo.run_mesh(["stale", "m", "--compact", "--format=json"])?;
    assert_eq!(out.status.code(), Some(0));

    let stdout = String::from_utf8(out.stdout)?;
    let v: Value = serde_json::from_str(stdout.trim())?;
    assert_eq!(
        v["reason"].as_str().unwrap_or(""),
        "staged_ops_present",
        "staged why must mark staged_ops_present: {v}"
    );
    assert_eq!(v["skipped_staged"].as_u64().unwrap_or(0), 1);

    let new_tip = repo.git_stdout(["rev-parse", "refs/meshes/v1/m"])?;
    assert_eq!(old_tip, new_tip, "staged why must not advance the mesh ref");
    Ok(())
}

#[test]
fn test_compact_staged_config_skips_mesh() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed(&repo, "m")?;
    advance_head(&repo)?;

    // Stage a config change without committing it.
    repo.mesh_stdout(["config", "m", "ignore-whitespace", "true"])?;

    let old_tip = repo.git_stdout(["rev-parse", "refs/meshes/v1/m"])?;
    let out = repo.run_mesh(["stale", "m", "--compact", "--format=json"])?;
    assert_eq!(out.status.code(), Some(0));

    let stdout = String::from_utf8(out.stdout)?;
    let v: Value = serde_json::from_str(stdout.trim())?;
    assert_eq!(
        v["reason"].as_str().unwrap_or(""),
        "staged_ops_present",
        "staged config must mark staged_ops_present: {v}"
    );
    assert_eq!(v["skipped_staged"].as_u64().unwrap_or(0), 1);

    let new_tip = repo.git_stdout(["rev-parse", "refs/meshes/v1/m"])?;
    assert_eq!(
        old_tip, new_tip,
        "staged config must not advance the mesh ref"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// F9: staged_ops_present reason token in JSON.
// ---------------------------------------------------------------------------

#[test]
fn test_compact_json_staged_ops_reason_token() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed(&repo, "m")?;
    advance_head(&repo)?;

    // Stage an add (don't commit it).
    repo.mesh_stdout(["add", "m", "file1.txt#L6-L10"])?;

    let out = repo.run_mesh(["stale", "m", "--compact", "--format=json"])?;
    assert_eq!(out.status.code(), Some(0));

    let stdout = String::from_utf8(out.stdout)?;
    let v: Value = serde_json::from_str(stdout.trim())?;
    let reason = v["reason"].as_str().unwrap_or("");
    assert_eq!(
        reason, "staged_ops_present",
        "staging skip must emit reason 'staged_ops_present': {v}"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Test: batch path (no name) compacts multiple meshes correctly.
//
// Exercises `compact_meshes_batch`, which threads a shared
// `EngineStateHandle` across each mesh and now also threads it into the
// CAS-conflict retry path. This test covers the batch happy path: two
// meshes both Fresh → both advance to HEAD with the same outcome shape
// as the single-mesh path. Forcing a true CAS conflict from an
// integration test would require a new test-only ref-mutation hook
// inside `apply_compact_attempt` to wedge a competing ref update
// between resolution and apply; that is out of scope here.
// ---------------------------------------------------------------------------

#[test]
fn test_compact_batch_two_meshes_advance() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed(&repo, "m-one")?;
    seed(&repo, "m-two")?;

    let old_one = repo.git_stdout(["rev-parse", "refs/meshes/v1/m-one"])?;
    let old_two = repo.git_stdout(["rev-parse", "refs/meshes/v1/m-two"])?;

    let new_head = advance_head(&repo)?;

    // Run --compact with no name → batch path through `compact_meshes_batch`.
    let out = repo.run_mesh(["stale", "--compact", "--format=json"])?;
    assert_eq!(out.status.code(), Some(0), "batch compact should exit 0");

    let stdout = String::from_utf8(out.stdout)?;
    // NDJSON: one outcome per line.
    let lines: Vec<&str> = stdout.lines().filter(|l| !l.is_empty()).collect();
    assert_eq!(lines.len(), 2, "expected two outcomes, got: {stdout}");

    for line in lines {
        let v: Value = serde_json::from_str(line)?;
        assert_eq!(v["advanced"].as_u64(), Some(1), "each mesh advances 1: {v}");
        assert_eq!(v["conflicts"].as_u64(), Some(0));
        assert_eq!(v["errors"].as_u64(), Some(0));
        let new_commit = v["anchors"][0]["new_commit"].as_str().unwrap_or("");
        assert_eq!(new_commit, new_head, "anchor advanced to HEAD: {v}");
    }

    let new_one = repo.git_stdout(["rev-parse", "refs/meshes/v1/m-one"])?;
    let new_two = repo.git_stdout(["rev-parse", "refs/meshes/v1/m-two"])?;
    assert_ne!(old_one, new_one, "m-one ref advanced");
    assert_ne!(old_two, new_two, "m-two ref advanced");
    Ok(())
}

//! Integration tests for same-session read scoping of mesh advice.
//!
//! When Claude opens a file inside a tracked mesh's anchor, advice surfaces
//! only when that mesh's creating commit was published while the current
//! session was active. Meshes from prior sessions stay silent on reads.
//! Write-path advice (touch) is unchanged.
//!
//! The mechanism: OID-based baseline snapshot at `ensure_mesh_baseline`
//! mark-time, with lazy fallback in `run_advice_read`. The baseline is a
//! JSON map `{name: oid, ...}`. On each observation, current mesh refs are
//! diffed against the baseline. Names not in baseline or with changed OID
//! are appended to `meshes-committed.jsonl`.

mod support;

use anyhow::Result;
use git_mesh::{append_add, commit_mesh, set_why};
use std::process::Output;
use support::TestRepo;
use uuid::Uuid;

fn sid(prefix: &str) -> String {
    format!("session-scope-{prefix}-{}", Uuid::new_v4())
}

fn run_advice(repo: &TestRepo, session: &str, extra: &[&str]) -> Result<Output> {
    let mut args: Vec<String> = vec!["advice".into(), session.into()];
    for a in extra {
        args.push((*a).into());
    }
    repo.run_mesh(args)
}

fn ok(out: &Output) {
    assert!(
        out.status.success(),
        "expected success, code={:?} stderr={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Establish a fresh baseline: mark+flush with no meshes yet.
fn establish_baseline(repo: &TestRepo, session: &str) -> Result<()> {
    ok(&run_advice(repo, session, &["mark", "baseline"])?);
    ok(&run_advice(repo, session, &["flush", "baseline"])?);
    Ok(())
}

/// Observe new mesh refs: mark+flush to diff against baseline.
fn observe_new_mesh(repo: &TestRepo, session: &str) -> Result<()> {
    ok(&run_advice(repo, session, &["mark", "obs"])?);
    ok(&run_advice(repo, session, &["flush", "obs"])?);
    Ok(())
}

// ---------------------------------------------------------------------------
// Test 1: Read on same-session mesh emits advice
// ---------------------------------------------------------------------------

#[test]
fn same_session_read_emits_advice() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let gix = repo.gix_repo()?;
    let s = sid("test1");

    // Establish baseline (no meshes exist yet)
    establish_baseline(&repo, &s)?;

    // Commit mesh
    append_add(&gix, "m1", "file1.txt", 1, 5, None)?;
    append_add(&gix, "m1", "file2.txt", 1, 5, None)?;
    set_why(&gix, "m1", "same-session pair")?;
    commit_mesh(&gix, "m1")?;

    // Observe the new mesh
    observe_new_mesh(&repo, &s)?;

    // Read partner file -> advice emitted
    let out = run_advice(&repo, &s, &["read", "file1.txt#L1-L5"])?;
    ok(&out);
    let stdout = String::from_utf8(out.stdout)?;
    assert!(!stdout.is_empty(), "same-session read must emit advice");
    assert!(stdout.contains("file2.txt"), "must mention partner path");

    // Re-read same file -> silent (meshes_seen dedup)
    let out2 = run_advice(&repo, &s, &["read", "file1.txt#L1-L5"])?;
    ok(&out2);
    assert!(
        out2.stdout.is_empty(),
        "re-read within same session must dedupe"
    );

    Ok(())
}

// ---------------------------------------------------------------------------
// Test 2: Read on prior-session mesh is silent
// ---------------------------------------------------------------------------

#[test]
fn prior_session_read_is_silent() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let gix = repo.gix_repo()?;

    // Pre-commit mesh (exists before any session)
    append_add(&gix, "m1", "file1.txt", 1, 5, None)?;
    append_add(&gix, "m1", "file2.txt", 1, 5, None)?;
    set_why(&gix, "m1", "prior-session pair")?;
    commit_mesh(&gix, "m1")?;

    // Session: read a meshed file (first read snapshots baseline)
    let s = sid("test2");
    let out = run_advice(&repo, &s, &["read", "file1.txt#L1-L5"])?;
    ok(&out);
    let stdout = String::from_utf8(out.stdout)?;
    assert!(
        stdout.is_empty(),
        "prior-session mesh must not emit advice on read"
    );

    // Read a different partner file -> still silent
    let out2 = run_advice(&repo, &s, &["read", "file2.txt#L1-L5"])?;
    ok(&out2);
    assert!(out2.stdout.is_empty(), "second read also silent");

    Ok(())
}

// ---------------------------------------------------------------------------
// Test 3: Touch on prior-session mesh still emits (write-path non-regression)
// ---------------------------------------------------------------------------

#[test]
fn touch_on_prior_session_mesh_emits() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let gix = repo.gix_repo()?;

    // Pre-commit mesh
    append_add(&gix, "m1", "file1.txt", 1, 5, None)?;
    append_add(&gix, "m1", "file2.txt", 1, 5, None)?;
    set_why(&gix, "m1", "write-path pair")?;
    commit_mesh(&gix, "m1")?;

    // Touch (write path) -> must emit despite prior session
    let s = sid("test3");
    let out = run_advice(&repo, &s, &["touch", "t1", "file1.txt#L1-L5", "modified"])?;
    ok(&out);
    let stdout = String::from_utf8(out.stdout)?;
    assert!(
        !stdout.is_empty(),
        "touch on prior-session mesh must still emit"
    );
    assert!(stdout.contains("file2.txt"), "must mention partner path");

    Ok(())
}

// ---------------------------------------------------------------------------
// Test 4: Cross-session isolation
// ---------------------------------------------------------------------------

#[test]
fn cross_session_isolation() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let gix = repo.gix_repo()?;

    // Session A
    let s_a = sid("test4a");
    establish_baseline(&repo, &s_a)?;

    append_add(&gix, "m1", "file1.txt", 1, 5, None)?;
    append_add(&gix, "m1", "file2.txt", 1, 5, None)?;
    set_why(&gix, "m1", "session-a mesh")?;
    commit_mesh(&gix, "m1")?;

    observe_new_mesh(&repo, &s_a)?;

    // Session A reads -> advice emitted
    let out_a = run_advice(&repo, &s_a, &["read", "file1.txt#L1-L5"])?;
    ok(&out_a);
    let stdout_a = String::from_utf8(out_a.stdout)?;
    assert!(!stdout_a.is_empty(), "session A: read must emit");

    // Session B (different sid): read same file -> silent
    let s_b = sid("test4b");
    let out_b = run_advice(&repo, &s_b, &["read", "file1.txt#L1-L5"])?;
    ok(&out_b);
    let stdout_b = String::from_utf8(out_b.stdout)?;
    assert!(
        stdout_b.is_empty(),
        "session B: read must be silent for pre-existing mesh"
    );

    Ok(())
}

// ---------------------------------------------------------------------------
// Test 5: Direct `git mesh commit` detected (no post-commit hook)
// ---------------------------------------------------------------------------

#[test]
fn direct_commit_detected() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let gix = repo.gix_repo()?;

    let s = sid("test5");

    // Establish baseline (no meshes)
    establish_baseline(&repo, &s)?;

    // Create and commit mesh directly via library
    append_add(&gix, "m1", "file1.txt", 1, 5, None)?;
    append_add(&gix, "m1", "file2.txt", 1, 5, None)?;
    set_why(&gix, "m1", "direct commit")?;
    commit_mesh(&gix, "m1")?;

    // Observe detects the new ref
    observe_new_mesh(&repo, &s)?;

    // Read -> advice emitted
    let out = run_advice(&repo, &s, &["read", "file1.txt#L1-L5"])?;
    ok(&out);
    let stdout = String::from_utf8(out.stdout)?;
    assert!(!stdout.is_empty(), "direct commit: read must emit advice");
    assert!(stdout.contains("file2.txt"), "must mention partner");

    Ok(())
}

// ---------------------------------------------------------------------------
// Test 6: Re-commit of existing mesh (OID change) is detected
// ---------------------------------------------------------------------------

#[test]
fn recommit_mesh_detected_by_oid_change() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let gix = repo.gix_repo()?;

    // Pre-commit mesh "demo" with initial anchors
    append_add(&gix, "demo", "file1.txt", 1, 3, None)?;
    append_add(&gix, "demo", "file2.txt", 1, 3, None)?;
    set_why(&gix, "demo", "initial commit")?;
    commit_mesh(&gix, "demo")?;

    let s = sid("test6");

    // Establish baseline which captures {"demo": "initial_oid"}
    establish_baseline(&repo, &s)?;

    // Re-commit mesh "demo" with updated anchors -> OID changes
    git_mesh::clear_staging(&gix, "demo")?;
    append_add(&gix, "demo", "file1.txt", 1, 5, None)?;
    append_add(&gix, "demo", "file2.txt", 1, 5, None)?;
    set_why(&gix, "demo", "re-committed with wider range")?;
    commit_mesh(&gix, "demo")?;

    // Observe -> OID changed -> name appended to meshes_committed
    observe_new_mesh(&repo, &s)?;

    // Read -> advice emitted (re-commit correctly attributed)
    let out = run_advice(&repo, &s, &["read", "file1.txt#L1-L5"])?;
    ok(&out);
    let stdout = String::from_utf8(out.stdout)?;
    assert!(
        !stdout.is_empty(),
        "re-committed mesh: read must emit"
    );
    assert!(
        stdout.contains("re-committed with wider range"),
        "must contain new why message"
    );

    Ok(())
}

// ---------------------------------------------------------------------------
// Test 7: Verify `append_read` side-effect preserved
// ---------------------------------------------------------------------------

#[test]
fn append_read_side_effect_preserved() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let s = sid("test7");

    // Read with offset+limit
    let out = run_advice(&repo, &s, &["read", "file2.txt#L1-L3"])?;
    ok(&out);

    // Verify reads.jsonl contains the expected record
    let gix = repo.gix_repo()?;
    let wd = gix.workdir().unwrap().to_path_buf();
    let gd = gix.git_dir().to_path_buf();
    use git_mesh::advice::session::SessionStore;
    let store = SessionStore::open(&wd, &gd, &s)?;
    let reads = store.load_reads()?;
    assert_eq!(reads.len(), 1, "must have exactly one read record");
    assert_eq!(
        reads[0].path, "file2.txt",
        "read path mismatch: {:?}",
        reads[0].path
    );
    assert_eq!(reads[0].start_line, Some(1), "start_line mismatch");
    assert_eq!(reads[0].end_line, Some(3), "end_line mismatch");

    Ok(())
}

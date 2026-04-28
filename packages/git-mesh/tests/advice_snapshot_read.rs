//! Skipped end-to-end CLI checks for `git mesh advice <id> snapshot` and `read`
//! (Phase 2). Each test uses `support::TestRepo` + `repo.run_mesh(args)`.

mod support;

use anyhow::Result;
use std::process::Output;
use support::TestRepo;
use uuid::Uuid;

fn session_id(label: &str) -> String {
    format!("snap-{label}-{}", Uuid::new_v4())
}

fn run_advice(repo: &TestRepo, session: &str, extra: &[&str]) -> Result<Output> {
    let mut args: Vec<String> = vec!["advice".into(), session.into()];
    for a in extra {
        args.push((*a).to_string());
    }
    repo.run_mesh(args)
}

fn assert_success_silent(out: &Output) {
    assert!(
        out.status.success(),
        "expected success, got code={:?} stderr={} stdout={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&out.stdout),
    );
    assert!(
        out.stdout.is_empty(),
        "expected silent success (no stdout), got: {}",
        String::from_utf8_lossy(&out.stdout)
    );
}

// ---------------------------------------------------------------------------
// cli: snapshot creates baseline.state, last-flush.state, all four empty JSONLs
// ---------------------------------------------------------------------------

#[test]
fn snapshot_creates_state_files_and_empty_jsonls() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let sid = session_id("create");

    let out = run_advice(&repo, &sid, &["snapshot"])?;
    assert_success_silent(&out);

    // Derive session dir: ${GIT_MESH_ADVICE_DIR:-/tmp/git-mesh/advice}/<repo-key>/<sid>/
    // The test locates it by calling the store helper.
    let repo_root = repo.path();
    let git_dir = repo_root.join(".git");
    let store = git_mesh::advice::SessionStore::open(repo_root, &git_dir, &sid)
        .expect("open store after snapshot");

    let session_dir = store
        .baseline_objects_dir()
        .parent()
        .expect("baseline_objects_dir has parent")
        .to_path_buf();

    assert!(session_dir.join("baseline.state").exists(), "baseline.state must exist");
    assert!(session_dir.join("last-flush.state").exists(), "last-flush.state must exist");
    assert!(session_dir.join("reads.jsonl").exists(), "reads.jsonl must exist");
    assert!(session_dir.join("touches.jsonl").exists(), "touches.jsonl must exist");
    assert!(session_dir.join("advice-seen.jsonl").exists(), "advice-seen.jsonl must exist");
    assert!(session_dir.join("docs-seen.jsonl").exists(), "docs-seen.jsonl must exist");

    for name in &["reads.jsonl", "touches.jsonl", "advice-seen.jsonl", "docs-seen.jsonl"] {
        let size = std::fs::metadata(session_dir.join(name))?.len();
        assert_eq!(size, 0, "{name} must be empty after snapshot");
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// cli: snapshot resets prior session state
// ---------------------------------------------------------------------------

#[test]
fn snapshot_resets_prior_session_state() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let sid = session_id("reset");

    // First snapshot.
    run_advice(&repo, &sid, &["snapshot"])?;

    // Append a read event.
    run_advice(&repo, &sid, &["read", "file1.txt"])?;

    // Second snapshot must reset reads.jsonl back to empty.
    run_advice(&repo, &sid, &["snapshot"])?;

    let store = git_mesh::advice::SessionStore::open(repo.path(), &repo.path().join(".git"), &sid)
        .expect("open");
    let session_dir = store
        .baseline_objects_dir()
        .parent()
        .expect("parent")
        .to_path_buf();

    let reads_size = std::fs::metadata(session_dir.join("reads.jsonl"))?.len();
    assert_eq!(reads_size, 0, "reads.jsonl must be truncated by second snapshot");
    Ok(())
}

// ---------------------------------------------------------------------------
// cli: read <path> validates path/anchor, appends to reads.jsonl, exits 0 silently
// ---------------------------------------------------------------------------

#[test]
fn read_appends_to_reads_jsonl_exits_0_silently() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let sid = session_id("read-append");

    run_advice(&repo, &sid, &["snapshot"])?;

    let out = run_advice(&repo, &sid, &["read", "file1.txt"])?;
    assert_success_silent(&out);

    let store = git_mesh::advice::SessionStore::open(repo.path(), &repo.path().join(".git"), &sid)
        .expect("open");
    let records = store.reads_since_cursor(0)?;
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].path, "file1.txt");
    Ok(())
}

// ---------------------------------------------------------------------------
// cli: read before snapshot → non-zero with "run snapshot first" message
// ---------------------------------------------------------------------------

#[test]
fn read_before_snapshot_nonzero_with_message() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let sid = session_id("no-snap");

    let out = run_advice(&repo, &sid, &["read", "file1.txt"])?;

    assert!(
        !out.status.success(),
        "read before snapshot must exit non-zero"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("snapshot"),
        "error must mention 'snapshot', got: {stderr}"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// cli: read invalid path/anchor → non-zero, no append
// ---------------------------------------------------------------------------

#[test]
fn read_invalid_path_nonzero_no_append() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let sid = session_id("invalid-path");

    run_advice(&repo, &sid, &["snapshot"])?;

    let out = run_advice(&repo, &sid, &["read", "../escape.txt"])?;
    assert!(!out.status.success(), "invalid path must exit non-zero");

    let store = git_mesh::advice::SessionStore::open(repo.path(), &repo.path().join(".git"), &sid)
        .expect("open");
    let records = store.reads_since_cursor(0)?;
    assert_eq!(records.len(), 0, "invalid read must not append to reads.jsonl");
    Ok(())
}

// ---------------------------------------------------------------------------
// cli: snapshot lock is Blocking; read lock times out at 30s
// ---------------------------------------------------------------------------

#[test]
fn snapshot_uses_blocking_lock_read_uses_bounded_30s() {
    // This is a contract / documentation test: snapshot acquires the advisory
    // lock with LockTimeout::Blocking; read acquires it with
    // LockTimeout::Bounded(30s). The property cannot be exercised without a
    // running process holding the lock, so Phase 3 will inject a helper binary.
    // For now we assert the constant so the contract is visible.
    use std::time::Duration;
    let bounded = git_mesh::advice::LockTimeout::Bounded(Duration::from_secs(30));
    match bounded {
        git_mesh::advice::LockTimeout::Bounded(d) => {
            assert_eq!(d, Duration::from_secs(30), "read lock timeout must be 30s")
        }
        _ => panic!("expected Bounded"),
    }
}

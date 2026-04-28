//! Skipped checks for the file-backed session store (Phase 2).
//!
//! Each test is `#[ignore = "phase-3-pending: ..."]` — they compile but do
//! not run until Phase 3 unskips them one-by-one as the implementation lands.

mod support;

use git_mesh::advice::{LockTimeout, SessionStore};
use std::path::PathBuf;
use std::time::Duration;
use uuid::Uuid;

/// Return an unused session-id scoped to this test invocation.
fn session_id(label: &str) -> String {
    format!("store-{label}-{}", Uuid::new_v4())
}

// ---------------------------------------------------------------------------
// Helper — fake repo root / git_dir inside a temp dir
// ---------------------------------------------------------------------------

fn fake_repo_dirs() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let repo_root = tmp.path().join("repo");
    let git_dir = repo_root.join(".git");
    std::fs::create_dir_all(&git_dir).expect("mkdir .git");
    (tmp, repo_root, git_dir)
}

// ---------------------------------------------------------------------------
// store: open creates dir mode 0700, acquires lock, releases on drop
// ---------------------------------------------------------------------------

#[test]
fn open_creates_dir_mode_0700_acquires_lock_releases_on_drop() {
    let (_tmp, repo_root, git_dir) = fake_repo_dirs();
    let sid = session_id("open");

    let store = SessionStore::open(&repo_root, &git_dir, &sid).expect("open should succeed");

    // Directory must exist under GIT_MESH_ADVICE_DIR / repo-key / session-id
    // The test verifies dir existence and mode 0700 on the session directory.
    let store_dir = store
        .baseline_objects_dir()
        .parent()
        .expect("baseline_objects_dir has parent")
        .to_path_buf();
    assert!(store_dir.exists(), "session dir must exist");

    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let meta = std::fs::metadata(&store_dir).expect("metadata");
        // Mode bits: 0o700
        assert_eq!(meta.mode() & 0o777, 0o700, "dir must be mode 0700");
    }

    // Drop releases the lock — a second open on the same session must succeed.
    drop(store);
    let _store2 = SessionStore::open(&repo_root, &git_dir, &sid)
        .expect("second open after drop should succeed");
}

// ---------------------------------------------------------------------------
// store: LockTimeout::Bounded(30s) returns timeout error when held
// ---------------------------------------------------------------------------

#[test]
fn bounded_lock_timeout_when_held() {
    use git_mesh::advice::store::{acquire_lock, advice_base_dir, repo_key};

    let (_tmp, repo_root, git_dir) = fake_repo_dirs();
    let key = repo_key(&repo_root, &git_dir);
    let base = advice_base_dir();
    let dir = base.join(&key).join("bounded-lock-test");
    std::fs::create_dir_all(&dir).expect("mkdir");

    let _guard = acquire_lock(&dir, LockTimeout::Blocking).expect("first acquire");

    // Second acquire with Bounded(30ms) must return an error immediately.
    let result = acquire_lock(&dir, LockTimeout::Bounded(Duration::from_millis(30)));
    assert!(
        result.is_err(),
        "bounded acquire must fail when lock is held"
    );
}

// ---------------------------------------------------------------------------
// store: LockTimeout::Blocking waits across a held guard until released
// ---------------------------------------------------------------------------

#[test]
fn blocking_lock_waits_until_released() {
    use git_mesh::advice::store::{acquire_lock, advice_base_dir, repo_key};
    use std::sync::Arc;

    let (_tmp, repo_root, git_dir) = fake_repo_dirs();
    let key = repo_key(&repo_root, &git_dir);
    let base = advice_base_dir();
    let dir = Arc::new(base.join(&key).join("blocking-lock-test"));
    std::fs::create_dir_all(dir.as_ref()).expect("mkdir");

    let dir_clone = Arc::clone(&dir);
    let guard = acquire_lock(dir.as_ref(), LockTimeout::Blocking).expect("first acquire");

    let handle = std::thread::spawn(move || {
        // Blocking acquire — must succeed once guard is dropped by main thread.
        acquire_lock(dir_clone.as_ref(), LockTimeout::Blocking)
            .expect("blocking acquire must succeed after guard is dropped")
    });

    // Release the guard after a short delay.
    std::thread::sleep(Duration::from_millis(50));
    drop(guard);

    handle.join().expect("thread must not panic");
}

// ---------------------------------------------------------------------------
// store: atomic_write writes via .tmp + rename and is observable atomically
// ---------------------------------------------------------------------------

#[test]
fn atomic_write_via_tmp_rename() {
    use git_mesh::advice::store::atomic_write;

    let tmp = tempfile::tempdir().expect("tempdir");
    let dest = tmp.path().join("state.json");
    let contents = b"{\"schema_version\":1}";

    atomic_write(&dest, contents).expect("atomic_write");

    let written = std::fs::read(&dest).expect("read dest");
    assert_eq!(written, contents);
    // No .tmp sibling must remain.
    let tmp_sibling = tmp.path().join("state.json.tmp");
    assert!(!tmp_sibling.exists(), ".tmp sibling must be cleaned up");
}

// ---------------------------------------------------------------------------
// store: append_read appends JSONL; reads_since_cursor parses tail
// ---------------------------------------------------------------------------

#[test]
fn append_read_and_reads_since_cursor() {
    use git_mesh::advice::state::ReadRecord;

    let (_tmp, repo_root, git_dir) = fake_repo_dirs();
    let sid = session_id("append-read");
    let store = SessionStore::open(&repo_root, &git_dir, &sid).expect("open");

    let rec = ReadRecord {
        path: "src/main.rs".into(),
        start_line: Some(10),
        end_line: Some(20),
        ts: "2026-01-01T00:00:00Z".into(),
    };

    store
        .append_read(&rec, LockTimeout::Blocking)
        .expect("append_read");

    let cursor: u64 = 0;
    let records = store
        .reads_since_cursor(cursor)
        .expect("reads_since_cursor");
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].path, "src/main.rs");
}

// ---------------------------------------------------------------------------
// store: malformed JSONL line → fail closed with file + line number
// ---------------------------------------------------------------------------

#[test]
fn malformed_jsonl_fails_closed_with_location() {
    let (_tmp, repo_root, git_dir) = fake_repo_dirs();
    let sid = session_id("malformed");
    let store = SessionStore::open(&repo_root, &git_dir, &sid).expect("open");

    // Manually inject a malformed MID-FILE line (followed by a valid
    // line) — torn-tail recovery (finding 5) only forgives the FINAL
    // line; earlier corruption stays a hard error.
    let reads_path = store
        .baseline_objects_dir()
        .parent()
        .expect("parent")
        .join("reads.jsonl");
    std::fs::write(
        &reads_path,
        b"not-valid-json\n{\"path\":\"x\",\"start_line\":null,\"end_line\":null,\"ts\":\"t\"}\n",
    )
    .expect("write malformed");

    let result = store.reads_since_cursor(0);
    assert!(
        result.is_err(),
        "malformed mid-file JSONL must return error"
    );
    let msg = format!("{:?}", result.unwrap_err());
    // Error must mention the file and a line number.
    assert!(
        msg.contains("reads.jsonl") && (msg.contains("line") || msg.contains("1")),
        "error must include file + line context: {msg}"
    );
}

// ---------------------------------------------------------------------------
// store: linked worktree (different git_dir) yields a different repo-key dir
// ---------------------------------------------------------------------------

#[test]
fn linked_worktree_different_repo_key() {
    use git_mesh::advice::store::repo_key;

    let tmp = tempfile::tempdir().expect("tempdir");
    let repo_root = tmp.path().join("repo");
    let main_git = repo_root.join(".git");
    let linked_git = repo_root.join(".git/worktrees/linked");
    std::fs::create_dir_all(&main_git).expect("main .git");
    std::fs::create_dir_all(&linked_git).expect("linked worktree gitdir");

    let key_main = repo_key(&repo_root, &main_git);
    let key_linked = repo_key(&repo_root, &linked_git);

    assert_ne!(
        key_main, key_linked,
        "linked worktree must produce a different repo-key"
    );
}

// ---------------------------------------------------------------------------
// state: unknown schema_version → error, no silent reinit
// ---------------------------------------------------------------------------

#[test]
fn unknown_schema_version_returns_error() {
    use git_mesh::advice::store::atomic_write;

    let (_tmp, repo_root, git_dir) = fake_repo_dirs();
    let sid = session_id("schema-ver");
    let store = SessionStore::open(&repo_root, &git_dir, &sid).expect("open");

    // Write a state file with an unknown schema_version.
    let baseline_path = store
        .baseline_objects_dir()
        .parent()
        .expect("parent")
        .join("baseline.state");
    let bad_state = br#"{"schema_version":99,"tree_sha":"","index_sha":"","captured_at":""}"#;
    atomic_write(&baseline_path, bad_state).expect("write bad state");

    let result = store.read_baseline();
    assert!(
        result.is_err(),
        "unknown schema_version must return error, not silent reinit"
    );
}

//! Integration tests for the file-backed session store.

mod support;

use git_mesh::advice::{LockTimeout, SessionStore};
use std::path::PathBuf;
use std::time::Duration;
use uuid::Uuid;

fn session_id(label: &str) -> String {
    format!("store-{label}-{}", Uuid::new_v4())
}

fn fake_repo_dirs() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let repo_root = tmp.path().join("repo");
    let git_dir = repo_root.join(".git");
    std::fs::create_dir_all(&git_dir).expect("mkdir .git");
    (tmp, repo_root, git_dir)
}

#[test]
fn open_creates_dir_mode_0700_acquires_lock_releases_on_drop() {
    let (_tmp, repo_root, git_dir) = fake_repo_dirs();
    let sid = session_id("open");

    let store = SessionStore::open(&repo_root, &git_dir, &sid).expect("open should succeed");
    let store_dir = store.dir().to_path_buf();
    assert!(store_dir.exists(), "session dir must exist");

    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let meta = std::fs::metadata(&store_dir).expect("metadata");
        assert_eq!(meta.mode() & 0o777, 0o700, "dir must be mode 0700");
    }

    drop(store);
    let _store2 = SessionStore::open(&repo_root, &git_dir, &sid)
        .expect("second open after drop should succeed");
}

#[test]
fn bounded_lock_timeout_when_held() {
    use git_mesh::advice::store::{acquire_lock, advice_base_dir, repo_key};

    let (_tmp, repo_root, git_dir) = fake_repo_dirs();
    let key = repo_key(&repo_root, &git_dir);
    let base = advice_base_dir();
    let dir = base.join(&key).join("bounded-lock-test");
    std::fs::create_dir_all(&dir).expect("mkdir");

    let _guard = acquire_lock(&dir, LockTimeout::Blocking).expect("first acquire");
    let result = acquire_lock(&dir, LockTimeout::Bounded(Duration::from_millis(30)));
    assert!(
        result.is_err(),
        "bounded acquire must fail when lock is held"
    );
}

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
        acquire_lock(dir_clone.as_ref(), LockTimeout::Blocking)
            .expect("blocking acquire must succeed after guard is dropped")
    });

    std::thread::sleep(Duration::from_millis(50));
    drop(guard);
    handle.join().expect("thread must not panic");
}

#[test]
fn atomic_write_via_tmp_rename() {
    use git_mesh::advice::store::atomic_write;

    let tmp = tempfile::tempdir().expect("tempdir");
    let dest = tmp.path().join("state.json");
    let contents = b"{\"schema_version\":2}";

    atomic_write(&dest, contents).expect("atomic_write");

    let written = std::fs::read(&dest).expect("read dest");
    assert_eq!(written, contents);
    let tmp_sibling = tmp.path().join("state.json.tmp");
    assert!(!tmp_sibling.exists(), ".tmp sibling must be cleaned up");
}

#[test]
fn append_read_round_trip() {
    use git_mesh::advice::state::ReadRecord;

    let (_tmp, repo_root, git_dir) = fake_repo_dirs();
    let sid = session_id("append-read");
    let store = SessionStore::open(&repo_root, &git_dir, &sid).expect("open");

    let rec = ReadRecord {
        path: "src/main.rs".into(),
        start_line: Some(10),
        end_line: Some(20),
        ts: "2026-01-01T00:00:00Z".into(),
        id: Some("tool-42".into()),
    };

    store
        .append_read(&rec, LockTimeout::Blocking)
        .expect("append_read");

    let records = store.all_reads().expect("all_reads");
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].path, "src/main.rs");
    assert_eq!(records[0].id.as_deref(), Some("tool-42"));
}

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

//! Phase 2: skipped contract tests for the SQLite cache.
//!
//! Every test here is `#[ignore]` — they assert the full cache contract
//! against the Phase 1 stub API.  Phase 3 lifts `#[ignore]` one by one as
//! each tier is implemented.

use super::*;
use crate::resolver::session::{CommitDelta, GroupedWalk};
use crate::resolver::walker::NS;
use crate::types::CopyDetection;
use std::process::Command;
use tempfile::tempdir;

// ── Fixture helpers ──────────────────────────────────────────────────────────

fn run_git(dir: &std::path::Path, args: &[&str]) {
    let out = Command::new("git")
        .current_dir(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&out.stderr)
    );
}

fn rev_parse(dir: &std::path::Path, refspec: &str) -> String {
    String::from_utf8(
        Command::new("git")
            .current_dir(dir)
            .args(["rev-parse", refspec])
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap()
    .trim()
    .to_string()
}

/// Minimal git repo with one commit so HEAD is valid.
fn init_repo() -> (tempfile::TempDir, gix::Repository) {
    let td = tempdir().unwrap();
    let dir = td.path();
    run_git(dir, &["init", "--initial-branch=main"]);
    run_git(dir, &["config", "user.email", "t@t"]);
    run_git(dir, &["config", "user.name", "t"]);
    run_git(dir, &["config", "commit.gpgsign", "false"]);
    std::fs::write(dir.join("a.txt"), "hello\n").unwrap();
    run_git(dir, &["add", "."]);
    run_git(dir, &["commit", "-m", "init"]);
    let repo = gix::open(dir).unwrap();
    (td, repo)
}

/// Add a second commit so we have two distinct SHAs.
fn add_commit(dir: &std::path::Path, filename: &str, content: &str) -> String {
    std::fs::write(dir.join(filename), content).unwrap();
    run_git(dir, &["add", "."]);
    run_git(dir, &["commit", "-m", &format!("add {filename}")]);
    rev_parse(dir, "HEAD")
}

fn make_grouped_walk(anchor_sha: &str, head_sha: &str) -> GroupedWalk {
    GroupedWalk {
        anchor_sha: anchor_sha.to_string(),
        head_sha: head_sha.to_string(),
        commits: vec![CommitDelta {
            parent: anchor_sha.to_string(),
            commit: head_sha.to_string(),
            entries: vec![NS::Modified { path: "a.txt".to_string() }],
        }],
        renames_disabled: false,
        closed_paths: None,
    }
}

fn make_grouped_walk_key(anchor_sha: &str, head_sha: &str) -> GroupedWalkKey {
    GroupedWalkKey {
        anchor_sha: anchor_sha.to_string(),
        head_sha: head_sha.to_string(),
        copy_detection: CopyDetection::Off,
        seed_hash: [0u8; 32],
        replace_refs_hash: [0u8; 32],
        git_config_hash: [0u8; 32],
        rename_budget: 200,
    }
}

// ── Tier 1 tests ─────────────────────────────────────────────────────────────

/// Write a `name_status` row through one `Cache` handle, drop it, reopen a
/// fresh handle against the same DB path, and read back identical data.
#[test]
fn name_status_round_trip_persists_across_connections() {
    let (_td, repo) = init_repo();
    let dir = _td.path();
    let head = rev_parse(dir, "HEAD");
    let parent = "0".repeat(40);

    let entries = vec![
        NS::Added { path: "foo.rs".to_string() },
        NS::Deleted { path: "bar.rs".to_string() },
    ];

    // Write through first connection.
    {
        let cache = Cache::open(&repo).expect("open");
        let txn = cache.conn.unchecked_transaction().expect("txn");
        cache
            .name_status_put_batch(
                &txn,
                &[(&parent, &head, CopyDetection::Off, entries.clone())],
            )
            .expect("put_batch");
        txn.commit().expect("commit");
    }

    // Read back through a second connection.
    let cache2 = Cache::open(&repo).expect("reopen");
    let got = cache2
        .name_status_get(&parent, &head, CopyDetection::Off)
        .expect("should hit after round-trip");

    assert_eq!(got.len(), 2, "expected 2 entries");
    match &got[0] {
        NS::Added { path } => assert_eq!(path, "foo.rs"),
        _ => panic!("expected first entry to be NS::Added {{ path: \"foo.rs\" }}"),
    }
    match &got[1] {
        NS::Deleted { path } => assert_eq!(path, "bar.rs"),
        _ => panic!("expected second entry to be NS::Deleted {{ path: \"bar.rs\" }}"),
    }
}

// ── Tier 2 tests ─────────────────────────────────────────────────────────────

/// Write a blob-diff hunk list through one handle, reopen, read back identical
/// tuples.
#[test]
fn blob_diff_round_trip_persists_across_connections() {
    let (_td, repo) = init_repo();

    let old_blob = "a".repeat(40);
    let new_blob = "b".repeat(40);
    let hunks: Vec<(u32, u32, u32, u32)> = vec![(1, 3, 1, 4), (10, 2, 11, 3)];

    // Write.
    {
        let cache = Cache::open(&repo).expect("open");
        let txn = cache.conn.unchecked_transaction().expect("txn");
        cache
            .blob_diff_put(&txn, &old_blob, &new_blob, &hunks)
            .expect("put");
        txn.commit().expect("commit");
    }

    // Read back.
    let cache2 = Cache::open(&repo).expect("reopen");
    let got = cache2
        .blob_diff_get(&old_blob, &new_blob)
        .expect("should hit");

    assert_eq!(got, hunks);
}

// ── Tier 3 tests ─────────────────────────────────────────────────────────────

/// An exact-key hit returns the same `GroupedWalk` that was stored.
#[test]
fn grouped_walk_exact_hit_returns_same_walk() {
    let (_td, repo) = init_repo();
    let dir = _td.path();
    let anchor = rev_parse(dir, "HEAD");
    let head = add_commit(dir, "b.txt", "world\n");

    let walk = make_grouped_walk(&anchor, &head);
    let key = make_grouped_walk_key(&anchor, &head);

    // Write.
    {
        let cache = Cache::open(&repo).expect("open");
        let txn = cache.conn.unchecked_transaction().expect("txn");
        cache
            .grouped_walk_replace(&txn, None, &key, &walk)
            .expect("replace");
        txn.commit().expect("commit");
    }

    // Exact-hit read.
    let cache2 = Cache::open(&repo).expect("reopen");
    let got = cache2
        .grouped_walk_get_exact(&key)
        .expect("exact hit expected");

    assert_eq!(got.anchor_sha, anchor);
    assert_eq!(got.head_sha, head);
    assert_eq!(got.commits.len(), 1);
}

/// An ancestor-hit: store a walk at `head_v1`, then query with `head_v2`
/// which is a descendant of `head_v1`. The cache returns the cached head
/// and the walk.
#[test]
fn grouped_walk_ancestor_hit_returns_cached_head_and_walk() {
    let (_td, repo) = init_repo();
    let dir = _td.path();
    let anchor = rev_parse(dir, "HEAD");
    let head_v1 = add_commit(dir, "b.txt", "world\n");
    let head_v2 = add_commit(dir, "c.txt", "again\n");

    let walk_v1 = make_grouped_walk(&anchor, &head_v1);
    let key_v1 = make_grouped_walk_key(&anchor, &head_v1);

    // Store walk at head_v1.
    {
        let cache = Cache::open(&repo).expect("open");
        let txn = cache.conn.unchecked_transaction().expect("txn");
        cache
            .grouped_walk_replace(&txn, None, &key_v1, &walk_v1)
            .expect("replace");
        txn.commit().expect("commit");
    }

    // Query with head_v2 (a descendant). Should get ancestor hit.
    let cache2 = Cache::open(&repo).expect("reopen");
    let result = cache2.grouped_walk_get_ancestor(
        &anchor,
        CopyDetection::Off,
        key_v1.seed_hash.as_ref(),
        key_v1.replace_refs_hash.as_ref(),
        key_v1.git_config_hash.as_ref(),
        key_v1.rename_budget as i64,
        &head_v2,
        &repo,
    );

    let (cached_head, cached_walk) = result.expect("ancestor hit expected");
    assert_eq!(cached_head, head_v1, "cached head should be the stored head");
    assert_eq!(cached_walk.anchor_sha, anchor);
}

/// `grouped_walk_replace` with `old_head` set deletes the old row and
/// inserts the new one atomically.
#[test]
fn grouped_walk_replace_evicts_old_head_in_one_txn() {
    let (_td, repo) = init_repo();
    let dir = _td.path();
    let anchor = rev_parse(dir, "HEAD");
    let head_v1 = add_commit(dir, "b.txt", "v1\n");
    let head_v2 = add_commit(dir, "c.txt", "v2\n");

    let walk_v1 = make_grouped_walk(&anchor, &head_v1);
    let walk_v2 = make_grouped_walk(&anchor, &head_v2);
    let key_v1 = make_grouped_walk_key(&anchor, &head_v1);
    let key_v2 = make_grouped_walk_key(&anchor, &head_v2);

    // Store v1.
    {
        let cache = Cache::open(&repo).expect("open");
        let txn = cache.conn.unchecked_transaction().expect("txn");
        cache
            .grouped_walk_replace(&txn, None, &key_v1, &walk_v1)
            .expect("replace v1");
        txn.commit().expect("commit");
    }

    // Replace v1 with v2 — old_head points at v1.
    {
        let cache = Cache::open(&repo).expect("open");
        let txn = cache.conn.unchecked_transaction().expect("txn");
        cache
            .grouped_walk_replace(&txn, Some(&head_v1), &key_v2, &walk_v2)
            .expect("replace v2");
        txn.commit().expect("commit");
    }

    // v1 must be gone; v2 must be present.
    let cache3 = Cache::open(&repo).expect("reopen");
    assert!(
        cache3.grouped_walk_get_exact(&key_v1).is_none(),
        "old row must be evicted"
    );
    let got = cache3
        .grouped_walk_get_exact(&key_v2)
        .expect("new row must exist");
    assert_eq!(got.head_sha, head_v2);
}

// ── Schema / version tests ────────────────────────────────────────────────────

/// Manually corrupt `user_version` in an existing DB, reopen via `Cache::open`,
/// and assert that the tables are freshly empty (schema was dropped and rebuilt).
#[test]
fn version_mismatch_drops_and_rebuilds() {
    let (_td, repo) = init_repo();
    let dir = _td.path();
    let anchor = rev_parse(dir, "HEAD");
    let head = anchor.clone();

    // Write a row so we know the DB has data.
    let db_path = {
        let cache = Cache::open(&repo).expect("open");
        // Insert a name_status row.
        let txn = cache.conn.unchecked_transaction().expect("txn");
        cache
            .name_status_put_batch(
                &txn,
                &[(&anchor, &head, CopyDetection::Off, vec![NS::Added { path: "x.rs".to_string() }])],
            )
            .expect("put_batch");
        txn.commit().expect("commit");

        // Derive the DB path the same way Cache::open does.
        let git_dir = repo.git_dir().to_owned();
        git_dir.join("mesh").join("cache").join("mesh_cache.sqlite")
    };

    // Corrupt user_version using a raw rusqlite connection.
    {
        let conn = rusqlite::Connection::open(&db_path).expect("raw open");
        conn.execute_batch("PRAGMA user_version = 999;")
            .expect("set bad version");
    }

    // Reopen via Cache — must silently rebuild.
    let cache2 = Cache::open(&repo).expect("reopen after version mismatch");

    // The table must exist (schema was rebuilt) but be empty (old data dropped).
    let count: i64 = cache2
        .conn
        .query_row("SELECT COUNT(*) FROM name_status_cache", [], |r| r.get(0))
        .expect("query count");
    assert_eq!(count, 0, "table should be empty after schema rebuild");

    // user_version must now be SCHEMA_VERSION.
    let ver: i32 = cache2
        .conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .expect("read version");
    assert_eq!(ver, SCHEMA_VERSION);
}

// ── GC test ───────────────────────────────────────────────────────────────────

/// GC removes rows whose SHAs are unreachable, and leaves reachable rows alone.
///
/// We can't easily manufacture an unreachable SHA without doing actual git
/// object manipulation, so this test uses a fake SHA that will never appear
/// in `git rev-list --all --objects` output and asserts it gets swept.
#[test]
#[ignore]
fn gc_drops_unreachable_rows_only() {
    let (_td, repo) = init_repo();
    let dir = _td.path();
    let real_head = rev_parse(dir, "HEAD");
    let ghost_parent = "dead".repeat(10); // 40-char fake SHA
    let ghost_commit = "cafe".repeat(10); // 40-char fake SHA

    // Insert one reachable row and one unreachable row.
    {
        let cache = Cache::open(&repo).expect("open");
        let txn = cache.conn.unchecked_transaction().expect("txn");
        // Reachable: uses the real HEAD SHA for both parent and commit.
        cache
            .name_status_put_batch(
                &txn,
                &[
                    (
                        &real_head,
                        &real_head,
                        CopyDetection::Off,
                        vec![NS::Modified { path: "a.txt".to_string() }],
                    ),
                    (
                        &ghost_parent,
                        &ghost_commit,
                        CopyDetection::Off,
                        vec![NS::Added { path: "ghost.rs".to_string() }],
                    ),
                ],
            )
            .expect("put_batch");
        txn.commit().expect("commit");
    }

    // Run GC.
    {
        let cache = Cache::open(&repo).expect("open for gc");
        let stats = cache.gc(&repo).expect("gc");
        assert!(
            stats.name_status_removed >= 1,
            "expected at least one unreachable row removed, got {}",
            stats.name_status_removed
        );
    }

    // Ghost row must be gone; reachable row must remain.
    let cache3 = Cache::open(&repo).expect("reopen");
    assert!(
        cache3
            .name_status_get(&ghost_parent, &ghost_commit, CopyDetection::Off)
            .is_none(),
        "unreachable row must be gone after GC"
    );
    assert!(
        cache3
            .name_status_get(&real_head, &real_head, CopyDetection::Off)
            .is_some(),
        "reachable row must survive GC"
    );
}

// ── Env-var disable test ──────────────────────────────────────────────────────

/// With `GIT_MESH_CACHE=0`, puts are silently skipped.  After re-enabling the
/// env var, the row must be absent (confirming the put was a no-op).
#[test]
fn cache_disabled_env_var_skips_reads_and_writes() {
    let (_td, repo) = init_repo();
    let dir = _td.path();
    let head = rev_parse(dir, "HEAD");
    let parent = "1".repeat(40);

    // Write with cache disabled.
    {
        // SAFETY: test process is single-threaded at this point; no other
        // threads read GIT_MESH_CACHE concurrently.
        #[allow(unused_unsafe)]
        unsafe {
            std::env::set_var("GIT_MESH_CACHE", "0");
        }
        let cache = Cache::open(&repo).expect("open disabled");
        let txn = cache.conn.unchecked_transaction().expect("txn");
        let result = cache.name_status_put_batch(
            &txn,
            &[(&parent, &head, CopyDetection::Off, vec![NS::Added { path: "z.rs".to_string() }])],
        );
        txn.commit().expect("commit (should be no-op)");
        #[allow(unused_unsafe)]
        unsafe {
            std::env::remove_var("GIT_MESH_CACHE");
        }
        result.expect("put should not error even when disabled");
    }

    // Re-enable and confirm the row is absent.
    let cache2 = Cache::open(&repo).expect("reopen enabled");
    assert!(
        cache2
            .name_status_get(&parent, &head, CopyDetection::Off)
            .is_none(),
        "row must be absent: put was a no-op when cache was disabled"
    );
}

// ── Concurrency test ──────────────────────────────────────────────────────────

/// Two threads each open their own `Cache` against the same DB and insert
/// non-overlapping rows.  Both must succeed and both rows must be visible
/// from a third reader.
///
/// WAL + 500 ms busy_timeout serializes concurrent writers without
/// application-level retry.
#[test]
#[ignore]
fn concurrent_writers_serialize_within_busy_timeout() {
    let (_td, repo) = init_repo();
    let dir = _td.path();
    let head = rev_parse(dir, "HEAD");

    // We need the DB path for thread-local opens; derive it the same way Cache::open does.
    let db_path = {
        let git_dir = repo.git_dir().to_owned();
        git_dir.join("mesh").join("cache").join("mesh_cache.sqlite")
    };

    // Ensure the DB and schema exist before spawning threads.
    let _ = Cache::open(&repo).expect("bootstrap");
    drop(repo);

    let head_clone = head.clone();
    let db_path_clone = db_path.clone();

    let parent_a = "aaaa".repeat(10);
    let parent_b = "bbbb".repeat(10);
    let commit_a = "cccc".repeat(10);
    let commit_b = "dddd".repeat(10);

    let pa = parent_a.clone();
    let ca = commit_a.clone();
    let pb = parent_b.clone();
    let cb = commit_b.clone();
    let h1 = head.clone();
    let h2 = head_clone.clone();

    let flags = rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE | rusqlite::OpenFlags::SQLITE_OPEN_FULL_MUTEX;

    let thread_a = std::thread::spawn(move || {
        let conn = rusqlite::Connection::open_with_flags(&db_path, flags).expect("open a");
        conn.execute_batch("PRAGMA busy_timeout = 500;").expect("pragma a");
        conn.execute_batch(&format!(
            "INSERT OR REPLACE INTO name_status_cache (parent_sha, commit_sha, copy_detection, entries_blob) \
             VALUES ('{}', '{}', 0, X'0000');",
            pa, ca
        ))
        .expect("insert a");
        h1
    });

    let thread_b = std::thread::spawn(move || {
        let conn = rusqlite::Connection::open_with_flags(&db_path_clone, flags).expect("open b");
        conn.execute_batch("PRAGMA busy_timeout = 500;").expect("pragma b");
        conn.execute_batch(&format!(
            "INSERT OR REPLACE INTO name_status_cache (parent_sha, commit_sha, copy_detection, entries_blob) \
             VALUES ('{}', '{}', 0, X'0000');",
            pb, cb
        ))
        .expect("insert b");
        h2
    });

    thread_a.join().expect("thread a panicked");
    thread_b.join().expect("thread b panicked");

    // Both rows must be visible from a fresh reader.
    let repo2 = gix::open(_td.path()).expect("reopen repo");
    let cache3 = Cache::open(&repo2).expect("reader");

    // We inserted raw blobs (X'0000') so name_status_get's bincode decode may
    // fail — just check row existence via raw SQL.
    let count: i64 = cache3
        .conn
        .query_row(
            "SELECT COUNT(*) FROM name_status_cache WHERE parent_sha IN (?, ?)",
            rusqlite::params![parent_a, parent_b],
            |r| r.get(0),
        )
        .expect("count query");
    assert_eq!(count, 2, "both rows must be visible after concurrent inserts");
}

// ── Performance SLA stub ──────────────────────────────────────────────────────

/// Documents the ≤40 ms warm-run SLA.  Phase 3 promotes this to a real
/// criterion benchmark in `benches/stale_warm.rs`; this stub just asserts
/// the scaffolding compiles.
///
/// A real timing assertion requires a fixture repo with ≥9 meshes / ≥22
/// anchors and two sequential `git mesh stale` invocations.  That setup
/// belongs in the bench harness, not here.
#[test]
#[ignore]
fn stale_warm_run_returns_under_40ms_on_fixture() {
    // SLA: a second `git mesh stale` run at the same HEAD must complete in
    // ≤ 40 ms (mean over multiple iterations in the criterion bench).
    //
    // This test is a compile-time placeholder.  When Phase 3 step 8 ships
    // `benches/stale_warm.rs`, this test should be deleted and the bench
    // becomes the authoritative SLA check.
    //
    // Minimal assertion so the body isn't empty:
    let (_td, repo) = init_repo();
    let cache = Cache::open(&repo).expect("open");
    let start = std::time::Instant::now();
    // Cold open with no data — serves as a lower-bound sanity check only.
    let _ = cache.name_status_get("0".repeat(40).as_str(), "1".repeat(40).as_str(), CopyDetection::Off);
    let elapsed = start.elapsed();
    assert!(
        elapsed.as_millis() < 40,
        "even an empty probe should be well under 40 ms; got {:?}",
        elapsed
    );
}

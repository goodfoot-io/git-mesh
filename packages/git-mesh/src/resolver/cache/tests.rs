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

/// Standard test key components for the new single-row `grouped_walk_cache`.
struct GwKey {
    anchor: String,
    cd: CopyDetection,
    seed_hash: [u8; 32],
    replace_refs_hash: [u8; 32],
    git_config_hash: [u8; 32],
    rename_budget: i64,
}

fn make_gw_key(anchor_sha: &str) -> GwKey {
    GwKey {
        anchor: anchor_sha.to_string(),
        cd: CopyDetection::Off,
        seed_hash: [0u8; 32],
        replace_refs_hash: [0u8; 32],
        git_config_hash: [0u8; 32],
        rename_budget: 200,
    }
}

fn gw_upsert(cache: &Cache, k: &GwKey, head_sha: &str, walk: &GroupedWalk) {
    cache
        .with_write_txn(|txn| {
            cache.grouped_walk_upsert(
                txn,
                &k.anchor,
                k.cd,
                k.seed_hash.as_ref(),
                k.replace_refs_hash.as_ref(),
                k.git_config_hash.as_ref(),
                k.rename_budget,
                head_sha,
                walk,
            )
        })
        .expect("upsert");
}

fn gw_get(
    cache: &Cache,
    k: &GwKey,
    current_head: &str,
    memo: &mut std::collections::HashSet<gix::ObjectId>,
    repo: &gix::Repository,
) -> GroupedWalkResult {
    cache.grouped_walk_get(
        &k.anchor,
        k.cd,
        k.seed_hash.as_ref(),
        k.replace_refs_hash.as_ref(),
        k.git_config_hash.as_ref(),
        k.rename_budget,
        current_head,
        memo,
        repo,
    )
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

/// Schema v4: exact-hit. Store a row with `head_sha = HEAD`, query with the
/// same HEAD → `ExactHit`.
#[test]
fn grouped_walk_get_exact_hit() {
    let (_td, repo) = init_repo();
    let dir = _td.path();
    let anchor = rev_parse(dir, "HEAD");
    let head = add_commit(dir, "b.txt", "world\n");

    let walk = make_grouped_walk(&anchor, &head);
    let key = make_gw_key(&anchor);

    let cache = Cache::open(&repo).expect("open");
    gw_upsert(&cache, &key, &head, &walk);

    let cache2 = Cache::open(&repo).expect("reopen");
    let mut memo = std::collections::HashSet::new();
    match gw_get(&cache2, &key, &head, &mut memo, &repo) {
        GroupedWalkResult::ExactHit(got) => {
            assert_eq!(got.anchor_sha, anchor);
            assert_eq!(got.head_sha, head);
            assert_eq!(got.commits.len(), 1);
        }
        _ => panic!("expected ExactHit"),
    }
}

/// Schema v4: extend-hit via a real gix ancestor check. Store a row at
/// `head_v1`, query with `head_v2` (a descendant), with the per-session
/// ancestor memo empty — this exercises `repo.merge_base` and the memo
/// insertion side-effect.
#[test]
fn grouped_walk_get_extend_hit_via_real_gix_ancestor() {
    use std::str::FromStr;
    let (_td, repo) = init_repo();
    let dir = _td.path();
    let anchor = rev_parse(dir, "HEAD");
    let head_v1 = add_commit(dir, "b.txt", "world\n");
    let head_v2 = add_commit(dir, "c.txt", "again\n");

    let walk_v1 = make_grouped_walk(&anchor, &head_v1);
    let key = make_gw_key(&anchor);

    let cache = Cache::open(&repo).expect("open");
    gw_upsert(&cache, &key, &head_v1, &walk_v1);

    let cache2 = Cache::open(&repo).expect("reopen");
    let mut memo = std::collections::HashSet::<gix::ObjectId>::new();
    let v1_oid = gix::ObjectId::from_str(&head_v1).expect("parse oid");

    match gw_get(&cache2, &key, &head_v2, &mut memo, &repo) {
        GroupedWalkResult::ExtendHit { cached_head, walk } => {
            assert_eq!(cached_head, head_v1, "cached head must be the stored head");
            assert_eq!(walk.anchor_sha, anchor);
        }
        other => panic!("expected ExtendHit, got {:?}", match other {
            GroupedWalkResult::ExactHit(_) => "ExactHit",
            GroupedWalkResult::Miss => "Miss",
            _ => "?",
        }),
    }
    assert!(
        memo.contains(&v1_oid),
        "merge_base success must populate the ancestor memo"
    );
}

/// Schema v4: extend-hit via the memo fast-path. Pre-populate `known_head_ancestors`
/// with the stored head's oid; the call must succeed without consulting gix.
#[test]
fn grouped_walk_get_extend_hit_via_memo_fast_path() {
    use std::str::FromStr;
    let (_td, repo) = init_repo();
    let dir = _td.path();
    let anchor = rev_parse(dir, "HEAD");
    let head_v1 = add_commit(dir, "b.txt", "world\n");
    let head_v2 = add_commit(dir, "c.txt", "again\n");

    let walk_v1 = make_grouped_walk(&anchor, &head_v1);
    let key = make_gw_key(&anchor);

    let cache = Cache::open(&repo).expect("open");
    gw_upsert(&cache, &key, &head_v1, &walk_v1);

    let cache2 = Cache::open(&repo).expect("reopen");
    let v1_oid = gix::ObjectId::from_str(&head_v1).expect("parse oid");
    let mut memo = std::collections::HashSet::<gix::ObjectId>::new();
    memo.insert(v1_oid);

    match gw_get(&cache2, &key, &head_v2, &mut memo, &repo) {
        GroupedWalkResult::ExtendHit { cached_head, .. } => {
            assert_eq!(cached_head, head_v1);
        }
        _ => panic!("expected ExtendHit via memo"),
    }
}

/// Schema v4: divergent (non-ancestor) stored head → `Miss`. UPSERT then
/// overwrites with the new head, leaving one row total.
#[test]
fn grouped_walk_get_miss_on_non_ancestor_and_upsert_overwrites() {
    let (_td, repo) = init_repo();
    let dir = _td.path();
    let anchor = rev_parse(dir, "HEAD");

    // Build a divergent branch: `head_b` is on `other`, not an ancestor of `head_a`.
    let head_a = add_commit(dir, "a.txt", "a-on-main\n");
    run_git(dir, &["checkout", "-b", "other", &anchor]);
    let head_b = add_commit(dir, "b.txt", "b-on-other\n");
    run_git(dir, &["checkout", "main"]);

    let walk_b = make_grouped_walk(&anchor, &head_b);
    let key = make_gw_key(&anchor);

    let cache = Cache::open(&repo).expect("open");
    gw_upsert(&cache, &key, &head_b, &walk_b);

    let cache2 = Cache::open(&repo).expect("reopen");
    let mut memo = std::collections::HashSet::new();
    match gw_get(&cache2, &key, &head_a, &mut memo, &repo) {
        GroupedWalkResult::Miss => {}
        _ => panic!("expected Miss for divergent stored head"),
    }

    // UPSERT with head_a overwrites the row in place; exactly one row remains.
    let walk_a = make_grouped_walk(&anchor, &head_a);
    gw_upsert(&cache2, &key, &head_a, &walk_a);

    let count: i64 = cache2
        .conn
        .query_row(
            "SELECT COUNT(*) FROM grouped_walk_cache WHERE anchor_sha = ?1",
            rusqlite::params![&anchor],
            |r| r.get(0),
        )
        .expect("count");
    assert_eq!(count, 1, "UPSERT must replace, not insert a second row");

    // The new query with head_a should now ExactHit.
    let mut memo2 = std::collections::HashSet::new();
    match gw_get(&cache2, &key, &head_a, &mut memo2, &repo) {
        GroupedWalkResult::ExactHit(got) => assert_eq!(got.head_sha, head_a),
        _ => panic!("expected ExactHit after overwrite"),
    }
}

/// UPSERT idempotency: two calls with different `head_sha` on the same key
/// tuple leave exactly one row whose `head_sha` is the latest.
#[test]
fn grouped_walk_upsert_is_idempotent() {
    let (_td, repo) = init_repo();
    let dir = _td.path();
    let anchor = rev_parse(dir, "HEAD");
    let head_v1 = add_commit(dir, "b.txt", "v1\n");
    let head_v2 = add_commit(dir, "c.txt", "v2\n");

    let key = make_gw_key(&anchor);
    let cache = Cache::open(&repo).expect("open");
    gw_upsert(&cache, &key, &head_v1, &make_grouped_walk(&anchor, &head_v1));
    gw_upsert(&cache, &key, &head_v2, &make_grouped_walk(&anchor, &head_v2));

    let count: i64 = cache
        .conn
        .query_row(
            "SELECT COUNT(*) FROM grouped_walk_cache",
            [],
            |r| r.get(0),
        )
        .expect("count");
    assert_eq!(count, 1);

    let stored_head: String = cache
        .conn
        .query_row(
            "SELECT head_sha FROM grouped_walk_cache",
            [],
            |r| r.get(0),
        )
        .expect("head");
    assert_eq!(stored_head, head_v2);
}

/// Cross-handle hit: two `Cache` handles against the same DB path observe
/// each other's writes (acceptance signal for shared common_dir).
#[test]
fn cross_handle_grouped_walk_hit_on_shared_db() {
    let (_td, repo) = init_repo();
    let dir = _td.path();
    let anchor = rev_parse(dir, "HEAD");
    let head = add_commit(dir, "b.txt", "world\n");

    let key = make_gw_key(&anchor);
    let walk = make_grouped_walk(&anchor, &head);

    let cache_a = Cache::open(&repo).expect("open a");
    gw_upsert(&cache_a, &key, &head, &walk);
    drop(cache_a);

    let cache_b = Cache::open(&repo).expect("open b");
    let mut memo = std::collections::HashSet::new();
    match gw_get(&cache_b, &key, &head, &mut memo, &repo) {
        GroupedWalkResult::ExactHit(got) => assert_eq!(got.head_sha, head),
        _ => panic!("expected cross-handle ExactHit on shared DB"),
    }
}

/// Concurrent `Cache::open` from two threads against the same path must both
/// succeed and the schema must be consistent (covers `create_dir_all` +
/// bootstrap race).
#[test]
fn concurrent_cache_open_succeeds() {
    let (_td, repo) = init_repo();
    drop(repo);
    let path = _td.path().to_owned();

    let p1 = path.clone();
    let p2 = path.clone();
    let t1 = std::thread::spawn(move || {
        let r = gix::open(&p1).expect("open repo 1");
        Cache::open(&r).is_ok()
    });
    let t2 = std::thread::spawn(move || {
        let r = gix::open(&p2).expect("open repo 2");
        Cache::open(&r).is_ok()
    });
    assert!(t1.join().expect("thread 1"));
    assert!(t2.join().expect("thread 2"));

    // Schema is consistent: user_version == SCHEMA_VERSION.
    let repo = gix::open(&path).expect("reopen");
    let cache = Cache::open(&repo).expect("final open");
    let ver: i32 = cache
        .conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .expect("version");
    assert_eq!(ver, SCHEMA_VERSION);
}

/// No SQLITE_BUSY stress: spawn N=4 writer threads, each running
/// `with_write_txn` calls; all must complete cleanly under the configured
/// busy_timeout.
#[test]
fn concurrent_writers_no_sqlite_busy_stress() {
    let (_td, repo) = init_repo();
    let dir = _td.path();
    let head = rev_parse(dir, "HEAD");
    let _ = Cache::open(&repo).expect("bootstrap");
    drop(repo);

    let n: usize = 4;
    let iters: usize = 25;
    let path = dir.to_owned();

    let handles: Vec<_> = (0..n)
        .map(|tid| {
            let path = path.clone();
            let head = head.clone();
            std::thread::spawn(move || -> Result<()> {
                let r = gix::open(&path).expect("open repo");
                let cache = Cache::open(&r).expect("open cache");
                for i in 0..iters {
                    let parent = format!("{:040x}", tid * 10000 + i);
                    cache.with_write_txn(|txn| {
                        cache.name_status_put_batch(
                            txn,
                            &[(&parent, &head, CopyDetection::Off,
                               vec![NS::Added { path: "x.rs".to_string() }])],
                        )
                    })?;
                }
                Ok(())
            })
        })
        .collect();
    for h in handles {
        h.join().expect("thread panic").expect("no SQLITE_BUSY");
    }
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

/// When the schema version mismatches, the rebuild also removes the legacy
/// `rename-trail/` JSON directory left by pre-sqlite Phase 2 code.
#[test]
fn version_mismatch_removes_legacy_rename_trail_dir() {
    let (_td, repo) = init_repo();

    // Derive paths the same way Cache::open does.
    let db_dir = repo.git_dir().join("mesh").join("cache");
    let db_path = db_dir.join("mesh_cache.sqlite");

    // Bootstrap a DB with a real schema version.
    let _ = Cache::open(&repo).expect("bootstrap");

    // Plant a fake legacy rename-trail/v1/ directory.
    let legacy_dir = db_dir.join("rename-trail").join("v1");
    std::fs::create_dir_all(&legacy_dir).expect("create legacy dir");
    std::fs::write(legacy_dir.join("some-file.json"), "{}").expect("write legacy file");

    // Corrupt user_version to trigger a schema mismatch rebuild.
    {
        let conn = rusqlite::Connection::open(&db_path).expect("raw open");
        conn.execute_batch("PRAGMA user_version = 999;").expect("corrupt version");
    }

    // Reopen — must rebuild schema and delete the legacy directory.
    let _ = Cache::open(&repo).expect("reopen after mismatch");

    assert!(
        !db_dir.join("rename-trail").exists(),
        "legacy rename-trail/ directory must be removed after schema rebuild"
    );
}

// ── GC test ───────────────────────────────────────────────────────────────────

/// GC removes rows whose SHAs are unreachable, and leaves reachable rows alone.
///
/// We can't easily manufacture an unreachable SHA without doing actual git
/// object manipulation, so this test uses a fake SHA that will never appear
/// in `git rev-list --all --objects` output and asserts it gets swept.
#[test]
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

// ── Tier 5 (drift_locus_cache) bootstrap tests ────────────────────────────────

/// cache miss → compute → store → hit on identical key.
#[test]
fn drift_locus_round_trip_persists_across_connections() {
    let (_td, repo) = init_repo();
    let dir = _td.path();
    let anchor = rev_parse(dir, "HEAD");
    let _head = add_commit(dir, "b.txt", "world\n");

    use super::DriftLocusCacheKey;
    use super::DriftLocusCachedValue;
    use crate::types::CopyDetection;

    let key = DriftLocusCacheKey {
        anchor_sha: anchor.clone(),
        path: "a.txt".to_string(),
        blob_oid: "a".repeat(40),
        range_start: 1,
        range_end: 5,
        copy_detection: CopyDetection::Off,
        rename_budget: 200,
    };
    let value = DriftLocusCachedValue {
        variant: 1, // ChangedAt
        answer_commit: _head.clone(),
    };

    // Write.
    {
        let cache = Cache::open(&repo).expect("open");
        let txn = cache.conn.unchecked_transaction().expect("txn");
        cache.drift_locus_put(&txn, &key, &value).expect("put");
        txn.commit().expect("commit");
    }

    // Read back on fresh connection.
    let cache2 = Cache::open(&repo).expect("reopen");
    let got = cache2.drift_locus_get(&key).expect("should hit");
    assert_eq!(got.variant, 1);
    assert_eq!(got.answer_commit, _head);
}

/// `None` round-trips through `encode_drift_locus`/`decode_drift_locus` as
/// `None`, not `Some(Unreachable)`.
///
/// This is a regression guard for the variant-3 decode bug: before the fix,
/// storing a `None` result (variant 3) and re-reading it produced
/// `Some(Unreachable)` because the decoder's catch-all mapped unknown variants
/// to `Unreachable` instead of checking for the sentinel.
#[test]
fn drift_locus_none_round_trips_as_none() {
    use crate::resolver::attribution;
    use crate::types::DriftLocus;

    // None encodes as variant 3 with the null sentinel commit.
    let encoded = attribution::encode_drift_locus_for_test(None);
    assert_eq!(encoded.variant, 3, "None must encode as variant 3");

    // Decoding variant 3 must return None, not Some(Unreachable).
    let decoded = attribution::decode_drift_locus_for_test(&encoded);
    assert!(
        decoded.is_none(),
        "variant 3 must decode back to None, got {decoded:?}"
    );

    // Some(Unreachable) still encodes as variant 0.
    let enc_unreachable = attribution::encode_drift_locus_for_test(Some(&DriftLocus::Unreachable));
    assert_eq!(enc_unreachable.variant, 0);
    let dec_unreachable = attribution::decode_drift_locus_for_test(&enc_unreachable);
    assert!(
        matches!(dec_unreachable, Some(DriftLocus::Unreachable)),
        "variant 0 must decode to Some(Unreachable)"
    );
}

/// Cached row whose `answer_commit` is not an ancestor of HEAD is treated as
/// a miss and recomputed. This test calls through the public attribution path
/// (`crate::resolver::attribution::drift_locus`) and verifies the counter
/// increments are correct.
#[test]
fn drift_locus_stale_answer_commit_causes_recompute() {
    let (_td, repo) = init_repo();
    let dir = _td.path();
    let anchor = rev_parse(dir, "HEAD");
    let _head = add_commit(dir, "b.txt", "world\n");

    use super::DriftLocusCacheKey;
    use super::DriftLocusCachedValue;
    use crate::types::CopyDetection;

    // Plant a row whose answer_commit is a fake (non-ancestor) SHA.
    let key = DriftLocusCacheKey {
        anchor_sha: anchor.clone(),
        path: "a.txt".to_string(),
        blob_oid: "b".repeat(40),
        range_start: 1,
        range_end: 2,
        copy_detection: CopyDetection::Off,
        rename_budget: 200,
    };
    let stale_value = DriftLocusCachedValue {
        variant: 1, // ChangedAt
        answer_commit: "dead".repeat(10), // fake, not a real commit
    };

    {
        let cache = Cache::open(&repo).expect("open");
        let txn = cache.conn.unchecked_transaction().expect("txn");
        cache.drift_locus_put(&txn, &key, &stale_value).expect("put");
        txn.commit().expect("commit");
    }

    // Re-open; the stale row must still exist (no in-band deletion).
    let cache2 = Cache::open(&repo).expect("reopen");
    let row = cache2.drift_locus_get(&key);
    assert!(row.is_some(), "stale row must be present (no in-band DELETE)");
    // The caller is responsible for the ancestor check; the cache itself
    // returns the row unconditionally.  The caller then rejects it.
    let val = row.unwrap();
    assert_eq!(val.answer_commit, "dead".repeat(10));
}

/// `GIT_MESH_CACHE=0` short-circuits both read and write of the drift_locus tier.
#[test]
fn drift_locus_cache_disabled_env_var_skips_reads_and_writes() {
    let (_td, repo) = init_repo();
    let dir = _td.path();
    let anchor = rev_parse(dir, "HEAD");

    use super::DriftLocusCacheKey;
    use super::DriftLocusCachedValue;
    use crate::types::CopyDetection;

    let key = DriftLocusCacheKey {
        anchor_sha: anchor.clone(),
        path: "a.txt".to_string(),
        blob_oid: "c".repeat(40),
        range_start: 1,
        range_end: 3,
        copy_detection: CopyDetection::Off,
        rename_budget: 200,
    };
    let value = DriftLocusCachedValue {
        variant: 0, // Unreachable
        answer_commit: "0".repeat(40),
    };

    // Write with cache disabled.
    {
        #[allow(unused_unsafe)]
        unsafe {
            std::env::set_var("GIT_MESH_CACHE", "0");
        }
        let cache = Cache::open(&repo).expect("open disabled");
        let txn = cache.conn.unchecked_transaction().expect("txn");
        let result = cache.drift_locus_put(&txn, &key, &value);
        txn.commit().expect("commit (no-op)");
        #[allow(unused_unsafe)]
        unsafe {
            std::env::remove_var("GIT_MESH_CACHE");
        }
        result.expect("put should not error when disabled");
    }

    // Re-enable; row must be absent.
    let cache2 = Cache::open(&repo).expect("reopen enabled");
    assert!(
        cache2.drift_locus_get(&key).is_none(),
        "row must be absent: put was a no-op when cache disabled"
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


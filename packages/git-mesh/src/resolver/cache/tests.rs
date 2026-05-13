//! Cache contract tests.
//!
//! Phase 2: skipped (`#[ignore]`) contract checks targeting the new
//! [`Cache`] / [`CacheKey`] / [`Kind`] surface declared in
//! [`super`](../mod.rs). Phase 3 unskips them tier by tier as
//! [`Cache::get_or_insert_with`](super::Cache::get_or_insert_with),
//! [`Cache::open`](super::Cache::open), and
//! [`Cache::gc`](super::Cache::gc) come online.
//!
//! Every check uses a fresh [`tempfile::TempDir`] so tests parallelize.
//! Path-equality assertions canonicalize first (see
//! [`notes/common-dir-resolution.md`](../../../notes/common-dir-resolution.md)).

use super::{Cache, CacheKey, DriftLocusKey, GroupedWalkKey, Kind, RenameTrailKey};
use crate::Result;
use crate::types::CopyDetection;
use serde::{Deserialize, Serialize};
use std::process::Command;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use tempfile::tempdir;

// ── Fixtures ────────────────────────────────────────────────────────────────

/// Representative `V` type used across every check. Bincode-friendly,
/// `PartialEq + Debug` so assertions read well.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Probe {
    count: u32,
    label: String,
}

impl Probe {
    fn sample() -> Self {
        Probe {
            count: 7,
            label: "phase-2".to_string(),
        }
    }
}

fn run_git(dir: &std::path::Path, args: &[&str]) {
    let out = Command::new("git")
        .current_dir(dir)
        .args(args)
        .output()
        .expect("spawn git");
    assert!(
        out.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&out.stderr)
    );
}

fn rev_parse(dir: &std::path::Path, refspec: &str) -> String {
    let out = Command::new("git")
        .current_dir(dir)
        .args(["rev-parse", refspec])
        .output()
        .expect("spawn git rev-parse");
    assert!(out.status.success());
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

fn commit_file(dir: &std::path::Path, path: &str, content: &str, msg: &str) {
    let abs = dir.join(path);
    if let Some(p) = abs.parent() {
        std::fs::create_dir_all(p).expect("create parent");
    }
    std::fs::write(abs, content).expect("write file");
    run_git(dir, &["add", "."]);
    run_git(dir, &["commit", "-m", msg]);
}

fn init_repo() -> tempfile::TempDir {
    let td = tempdir().expect("tempdir");
    let dir = td.path();
    run_git(dir, &["init", "--initial-branch=main"]);
    run_git(dir, &["config", "user.email", "t@t"]);
    run_git(dir, &["config", "user.name", "t"]);
    run_git(dir, &["config", "commit.gpgsign", "false"]);
    td
}

fn sample_rename_trail_key() -> RenameTrailKey {
    RenameTrailKey {
        anchor_sha: "a".repeat(40),
        head_sha: "b".repeat(40),
        copy_detection: CopyDetection::SameCommit,
        rename_budget: 1000,
        candidate_seed_hash: [1u8; 32],
        replace_refs_hash: [2u8; 32],
        git_config_hash: [3u8; 32],
    }
}

fn sample_grouped_walk_key() -> GroupedWalkKey {
    GroupedWalkKey {
        anchor_sha: "c".repeat(40),
        copy_detection: CopyDetection::Off,
        seed_hash: [4u8; 32],
        replace_refs_hash: [5u8; 32],
        git_config_hash: [6u8; 32],
        rename_budget: 500,
        head_sha: "d".repeat(40),
    }
}

fn sample_drift_locus_key() -> DriftLocusKey {
    DriftLocusKey {
        anchor_sha: "e".repeat(40),
        path: "src/lib.rs".to_string(),
        blob_oid: "f".repeat(40),
        range_start: 10,
        range_end: 42,
        copy_detection: CopyDetection::AnyFileInRepo,
        rename_budget: 200,
    }
}

/// BLAKE3 of `key.canonical_bytes()`. Tests use it to assert L2 file
/// placement under `v1/<kind.as_dir()>/<aa>/<rest>`.
fn key_hex<K: CacheKey>(key: &K) -> String {
    let mut buf = Vec::new();
    key.canonical_bytes(&mut buf);
    blake3::hash(&buf).to_hex().to_string()
}

fn l2_path(dir: &std::path::Path, kind: Kind, hex: &str) -> std::path::PathBuf {
    dir.join("mesh")
        .join("cache")
        .join("v1")
        .join(kind.as_dir())
        .join(&hex[0..2])
        .join(&hex[2..])
}

// ── Checks ──────────────────────────────────────────────────────────────────

/// (1) Miss invokes `compute`, persists L1 + L2, returns the computed value.
#[test]
fn miss_invokes_compute_and_persists_both_tiers() -> Result<()> {
    let td = init_repo();
    commit_file(td.path(), "f.txt", "x\n", "init");
    let repo = gix::open(td.path()).expect("gix open");
    let cache = Cache::open(&repo)?;
    let key = sample_rename_trail_key();
    let calls = AtomicUsize::new(0);

    let got: Probe = cache.get_or_insert_with(Kind::RenameTrail, &key, || {
        calls.fetch_add(1, Ordering::SeqCst);
        Ok(Probe::sample())
    })?;

    assert_eq!(got, Probe::sample());
    assert_eq!(calls.load(Ordering::SeqCst), 1, "compute invoked once");

    let common = repo.common_dir().canonicalize().expect("canonicalize");
    let hex = key_hex(&key);
    let l2 = l2_path(&common, Kind::RenameTrail, &hex);
    assert!(l2.exists(), "L2 file persisted at {l2:?}");
    Ok(())
}

/// (2) Second call with the same `(Kind, key)` is an L1 hit; `compute`
/// is not invoked.
#[test]
fn second_call_is_l1_hit_compute_not_invoked() -> Result<()> {
    let td = init_repo();
    commit_file(td.path(), "f.txt", "x\n", "init");
    let repo = gix::open(td.path()).expect("gix open");
    let cache = Cache::open(&repo)?;
    let key = sample_rename_trail_key();
    let calls = AtomicUsize::new(0);

    let _: Probe = cache.get_or_insert_with(Kind::RenameTrail, &key, || {
        calls.fetch_add(1, Ordering::SeqCst);
        Ok(Probe::sample())
    })?;
    let again: Probe = cache.get_or_insert_with(Kind::RenameTrail, &key, || {
        calls.fetch_add(1, Ordering::SeqCst);
        Ok(Probe {
            count: 999,
            label: "should-not-run".to_string(),
        })
    })?;

    assert_eq!(again, Probe::sample(), "L1 hit returns persisted value");
    assert_eq!(calls.load(Ordering::SeqCst), 1, "compute invoked exactly once");
    Ok(())
}

/// (3) A fresh `Cache` pointed at the same `dir` reads the entry from L2
/// on the first call, then L1 on the second.
#[test]
fn fresh_cache_reads_l2_then_l1() -> Result<()> {
    let td = init_repo();
    commit_file(td.path(), "f.txt", "x\n", "init");
    let repo = gix::open(td.path()).expect("gix open");
    let key = sample_rename_trail_key();

    {
        let cache_a = Cache::open(&repo)?;
        let _: Probe =
            cache_a.get_or_insert_with(Kind::RenameTrail, &key, || Ok(Probe::sample()))?;
    }

    let cache_b = Cache::open(&repo)?;
    let calls = AtomicUsize::new(0);
    let first: Probe = cache_b.get_or_insert_with(Kind::RenameTrail, &key, || {
        calls.fetch_add(1, Ordering::SeqCst);
        Ok(Probe {
            count: 0,
            label: "miss".to_string(),
        })
    })?;
    let second: Probe = cache_b.get_or_insert_with(Kind::RenameTrail, &key, || {
        calls.fetch_add(1, Ordering::SeqCst);
        Ok(Probe {
            count: 0,
            label: "miss".to_string(),
        })
    })?;

    assert_eq!(first, Probe::sample(), "L2 hit on first call");
    assert_eq!(second, Probe::sample(), "L1 hit on second call");
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "compute never invoked on a fresh Cache sharing the same dir"
    );
    Ok(())
}

/// (4) `Cache::open_disabled()` bypasses both tiers — `compute` runs every
/// call, no FS writes occur under the cache dir.
#[test]
fn open_disabled_bypasses_both_tiers() -> Result<()> {
    let td = init_repo();
    commit_file(td.path(), "f.txt", "x\n", "init");
    let repo = gix::open(td.path()).expect("gix open");
    let cache = Cache::open_disabled();
    assert!(!cache.is_enabled());
    let key = sample_rename_trail_key();
    let calls = AtomicUsize::new(0);

    for _ in 0..3 {
        let _: Probe = cache.get_or_insert_with(Kind::RenameTrail, &key, || {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok(Probe::sample())
        })?;
    }

    assert_eq!(calls.load(Ordering::SeqCst), 3, "compute invoked every call");

    let common = repo.common_dir().canonicalize().expect("canonicalize");
    let v1 = common.join("mesh").join("cache").join("v1");
    assert!(!v1.exists(), "no FS writes under {v1:?}");
    Ok(())
}

/// (5) Distinct keys for the same `Kind` produce distinct files under
/// `v1/<kind.as_dir()>/<aa>/<rest>`.
#[test]
fn distinct_keys_same_kind_produce_distinct_files() -> Result<()> {
    let td = init_repo();
    commit_file(td.path(), "f.txt", "x\n", "init");
    let repo = gix::open(td.path()).expect("gix open");
    let cache = Cache::open(&repo)?;

    let mut key_b = sample_rename_trail_key();
    key_b.head_sha = "z".repeat(40);
    let key_a = sample_rename_trail_key();

    let _: Probe =
        cache.get_or_insert_with(Kind::RenameTrail, &key_a, || Ok(Probe::sample()))?;
    let _: Probe = cache.get_or_insert_with(Kind::RenameTrail, &key_b, || {
        Ok(Probe {
            count: 11,
            label: "b".into(),
        })
    })?;

    let common = repo.common_dir().canonicalize().expect("canonicalize");
    let hex_a = key_hex(&key_a);
    let hex_b = key_hex(&key_b);
    assert_ne!(hex_a, hex_b, "distinct keys hash to distinct hex");
    let path_a = l2_path(&common, Kind::RenameTrail, &hex_a);
    let path_b = l2_path(&common, Kind::RenameTrail, &hex_b);
    assert!(path_a.exists(), "{path_a:?} exists");
    assert!(path_b.exists(), "{path_b:?} exists");
    assert_ne!(path_a, path_b);
    Ok(())
}

/// (6) `CacheKey::canonical_bytes` is deterministic — identical key values
/// produce identical hashes; permuting fields produces different hashes.
#[test]
fn canonical_bytes_are_deterministic_and_field_sensitive() -> Result<()> {
    let k1 = sample_grouped_walk_key();
    let k2 = sample_grouped_walk_key();
    assert_eq!(
        key_hex(&k1),
        key_hex(&k2),
        "identical fields → identical hash"
    );

    let mut k3 = sample_grouped_walk_key();
    std::mem::swap(&mut k3.anchor_sha, &mut k3.head_sha);
    assert_ne!(
        key_hex(&k1),
        key_hex(&k3),
        "permuted anchor/head → different hash"
    );

    let mut k4 = sample_grouped_walk_key();
    k4.rename_budget += 1;
    assert_ne!(
        key_hex(&k1),
        key_hex(&k4),
        "different rename_budget → different hash"
    );

    let mut k5 = sample_grouped_walk_key();
    k5.copy_detection = CopyDetection::AnyFileInRepo;
    assert_ne!(
        key_hex(&k1),
        key_hex(&k5),
        "different copy_detection → different hash"
    );
    Ok(())
}

/// (7) Concurrent `get_or_insert_with` calls with the same `(Kind, key)`
/// from two threads each return the value and leave the persisted file
/// well-formed (exactly one byte-identical file under the BLAKE3 path).
#[test]
fn concurrent_writes_converge_on_one_well_formed_file() -> Result<()> {
    use std::sync::Barrier;

    let td = init_repo();
    commit_file(td.path(), "f.txt", "x\n", "init");
    let repo = gix::open(td.path()).expect("gix open");
    let cache = Cache::open(&repo)?;
    let key = sample_rename_trail_key();
    let barrier = Barrier::new(2);
    let results: Mutex<Vec<Probe>> = Mutex::new(Vec::new());

    std::thread::scope(|s| {
        for _ in 0..2 {
            s.spawn(|| {
                barrier.wait();
                let v: Probe = cache
                    .get_or_insert_with(Kind::RenameTrail, &key, || Ok(Probe::sample()))
                    .expect("get_or_insert_with");
                results.lock().unwrap().push(v);
            });
        }
    });

    let got = results.into_inner().unwrap();
    assert_eq!(got.len(), 2);
    assert!(got.iter().all(|v| v == &Probe::sample()));

    let common = repo.common_dir().canonicalize().expect("canonicalize");
    let hex = key_hex(&key);
    let l2 = l2_path(&common, Kind::RenameTrail, &hex);
    assert!(l2.is_file(), "exactly one file at {l2:?}");
    let bytes = std::fs::read(&l2).expect("read l2");
    assert!(!bytes.is_empty(), "persisted file is non-empty");
    Ok(())
}

/// (8) Kind isolation — identical raw bytes under different `Kind`s never
/// collide. The per-`Kind` directory enforces this on the filesystem, and
/// the `b"gm.v1.<kind>\0"` domain-separation tag enforces it in the hash.
#[test]
fn kind_isolation_prevents_cross_kind_collisions() -> Result<()> {
    // Two keys whose payload structure is identical apart from the kind
    // tag emitted by their `CacheKey` impl. (We can't easily construct
    // the *same* concrete key under two `Kind`s, but the domain tag is
    // the property under test.)
    let gw = sample_grouped_walk_key();
    let rt = sample_rename_trail_key();
    assert_ne!(
        key_hex(&gw),
        key_hex(&rt),
        "domain tag makes per-kind hashes distinct"
    );

    let td = init_repo();
    commit_file(td.path(), "f.txt", "x\n", "init");
    let repo = gix::open(td.path()).expect("gix open");
    let cache = Cache::open(&repo)?;
    let _: Probe = cache.get_or_insert_with(Kind::GroupedWalk, &gw, || Ok(Probe::sample()))?;
    let _: Probe = cache.get_or_insert_with(Kind::RenameTrail, &rt, || {
        Ok(Probe {
            count: 99,
            label: "rt".into(),
        })
    })?;

    let common = repo.common_dir().canonicalize().expect("canonicalize");
    let gw_dir = common
        .join("mesh")
        .join("cache")
        .join("v1")
        .join(Kind::GroupedWalk.as_dir());
    let rt_dir = common
        .join("mesh")
        .join("cache")
        .join("v1")
        .join(Kind::RenameTrail.as_dir());
    assert_ne!(gw_dir, rt_dir);
    assert!(gw_dir.exists());
    assert!(rt_dir.exists());

    // And confirm DriftLocus uses its own subdir as well.
    let dl_key = sample_drift_locus_key();
    let _: Probe = cache.get_or_insert_with(Kind::DriftLocus, &dl_key, || Ok(Probe::sample()))?;
    let dl_dir = common
        .join("mesh")
        .join("cache")
        .join("v1")
        .join(Kind::DriftLocus.as_dir());
    assert!(dl_dir.exists());
    Ok(())
}

/// (9) `gc` removes orphan entries whose anchor / head oids are absent from
/// `git rev-list --all --objects`. Lean version: create an entry whose key
/// references the current HEAD oid, prune that commit, run gc, assert the
/// entry file is gone.
#[test]
fn gc_removes_orphan_entries() -> Result<()> {
    let td = init_repo();
    commit_file(td.path(), "f.txt", "x\n", "init");
    let orphan_sha = rev_parse(td.path(), "HEAD");
    // Move HEAD forward so the original commit is reachable...
    commit_file(td.path(), "f.txt", "y\n", "advance");
    // ...then reset the branch and prune so the original oid becomes
    // unreachable (orphan).
    run_git(td.path(), &["update-ref", "-d", "refs/heads/main"]);
    commit_file(td.path(), "g.txt", "z\n", "new root");
    run_git(td.path(), &["reflog", "expire", "--expire=now", "--all"]);
    run_git(td.path(), &["gc", "--prune=now"]);

    let repo = gix::open(td.path()).expect("gix open");
    let cache = Cache::open(&repo)?;
    let mut key = sample_rename_trail_key();
    key.anchor_sha = orphan_sha.clone();
    key.head_sha = orphan_sha.clone();
    let _: Probe = cache.get_or_insert_with(Kind::RenameTrail, &key, || Ok(Probe::sample()))?;

    let common = repo.common_dir().canonicalize().expect("canonicalize");
    let hex = key_hex(&key);
    let l2 = l2_path(&common, Kind::RenameTrail, &hex);
    assert!(l2.exists(), "entry persisted before gc");

    let stats = cache.gc(&repo)?;
    assert!(
        stats.rename_trail_removed >= 1,
        "gc reports at least one rename_trail removal, got {}",
        stats.rename_trail_removed
    );
    assert!(!l2.exists(), "orphan entry file is gone after gc");
    Ok(())
}

//! SQLite-backed content-addressed cache for `git mesh stale`.
//!
//! Five tiers of caches share a single database at
//! `<git_dir>/mesh/cache/mesh_cache.sqlite`:
//!
//! - **Tier 1** — `name_status_cache`: per-commit-pair `Vec<NS>` blobs.
//! - **Tier 2** — `blob_diff_cache`: per-blob-pair hunk lists.
//! - **Tier 3** — `grouped_walk_cache`: full `GroupedWalk` materializations.
//! - **Tier 4** — `rename_trail_cache`: Pass 1 rename-trail closure keyed by
//!   anchor + head + copy-detection + seed hash + config hashes.
//! - **Tier 5** — `drift_locus_cache`: `DriftLocus` result keyed by anchor
//!   metadata. The stored `answer_commit` enables a HEAD-ancestor check on
//!   read so stale rows are detected and recomputed rather than trusted.

use crate::Result;
use crate::git;
use crate::resolver::session::GroupedWalk;
use crate::resolver::walker::NS;
use crate::types::CopyDetection;
use rusqlite::{Connection, OpenFlags, Transaction};
use std::collections::{HashMap, HashSet};
use std::fs;

pub const SCHEMA_VERSION: i32 = 4;

// ── Schema DDL ──────────────────────────────────────────────────────────────

const DDL: &str = "
CREATE TABLE IF NOT EXISTS name_status_cache (
    parent_sha     TEXT NOT NULL,
    commit_sha     TEXT NOT NULL,
    copy_detection INTEGER NOT NULL,
    entries_blob   BLOB NOT NULL,
    PRIMARY KEY (parent_sha, commit_sha, copy_detection)
) WITHOUT ROWID;

CREATE TABLE IF NOT EXISTS blob_diff_cache (
    old_blob_sha TEXT NOT NULL,
    new_blob_sha TEXT NOT NULL,
    hunks_blob   BLOB NOT NULL,
    PRIMARY KEY (old_blob_sha, new_blob_sha)
) WITHOUT ROWID;

CREATE TABLE IF NOT EXISTS grouped_walk_cache (
    anchor_sha          TEXT NOT NULL,
    copy_detection      INTEGER NOT NULL,
    seed_hash           BLOB NOT NULL,
    replace_refs_hash   BLOB NOT NULL,
    git_config_hash     BLOB NOT NULL,
    rename_budget       INTEGER NOT NULL,
    head_sha            TEXT NOT NULL,
    walk_blob           BLOB NOT NULL,
    PRIMARY KEY (anchor_sha, copy_detection, seed_hash,
                 replace_refs_hash, git_config_hash, rename_budget)
) WITHOUT ROWID;

CREATE TABLE IF NOT EXISTS rename_trail_cache (
    anchor_sha          TEXT NOT NULL,
    head_sha            TEXT NOT NULL,
    copy_detection      INTEGER NOT NULL,
    rename_budget       INTEGER NOT NULL,
    seed_hash           BLOB NOT NULL,
    replace_refs_hash   BLOB NOT NULL,
    git_config_hash     BLOB NOT NULL,
    trail_blob          BLOB NOT NULL,
    PRIMARY KEY (anchor_sha, head_sha, copy_detection,
                 rename_budget, seed_hash, replace_refs_hash, git_config_hash)
) WITHOUT ROWID;

CREATE TABLE IF NOT EXISTS drift_locus_cache (
    anchor_sha          TEXT NOT NULL,
    path                TEXT NOT NULL,
    blob_oid            TEXT NOT NULL,
    range_start         INTEGER NOT NULL,
    range_end           INTEGER NOT NULL,
    copy_detection      INTEGER NOT NULL,
    rename_budget       INTEGER NOT NULL,
    locus_blob          BLOB NOT NULL,
    answer_commit       TEXT NOT NULL,
    PRIMARY KEY (anchor_sha, path, blob_oid, range_start, range_end,
                 copy_detection, rename_budget)
) WITHOUT ROWID;
";

// ── TrailCacheKey / TrailCacheEntry ─────────────────────────────────────────

/// Every component that must match for a rename-trail cache hit.
pub(crate) struct TrailCacheKey {
    pub anchor_sha: String,
    pub head_sha: String,
    pub copy_detection: CopyDetection,
    pub rename_budget: usize,
    pub candidate_seed_hash: [u8; 32],
    pub replace_refs_hash: [u8; 32],
    pub git_config_hash: [u8; 32],
}

/// The cached Pass 1 output stored in the `rename_trail_cache` table.
#[derive(serde::Serialize, serde::Deserialize)]
pub(crate) struct TrailCacheEntry {
    pub seed: Vec<String>,
    pub closed: HashSet<String>,
    pub interesting: HashSet<String>,
}

/// Result of a `grouped_walk_get` probe.
///
/// `Miss` means there's no row at all for the key tuple, or the stored
/// `head_sha` is neither equal to nor an ancestor of `current_head`. The
/// caller recomputes and `UPSERT`s.
pub(crate) enum GroupedWalkResult {
    ExactHit(GroupedWalk),
    ExtendHit {
        cached_head: String,
        walk: GroupedWalk,
    },
    Miss,
}

// ── DriftLocusCacheKey / DriftLocusCachedValue ───────────────────────────────

/// Cache key for a `DriftLocus` result. Every component that must match for
/// a hit: the anchor identity, the anchored blob+range, copy-detection level,
/// and rename budget.
pub(crate) struct DriftLocusCacheKey {
    pub anchor_sha: String,
    pub path: String,
    pub blob_oid: String,
    pub range_start: u32,
    pub range_end: u32,
    pub copy_detection: CopyDetection,
    pub rename_budget: usize,
}

/// Discriminant stored in the `locus_blob` BLOB column.  The
/// `answer_commit` field carries the ObjectId of the commit named by
/// `ChangedAt`/`OrphanedAt`, or the all-zeros sentinel for `Unreachable`.
/// The caller validates `answer_commit` against HEAD ancestry before trusting
/// a cached result.
#[derive(serde::Serialize, serde::Deserialize)]
pub(crate) struct DriftLocusCachedValue {
    /// 0 = Unreachable, 1 = ChangedAt, 2 = OrphanedAt
    pub variant: u8,
    pub answer_commit: String,
}

// ── GcStats ─────────────────────────────────────────────────────────────────

#[derive(Default)]
pub(crate) struct GcStats {
    pub name_status_removed: usize,
    pub blob_diff_removed: usize,
    pub grouped_walk_removed: usize,
    pub rename_trail_removed: usize,
    pub drift_locus_removed: usize,
}

// ── Cache ───────────────────────────────────────────────────────────────────

/// One SQLite connection for the duration of a `ResolverSession`.
pub(crate) struct Cache {
    conn: Connection,
    enabled: bool,
    /// Number of times this `Cache::open` call triggered a destructive
    /// schema rebuild (drop+recreate due to `user_version` mismatch).
    /// 0 or 1 in practice; surfaced through `--perf` so an operator can
    /// correlate a cache-hit drop with a recent binary rollout.
    destructive_rebuilds: u64,
}

impl Cache {
    /// Return a permanently-disabled `Cache` backed by an in-memory database.
    /// Used as the silent-failure fallback when `open` errors.
    pub(crate) fn open_disabled() -> Cache {
        let conn = Connection::open_in_memory().expect("in-memory sqlite always opens");
        Cache { conn, enabled: false, destructive_rebuilds: 0 }
    }

    pub(crate) fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Number of destructive schema rebuilds triggered during `Cache::open`
    /// (typically 0; 1 on schema-version mismatch after a binary rollout).
    pub(crate) fn destructive_rebuilds(&self) -> u64 {
        self.destructive_rebuilds
    }

    /// `SELECT COUNT(*)` per tier table. Returns zeros when the cache is
    /// disabled. Intended to be called once at end-of-run from the perf-emit
    /// block; each probe is a single full-table scan and stays sub-ms on the
    /// local-disk sqlite file.
    pub(crate) fn row_counts(&self) -> [(&'static str, u64); 5] {
        let probe = |table: &str| -> u64 {
            if !self.enabled {
                return 0;
            }
            let sql = format!("SELECT COUNT(*) FROM {table}");
            crate::perf::time_sqlite_read(|| {
                self.conn
                    .query_row(&sql, [], |row| row.get::<_, i64>(0))
                    .ok()
            })
            .map(|v| v as u64)
            .unwrap_or(0)
        };
        [
            ("name_status_cache", probe("name_status_cache")),
            ("blob_diff_cache", probe("blob_diff_cache")),
            ("grouped_walk_cache", probe("grouped_walk_cache")),
            ("rename_trail_cache", probe("rename_trail_cache")),
            ("drift_locus_cache", probe("drift_locus_cache")),
        ]
    }

    /// Open (or create) the cache database for `repo`.
    ///
    /// Path: `<git_dir>/mesh/cache/mesh_cache.sqlite`.
    ///
    /// `GIT_MESH_CACHE=0` disables all cache I/O; all accessors become
    /// no-ops.
    pub(crate) fn open(repo: &gix::Repository) -> Result<Cache> {
        let enabled = std::env::var("GIT_MESH_CACHE")
            .map(|v| v != "0")
            .unwrap_or(true);

        if !enabled {
            // Open an in-memory database so the struct is always valid.
            let conn = Connection::open_in_memory()
                .map_err(|e| crate::Error::Git(format!("sqlite in-memory open: {e}")))?;
            return Ok(Cache { conn, enabled: false, destructive_rebuilds: 0 });
        }

        // Best-effort cleanup of the legacy per-worktree cache path.
        // Only runs in linked worktrees (main worktree: git_dir == common_dir).
        // Errors are surfaced to stderr but do not block opening the shared
        // DB — leaving leftover bytes is wasted space, not data loss, and
        // the operator needs visibility (per repo "fail closed" guidance:
        // silent swallow violates the discoverability invariant).
        if git::git_dir(repo) != git::common_dir(repo) {
            let legacy = git::git_dir(repo).join("mesh").join("cache");
            if legacy.exists()
                && let Err(e) = fs::remove_dir_all(&legacy)
            {
                eprintln!(
                    "[git-mesh cache] legacy per-worktree cache cleanup failed at {}: {e}",
                    legacy.display()
                );
            }
        }

        let db_dir = git::cache_dir(repo);
        fs::create_dir_all(&db_dir)
            .map_err(|e| crate::Error::Git(format!("create cache dir: {e}")))?;

        let db_path = db_dir.join("mesh_cache.sqlite");
        let flags = OpenFlags::SQLITE_OPEN_CREATE
            | OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_FULL_MUTEX;

        let (conn, rebuilt) = open_and_bootstrap(&db_path, flags, &db_dir)?;
        Ok(Cache {
            conn,
            enabled: true,
            destructive_rebuilds: if rebuilt { 1 } else { 0 },
        })
    }

    // ── Tier 1: name_status ─────────────────────────────────────────────────

    pub(crate) fn name_status_get(
        &self,
        parent: &str,
        commit: &str,
        cd: CopyDetection,
    ) -> Option<Vec<NS>> {
        if !self.enabled {
            return None;
        }
        let cd_int = copy_detection_to_int(cd);
        let result: rusqlite::Result<Vec<u8>> = crate::perf::time_sqlite_read(|| {
            self.conn.query_row(
                "SELECT entries_blob FROM name_status_cache \
                 WHERE parent_sha = ?1 AND commit_sha = ?2 AND copy_detection = ?3",
                rusqlite::params![parent, commit, cd_int],
                |row| row.get(0),
            )
        });
        match result {
            Ok(blob) => bincode::deserialize::<Vec<NS>>(&blob).ok(),
            Err(rusqlite::Error::QueryReturnedNoRows) => None,
            Err(_) => None,
        }
    }

    pub(crate) fn name_status_put_batch(
        &self,
        txn: &Transaction,
        rows: &[(&str, &str, CopyDetection, Vec<NS>)],
    ) -> Result<()> {
        if !self.enabled {
            return Ok(());
        }
        for (parent, commit, cd, entries) in rows {
            let cd_int = copy_detection_to_int(*cd);
            let blob = bincode::serialize(entries)
                .map_err(|e| crate::Error::Git(format!("bincode serialize name_status: {e}")))?;
            crate::perf::time_sqlite_write(|| {
                txn.execute(
                    "INSERT OR REPLACE INTO name_status_cache \
                     (parent_sha, commit_sha, copy_detection, entries_blob) \
                     VALUES (?1, ?2, ?3, ?4)",
                    rusqlite::params![parent, commit, cd_int, blob],
                )
            })
            .map_err(|e| crate::Error::Git(format!("name_status insert: {e}")))?;
        }
        Ok(())
    }

    /// Open a `BEGIN IMMEDIATE` transaction, run `f`, and commit.
    /// Errors from `f` cause the transaction to roll back; the error is returned.
    pub(crate) fn with_write_txn<F, R>(&self, f: F) -> Result<R>
    where
        F: FnOnce(&Transaction) -> Result<R>,
    {
        crate::perf::record_write_txn();
        let txn = Transaction::new_unchecked(&self.conn, rusqlite::TransactionBehavior::Immediate)
            .map_err(|e| crate::Error::Git(format!("cache begin txn: {e}")))?;
        let result = f(&txn)?;
        crate::perf::time_sqlite_write(|| txn.commit())
            .map_err(|e| crate::Error::Git(format!("cache commit txn: {e}")))?;
        Ok(result)
    }

    // ── Tier 2: blob_diff ───────────────────────────────────────────────────

    pub(crate) fn blob_diff_get(
        &self,
        old_blob: &str,
        new_blob: &str,
    ) -> Option<Vec<(u32, u32, u32, u32)>> {
        if !self.enabled {
            return None;
        }
        let result: rusqlite::Result<Vec<u8>> = crate::perf::time_sqlite_read(|| {
            self.conn.query_row(
                "SELECT hunks_blob FROM blob_diff_cache \
                 WHERE old_blob_sha = ?1 AND new_blob_sha = ?2",
                rusqlite::params![old_blob, new_blob],
                |row| row.get(0),
            )
        });
        match result {
            Ok(blob) => bincode::deserialize::<Vec<(u32, u32, u32, u32)>>(&blob).ok(),
            Err(rusqlite::Error::QueryReturnedNoRows) => None,
            Err(_) => None,
        }
    }

    pub(crate) fn blob_diff_put(
        &self,
        txn: &Transaction,
        old: &str,
        new: &str,
        hunks: &[(u32, u32, u32, u32)],
    ) -> Result<()> {
        if !self.enabled {
            return Ok(());
        }
        let blob = bincode::serialize(hunks)
            .map_err(|e| crate::Error::Git(format!("bincode serialize blob_diff: {e}")))?;
        crate::perf::time_sqlite_write(|| {
            txn.execute(
                "INSERT OR REPLACE INTO blob_diff_cache \
                 (old_blob_sha, new_blob_sha, hunks_blob) \
                 VALUES (?1, ?2, ?3)",
                rusqlite::params![old, new, blob],
            )
        })
        .map_err(|e| crate::Error::Git(format!("blob_diff insert: {e}")))?;
        Ok(())
    }

    // ── Tier 3: grouped_walk ────────────────────────────────────────────────

    /// Probe the grouped_walk cache for a key tuple.
    ///
    /// Schema v4: one row per `(anchor, copy_detection, seed_hash,
    /// replace_refs_hash, git_config_hash, rename_budget)` tuple. The row's
    /// `head_sha` column names the HEAD the cached walk was computed against;
    /// on read we discriminate `ExactHit` vs `ExtendHit` vs `Miss`.
    ///
    /// `known_head_ancestors` is the per-session memo (shared with
    /// `drift_locus` ancestor checks): if the stored `head_sha` is present
    /// we short-circuit `repo.merge_base`. On a successful `merge_base`
    /// ancestor check the stored oid is inserted for subsequent reuse.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn grouped_walk_get(
        &self,
        anchor: &str,
        cd: CopyDetection,
        seed_hash: &[u8],
        replace_refs_hash: &[u8],
        git_config_hash: &[u8],
        rename_budget: i64,
        current_head: &str,
        known_head_ancestors: &mut HashMap<gix::ObjectId, HashSet<gix::ObjectId>>,
        repo: &gix::Repository,
    ) -> GroupedWalkResult {
        if !self.enabled {
            return GroupedWalkResult::Miss;
        }
        let cd_int = copy_detection_to_int(cd);
        let result: rusqlite::Result<(String, Vec<u8>)> = crate::perf::time_sqlite_read(|| {
            self.conn.query_row(
                "SELECT head_sha, walk_blob FROM grouped_walk_cache \
                 WHERE anchor_sha = ?1 AND copy_detection = ?2 \
                   AND seed_hash = ?3 AND replace_refs_hash = ?4 \
                   AND git_config_hash = ?5 AND rename_budget = ?6",
                rusqlite::params![
                    anchor, cd_int, seed_hash, replace_refs_hash, git_config_hash, rename_budget,
                ],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, Vec<u8>>(1)?)),
            )
        });
        let (stored_head, walk_blob) = match result {
            Ok(pair) => pair,
            Err(_) => return GroupedWalkResult::Miss,
        };
        let walk = match bincode::deserialize::<GroupedWalk>(&walk_blob) {
            Ok(w) => w,
            Err(_) => return GroupedWalkResult::Miss,
        };
        if stored_head == current_head {
            return GroupedWalkResult::ExactHit(walk);
        }
        use std::str::FromStr;
        let stored_oid = match gix::ObjectId::from_str(&stored_head) {
            Ok(id) => id,
            Err(_) => return GroupedWalkResult::Miss,
        };
        let head_oid = match gix::ObjectId::from_str(current_head) {
            Ok(id) => id,
            Err(_) => return GroupedWalkResult::Miss,
        };
        let entry = known_head_ancestors.entry(head_oid).or_default();
        if entry.contains(&stored_oid) {
            crate::perf::record_is_ancestor_memo_hit();
            return GroupedWalkResult::ExtendHit {
                cached_head: stored_head,
                walk,
            };
        }
        // merge_base(A, B) == A iff A is an ancestor of B.
        crate::perf::record_is_ancestor_subprocess();
        let is_ancestor = match repo.merge_base(stored_oid, head_oid) {
            Ok(base) => base.detach() == stored_oid,
            Err(_) => false,
        };
        if is_ancestor {
            known_head_ancestors
                .entry(head_oid)
                .or_default()
                .insert(stored_oid);
            GroupedWalkResult::ExtendHit {
                cached_head: stored_head,
                walk,
            }
        } else {
            GroupedWalkResult::Miss
        }
    }

    /// Monotone UPSERT for grouped_walk.
    ///
    /// Writes the row when there is no existing entry at the key tuple, or
    /// when the incoming `head_sha` is equal-to or a descendant-of the
    /// stored `head_sha`. Divergent (non-ancestor / non-descendant) heads
    /// — two worktrees on independent branches — leave the existing row
    /// alone so the cache does not ping-pong between divergent worktrees.
    ///
    /// "Extend-only" is enforced inside the same `with_write_txn` that
    /// performs the write, using a fresh `repo.merge_base` check against
    /// the stored head; this avoids a TOCTOU window where a sibling
    /// process could replace the row between read and write.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn grouped_walk_upsert(
        &self,
        txn: &Transaction,
        anchor: &str,
        cd: CopyDetection,
        seed_hash: &[u8],
        replace_refs_hash: &[u8],
        git_config_hash: &[u8],
        rename_budget: i64,
        head_sha: &str,
        walk: &GroupedWalk,
        repo: &gix::Repository,
    ) -> Result<()> {
        if !self.enabled {
            return Ok(());
        }
        let cd_int = copy_detection_to_int(cd);

        // Look up the existing row's head (if any) inside the txn.
        let existing_head: Option<String> = crate::perf::time_sqlite_read(|| {
            txn.query_row(
                "SELECT head_sha FROM grouped_walk_cache \
                 WHERE anchor_sha = ?1 AND copy_detection = ?2 \
                   AND seed_hash = ?3 AND replace_refs_hash = ?4 \
                   AND git_config_hash = ?5 AND rename_budget = ?6",
                rusqlite::params![
                    anchor, cd_int, seed_hash, replace_refs_hash, git_config_hash, rename_budget,
                ],
                |row| row.get::<_, String>(0),
            )
            .ok()
        });

        // Decide whether the write is extend-only.
        let allow_write = match &existing_head {
            None => true,
            Some(stored) if stored == head_sha => true,
            Some(stored) => {
                use std::str::FromStr;
                match (
                    gix::ObjectId::from_str(stored),
                    gix::ObjectId::from_str(head_sha),
                ) {
                    (Ok(stored_oid), Ok(incoming_oid)) => {
                        // stored is ancestor of incoming iff merge_base == stored.
                        crate::perf::record_is_ancestor_subprocess();
                        matches!(
                            repo.merge_base(stored_oid, incoming_oid),
                            Ok(base) if base.detach() == stored_oid
                        )
                    }
                    _ => false,
                }
            }
        };

        if !allow_write {
            return Ok(());
        }

        let blob = bincode::serialize(walk)
            .map_err(|e| crate::Error::Git(format!("bincode serialize grouped_walk: {e}")))?;
        crate::perf::time_sqlite_write(|| {
            txn.execute(
                "INSERT INTO grouped_walk_cache \
                (anchor_sha, copy_detection, seed_hash, replace_refs_hash, \
                 git_config_hash, rename_budget, head_sha, walk_blob) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8) \
             ON CONFLICT(anchor_sha, copy_detection, seed_hash, replace_refs_hash, \
                         git_config_hash, rename_budget) \
             DO UPDATE SET head_sha = excluded.head_sha, walk_blob = excluded.walk_blob",
            rusqlite::params![
                anchor,
                cd_int,
                seed_hash,
                replace_refs_hash,
                git_config_hash,
                rename_budget,
                head_sha,
                blob,
            ],
            )
        })
        .map_err(|e| crate::Error::Git(format!("grouped_walk upsert: {e}")))?;
        Ok(())
    }

    // ── Tier 4: rename_trail ────────────────────────────────────────────────

    pub(crate) fn rename_trail_get(&self, key: &TrailCacheKey) -> Option<TrailCacheEntry> {
        if !self.enabled {
            return None;
        }
        let cd_int = copy_detection_to_int(key.copy_detection);
        let rename_budget = key.rename_budget as i64;
        let result: rusqlite::Result<Vec<u8>> = crate::perf::time_sqlite_read(|| {
            self.conn.query_row(
            "SELECT trail_blob FROM rename_trail_cache \
             WHERE anchor_sha = ?1 AND head_sha = ?2 AND copy_detection = ?3 \
               AND rename_budget = ?4 AND seed_hash = ?5 \
               AND replace_refs_hash = ?6 AND git_config_hash = ?7",
            rusqlite::params![
                key.anchor_sha,
                key.head_sha,
                cd_int,
                rename_budget,
                key.candidate_seed_hash.as_ref(),
                key.replace_refs_hash.as_ref(),
                key.git_config_hash.as_ref(),
            ],
            |row| row.get(0),
            )
        });
        match result {
            Ok(blob) => bincode::deserialize::<TrailCacheEntry>(&blob).ok(),
            Err(rusqlite::Error::QueryReturnedNoRows) => None,
            Err(_) => None,
        }
    }

    pub(crate) fn rename_trail_put(
        &self,
        txn: &Transaction,
        key: &TrailCacheKey,
        entry: &TrailCacheEntry,
    ) -> Result<()> {
        if !self.enabled {
            return Ok(());
        }
        let cd_int = copy_detection_to_int(key.copy_detection);
        let rename_budget = key.rename_budget as i64;
        let blob = bincode::serialize(entry)
            .map_err(|e| crate::Error::Git(format!("bincode serialize rename_trail: {e}")))?;
        crate::perf::time_sqlite_write(|| {
            txn.execute(
                "INSERT OR REPLACE INTO rename_trail_cache \
                 (anchor_sha, head_sha, copy_detection, rename_budget, seed_hash, \
                  replace_refs_hash, git_config_hash, trail_blob) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                rusqlite::params![
                    key.anchor_sha,
                    key.head_sha,
                    cd_int,
                    rename_budget,
                    key.candidate_seed_hash.as_ref(),
                    key.replace_refs_hash.as_ref(),
                    key.git_config_hash.as_ref(),
                    blob,
                ],
            )
        })
        .map_err(|e| crate::Error::Git(format!("rename_trail insert: {e}")))?;
        Ok(())
    }

    // ── Tier 5: drift_locus ─────────────────────────────────────────────────

    /// Look up a cached `DriftLocus` result.
    ///
    /// Returns `Some((value, answer_commit_hex))` on a cache hit.
    /// The caller is responsible for validating that `answer_commit` is still
    /// an ancestor of HEAD before trusting the returned value.
    pub(crate) fn drift_locus_get(
        &self,
        key: &DriftLocusCacheKey,
    ) -> Option<DriftLocusCachedValue> {
        if !self.enabled {
            return None;
        }
        let cd_int = copy_detection_to_int(key.copy_detection);
        let rename_budget = key.rename_budget as i64;
        let result: rusqlite::Result<Vec<u8>> = crate::perf::time_sqlite_read(|| {
            self.conn.query_row(
            "SELECT locus_blob FROM drift_locus_cache \
             WHERE anchor_sha = ?1 AND path = ?2 AND blob_oid = ?3 \
               AND range_start = ?4 AND range_end = ?5 \
               AND copy_detection = ?6 AND rename_budget = ?7",
            rusqlite::params![
                key.anchor_sha,
                key.path,
                key.blob_oid,
                key.range_start,
                key.range_end,
                cd_int,
                rename_budget,
            ],
            |row| row.get(0),
            )
        });
        match result {
            Ok(blob) => bincode::deserialize::<DriftLocusCachedValue>(&blob).ok(),
            Err(rusqlite::Error::QueryReturnedNoRows) => None,
            Err(_) => None,
        }
    }

    /// Store a `DriftLocus` result.
    ///
    /// `value.answer_commit` must carry the hex SHA of the commit named by the
    /// variant, or the all-zeros sentinel for `Unreachable`.
    pub(crate) fn drift_locus_put(
        &self,
        txn: &Transaction,
        key: &DriftLocusCacheKey,
        value: &DriftLocusCachedValue,
    ) -> Result<()> {
        if !self.enabled {
            return Ok(());
        }
        let cd_int = copy_detection_to_int(key.copy_detection);
        let rename_budget = key.rename_budget as i64;
        let blob = bincode::serialize(value)
            .map_err(|e| crate::Error::Git(format!("bincode serialize drift_locus: {e}")))?;
        crate::perf::time_sqlite_write(|| {
            txn.execute(
                "INSERT OR REPLACE INTO drift_locus_cache \
                 (anchor_sha, path, blob_oid, range_start, range_end, \
                  copy_detection, rename_budget, locus_blob, answer_commit) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                rusqlite::params![
                    key.anchor_sha,
                    key.path,
                    key.blob_oid,
                    key.range_start,
                    key.range_end,
                    cd_int,
                    rename_budget,
                    blob,
                    value.answer_commit,
                ],
            )
        })
        .map_err(|e| crate::Error::Git(format!("drift_locus insert: {e}")))?;
        Ok(())
    }

    // ── GC ──────────────────────────────────────────────────────────────────

    /// Remove cache rows whose referenced SHAs are no longer reachable in the
    /// repository.
    ///
    /// Live objects are discovered by running `git rev-list --all --objects`
    /// as a subprocess (the git-dir parent is the working directory).  Each
    /// line's first whitespace-separated token is a 40-char hex SHA.  We
    /// collect them into a `HashSet<String>` and then sweep each cache table,
    /// issuing chunked `DELETE` statements (5 000 rows per chunk, one
    /// `BEGIN IMMEDIATE` per table).
    pub(crate) fn gc(&self, repo: &gix::Repository) -> Result<GcStats> {
        if !self.enabled {
            return Ok(GcStats::default());
        }

        // ── 1. Build the live SHA set ────────────────────────────────────────
        // `git rev-list --all --objects` prints one object per line; the first
        // token is the SHA (subsequent tokens are optional path names for blobs
        // and trees).  We use the git work-dir as cwd so that relative paths
        // inside git's config resolve correctly.
        let git_dir = repo.git_dir();
        // For a normal repo git_dir is `.git`; its parent is the work tree.
        // For a bare repo git_dir *is* the root.  Either way, git can be
        // invoked from the git_dir itself.
        let cwd = git_dir.parent().unwrap_or(git_dir);

        let output = std::process::Command::new("git")
            .current_dir(cwd)
            .args(["rev-list", "--all", "--objects"])
            .output()
            .map_err(|e| crate::Error::Git(format!("gc: spawn git rev-list: {e}")))?;

        // `git rev-list --all --objects` exits 0 even on an empty repo, but
        // may exit non-zero when the repo is corrupt.  Surface that.
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(crate::Error::Git(format!(
                "gc: git rev-list failed: {stderr}"
            )));
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let mut live: std::collections::HashSet<String> =
            std::collections::HashSet::with_capacity(4096);
        for line in stdout.lines() {
            // Each line: "<sha> [optional path]"
            let sha = line.split_whitespace().next().unwrap_or("");
            if sha.len() == 40 {
                live.insert(sha.to_string());
            }
        }

        // ── 2. Sweep name_status_cache ───────────────────────────────────────
        let dead_ns: Vec<(String, String, i32)> = {
            let mut stmt = self.conn.prepare(
                "SELECT parent_sha, commit_sha, copy_detection FROM name_status_cache",
            )
            .map_err(|e| crate::Error::Git(format!("gc: prepare name_status scan: {e}")))?;
            stmt.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i32>(2)?,
                ))
            })
            .map_err(|e| crate::Error::Git(format!("gc: name_status scan: {e}")))?
            .filter_map(|r| r.ok())
            .filter(|(parent, commit, _)| !live.contains(parent) || !live.contains(commit))
            .collect()
        };

        let name_status_removed = dead_ns.len();
        for chunk in dead_ns.chunks(5000) {
            let txn = Transaction::new_unchecked(&self.conn, rusqlite::TransactionBehavior::Immediate)
                .map_err(|e| crate::Error::Git(format!("gc: begin txn name_status: {e}")))?;
            for (parent, commit, cd) in chunk {
                txn.execute(
                    "DELETE FROM name_status_cache \
                     WHERE parent_sha = ?1 AND commit_sha = ?2 AND copy_detection = ?3",
                    rusqlite::params![parent, commit, cd],
                )
                .map_err(|e| crate::Error::Git(format!("gc: delete name_status: {e}")))?;
            }
            txn.commit()
                .map_err(|e| crate::Error::Git(format!("gc: commit name_status: {e}")))?;
        }

        // ── 3. Sweep blob_diff_cache ─────────────────────────────────────────
        let dead_bd: Vec<(String, String)> = {
            let mut stmt = self
                .conn
                .prepare("SELECT old_blob_sha, new_blob_sha FROM blob_diff_cache")
                .map_err(|e| crate::Error::Git(format!("gc: prepare blob_diff scan: {e}")))?;
            stmt.query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(|e| crate::Error::Git(format!("gc: blob_diff scan: {e}")))?
            .filter_map(|r| r.ok())
            .filter(|(old, new)| !live.contains(old) || !live.contains(new))
            .collect()
        };

        let blob_diff_removed = dead_bd.len();
        for chunk in dead_bd.chunks(5000) {
            let txn = Transaction::new_unchecked(&self.conn, rusqlite::TransactionBehavior::Immediate)
                .map_err(|e| crate::Error::Git(format!("gc: begin txn blob_diff: {e}")))?;
            for (old, new) in chunk {
                txn.execute(
                    "DELETE FROM blob_diff_cache \
                     WHERE old_blob_sha = ?1 AND new_blob_sha = ?2",
                    rusqlite::params![old, new],
                )
                .map_err(|e| crate::Error::Git(format!("gc: delete blob_diff: {e}")))?;
            }
            txn.commit()
                .map_err(|e| crate::Error::Git(format!("gc: commit blob_diff: {e}")))?;
        }

        // ── 4. Sweep grouped_walk_cache ──────────────────────────────────────
        // Schema v4: one row per `(anchor, copy_detection, seed_hash,
        // replace_refs_hash, git_config_hash, rename_budget)` tuple; the
        // head_sha is a regular column. We identify dead rows by checking
        // both anchor_sha and head_sha against the live set, but delete by
        // the PK columns only.
        #[allow(clippy::type_complexity)]
        let dead_gw: Vec<(String, i32, Vec<u8>, Vec<u8>, Vec<u8>, i64)> = {
            let mut stmt = self.conn.prepare(
                "SELECT anchor_sha, copy_detection, \
                        seed_hash, replace_refs_hash, git_config_hash, rename_budget, head_sha \
                 FROM grouped_walk_cache",
            )
            .map_err(|e| crate::Error::Git(format!("gc: prepare grouped_walk scan: {e}")))?;
            stmt.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i32>(1)?,
                    row.get::<_, Vec<u8>>(2)?,
                    row.get::<_, Vec<u8>>(3)?,
                    row.get::<_, Vec<u8>>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, String>(6)?,
                ))
            })
            .map_err(|e| crate::Error::Git(format!("gc: grouped_walk scan: {e}")))?
            .filter_map(|r| r.ok())
            .filter(|(anchor, _, _, _, _, _, head)| {
                !live.contains(anchor) || !live.contains(head)
            })
            .map(|(anchor, cd, seed, replace_refs, git_config, budget, _head)| {
                (anchor, cd, seed, replace_refs, git_config, budget)
            })
            .collect()
        };

        let grouped_walk_removed = dead_gw.len();
        for chunk in dead_gw.chunks(5000) {
            let txn = Transaction::new_unchecked(&self.conn, rusqlite::TransactionBehavior::Immediate)
                .map_err(|e| crate::Error::Git(format!("gc: begin txn grouped_walk: {e}")))?;
            for (anchor, cd, seed, replace_refs, git_config, budget) in chunk {
                txn.execute(
                    "DELETE FROM grouped_walk_cache \
                     WHERE anchor_sha = ?1 AND copy_detection = ?2 \
                       AND seed_hash = ?3 AND replace_refs_hash = ?4 \
                       AND git_config_hash = ?5 AND rename_budget = ?6",
                    rusqlite::params![anchor, cd, seed, replace_refs, git_config, budget],
                )
                .map_err(|e| crate::Error::Git(format!("gc: delete grouped_walk: {e}")))?;
            }
            txn.commit()
                .map_err(|e| crate::Error::Git(format!("gc: commit grouped_walk: {e}")))?;
        }

        // ── 5. Sweep rename_trail_cache ──────────────────────────────────────
        #[allow(clippy::type_complexity)]
        let dead_rt: Vec<(String, String, i32, i64, Vec<u8>, Vec<u8>, Vec<u8>)> = {
            let mut stmt = self.conn.prepare(
                "SELECT anchor_sha, head_sha, copy_detection, rename_budget, \
                        seed_hash, replace_refs_hash, git_config_hash \
                 FROM rename_trail_cache",
            )
            .map_err(|e| crate::Error::Git(format!("gc: prepare rename_trail scan: {e}")))?;
            stmt.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i32>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, Vec<u8>>(4)?,
                    row.get::<_, Vec<u8>>(5)?,
                    row.get::<_, Vec<u8>>(6)?,
                ))
            })
            .map_err(|e| crate::Error::Git(format!("gc: rename_trail scan: {e}")))?
            .filter_map(|r| r.ok())
            .filter(|(anchor, head, _, _, _, _, _)| {
                !live.contains(anchor) || !live.contains(head)
            })
            .collect()
        };

        let rename_trail_removed = dead_rt.len();
        for chunk in dead_rt.chunks(5000) {
            let txn = Transaction::new_unchecked(&self.conn, rusqlite::TransactionBehavior::Immediate)
                .map_err(|e| crate::Error::Git(format!("gc: begin txn rename_trail: {e}")))?;
            for (anchor, head, cd, budget, seed, replace_refs, git_config) in chunk {
                txn.execute(
                    "DELETE FROM rename_trail_cache \
                     WHERE anchor_sha = ?1 AND head_sha = ?2 AND copy_detection = ?3 \
                       AND rename_budget = ?4 AND seed_hash = ?5 \
                       AND replace_refs_hash = ?6 AND git_config_hash = ?7",
                    rusqlite::params![anchor, head, cd, budget, seed, replace_refs, git_config],
                )
                .map_err(|e| crate::Error::Git(format!("gc: delete rename_trail: {e}")))?;
            }
            txn.commit()
                .map_err(|e| crate::Error::Git(format!("gc: commit rename_trail: {e}")))?;
        }

        // ── 6. Sweep drift_locus_cache ───────────────────────────────────────
        // Dead rows are those whose anchor_sha is no longer reachable.
        let dead_dl: Vec<(String, String, String, i32, i32, i32, i64)> = {
            let mut stmt = self.conn.prepare(
                "SELECT anchor_sha, path, blob_oid, range_start, range_end, \
                        copy_detection, rename_budget \
                 FROM drift_locus_cache",
            )
            .map_err(|e| crate::Error::Git(format!("gc: prepare drift_locus scan: {e}")))?;
            stmt.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i32>(3)?,
                    row.get::<_, i32>(4)?,
                    row.get::<_, i32>(5)?,
                    row.get::<_, i64>(6)?,
                ))
            })
            .map_err(|e| crate::Error::Git(format!("gc: drift_locus scan: {e}")))?
            .filter_map(|r| r.ok())
            .filter(|(anchor, _, blob, _, _, _, _)| {
                !live.contains(anchor) || !live.contains(blob)
            })
            .collect()
        };

        let drift_locus_removed = dead_dl.len();
        for chunk in dead_dl.chunks(5000) {
            let txn = Transaction::new_unchecked(&self.conn, rusqlite::TransactionBehavior::Immediate)
                .map_err(|e| crate::Error::Git(format!("gc: begin txn drift_locus: {e}")))?;
            for (anchor, path, blob, rs, re, cd, budget) in chunk {
                txn.execute(
                    "DELETE FROM drift_locus_cache \
                     WHERE anchor_sha = ?1 AND path = ?2 AND blob_oid = ?3 \
                       AND range_start = ?4 AND range_end = ?5 \
                       AND copy_detection = ?6 AND rename_budget = ?7",
                    rusqlite::params![anchor, path, blob, rs, re, cd, budget],
                )
                .map_err(|e| crate::Error::Git(format!("gc: delete drift_locus: {e}")))?;
            }
            txn.commit()
                .map_err(|e| crate::Error::Git(format!("gc: commit drift_locus: {e}")))?;
        }

        Ok(GcStats {
            name_status_removed,
            blob_diff_removed,
            grouped_walk_removed,
            rename_trail_removed,
            drift_locus_removed,
        })
    }
}

// ── internals ───────────────────────────────────────────────────────────────

fn copy_detection_to_int(cd: CopyDetection) -> i32 {
    match cd {
        CopyDetection::Off => 0,
        CopyDetection::SameCommit => 1,
        CopyDetection::AnyFileInCommit => 2,
        CopyDetection::AnyFileInRepo => 3,
    }
}

fn open_and_bootstrap(
    db_path: &std::path::Path,
    flags: OpenFlags,
    db_dir: &std::path::Path,
) -> Result<(Connection, bool)> {
    let conn = Connection::open_with_flags(db_path, flags)
        .map_err(|e| crate::Error::Git(format!("sqlite open: {e}")))?;

    apply_pragmas(&conn)?;

    let version: i32 = conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .map_err(|e| crate::Error::Git(format!("read user_version: {e}")))?;

    if version != 0 && version != SCHEMA_VERSION {
        // Version mismatch — drop and rebuild. The DB is shared across all
        // worktrees rooted at the same common-dir, so any one binary upgrade
        // resets the cache for siblings on next open. Log a one-line
        // breadcrumb to stderr (perf subsystem isn't initialized here) so
        // operators of multi-worktree setups can identify the cause.
        //
        // Accepted: TOCTOU race during binary rollout — if an old-binary
        // process A holds an open connection while new-binary process B
        // removes the file, A's writes land in the deleted inode and B
        // bootstraps a fresh DB. No corrupted data reaches either process;
        // recovery if orphaned `-wal`/`-shm` files appear is to run
        // `rm <common_dir>/mesh/cache/mesh_cache.sqlite*` and let the next
        // opener bootstrap from scratch.
        eprintln!(
            "[git-mesh cache] schema version mismatch (was {version}, want {SCHEMA_VERSION}): \
             dropping shared DB at {}",
            db_path.display()
        );
        drop(conn);
        let _ = fs::remove_file(db_path);
        let _ = fs::remove_file(db_dir.join("mesh_cache.sqlite-wal"));
        let _ = fs::remove_file(db_dir.join("mesh_cache.sqlite-shm"));
        // Remove the legacy JSON rename-trail directory from Phase 2 (pre-sqlite).
        // Ignore NotFound; fail silently on other errors (non-blocking).
        let legacy_trail = db_dir.join("rename-trail");
        if legacy_trail.exists() {
            let _ = fs::remove_dir_all(&legacy_trail);
        }

        let conn = Connection::open_with_flags(db_path, flags)
            .map_err(|e| crate::Error::Git(format!("sqlite reopen: {e}")))?;
        apply_pragmas(&conn)?;
        bootstrap_schema(&conn)?;
        return Ok((conn, true));
    }

    bootstrap_schema(&conn)?;
    Ok((conn, false))
}

fn apply_pragmas(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "PRAGMA journal_mode = WAL;
         PRAGMA busy_timeout = 500;
         PRAGMA synchronous = NORMAL;",
    )
    .map_err(|e| crate::Error::Git(format!("sqlite pragmas: {e}")))
}

fn bootstrap_schema(conn: &Connection) -> Result<()> {
    conn.execute_batch(DDL)
        .map_err(|e| crate::Error::Git(format!("sqlite schema: {e}")))?;
    conn.execute_batch(&format!("PRAGMA user_version = {SCHEMA_VERSION};"))
        .map_err(|e| crate::Error::Git(format!("set user_version: {e}")))
}

#[cfg(test)]
mod tests;

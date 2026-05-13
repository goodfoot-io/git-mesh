//! SQLite-backed content-addressed cache for `git mesh stale`.
//!
//! Four tiers of caches share a single database at
//! `<git_dir>/mesh/cache/mesh_cache.sqlite`:
//!
//! - **Tier 1** — `name_status_cache`: per-commit-pair `Vec<NS>` blobs.
//! - **Tier 2** — `blob_diff_cache`: per-blob-pair hunk lists.
//! - **Tier 3** — `grouped_walk_cache`: full `GroupedWalk` materializations.
//! - **Tier 4** — `rename_trail_cache`: Pass 1 rename-trail closure keyed by
//!   anchor + head + copy-detection + seed hash + config hashes.

use crate::Result;
use crate::git;
use crate::resolver::session::GroupedWalk;
use crate::resolver::walker::NS;
use crate::types::CopyDetection;
use rusqlite::{Connection, OpenFlags, Transaction};
use std::collections::HashSet;
use std::fs;

pub const SCHEMA_VERSION: i32 = 2;

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
    head_sha            TEXT NOT NULL,
    copy_detection      INTEGER NOT NULL,
    seed_hash           BLOB NOT NULL,
    replace_refs_hash   BLOB NOT NULL,
    git_config_hash     BLOB NOT NULL,
    rename_budget       INTEGER NOT NULL,
    walk_blob           BLOB NOT NULL,
    PRIMARY KEY (anchor_sha, head_sha, copy_detection,
                 seed_hash, replace_refs_hash, git_config_hash, rename_budget)
) WITHOUT ROWID;

CREATE INDEX IF NOT EXISTS grouped_walk_anchor_idx
    ON grouped_walk_cache (anchor_sha, copy_detection);

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

// ── GroupedWalkKey ──────────────────────────────────────────────────────────

/// Cache key for a full `GroupedWalk` materialization.  Mirrors
/// [`TrailCacheKey`] plus the `head_sha` that the walk was computed against.
pub(crate) struct GroupedWalkKey {
    pub anchor_sha: String,
    pub head_sha: String,
    pub copy_detection: CopyDetection,
    pub seed_hash: [u8; 32],
    pub replace_refs_hash: [u8; 32],
    pub git_config_hash: [u8; 32],
    pub rename_budget: usize,
}

impl GroupedWalkKey {
    pub(crate) fn from_trail_key(trail: &crate::resolver::cache::TrailCacheKey, head_sha: String) -> Self {
        Self {
            anchor_sha: trail.anchor_sha.clone(),
            head_sha,
            copy_detection: trail.copy_detection,
            seed_hash: trail.candidate_seed_hash,
            replace_refs_hash: trail.replace_refs_hash,
            git_config_hash: trail.git_config_hash,
            rename_budget: trail.rename_budget,
        }
    }
}

// ── GcStats ─────────────────────────────────────────────────────────────────

#[derive(Default)]
pub(crate) struct GcStats {
    pub name_status_removed: usize,
    pub blob_diff_removed: usize,
    pub grouped_walk_removed: usize,
    pub rename_trail_removed: usize,
}

// ── Cache ───────────────────────────────────────────────────────────────────

/// One SQLite connection for the duration of a `ResolverSession`.
pub(crate) struct Cache {
    conn: Connection,
    enabled: bool,
}

impl Cache {
    /// Return a permanently-disabled `Cache` backed by an in-memory database.
    /// Used as the silent-failure fallback when `open` errors.
    pub(crate) fn open_disabled() -> Cache {
        let conn = Connection::open_in_memory().expect("in-memory sqlite always opens");
        Cache { conn, enabled: false }
    }

    pub(crate) fn is_enabled(&self) -> bool {
        self.enabled
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
            return Ok(Cache { conn, enabled: false });
        }

        let db_dir = git::mesh_dir(repo).join("cache");
        fs::create_dir_all(&db_dir)
            .map_err(|e| crate::Error::Git(format!("create cache dir: {e}")))?;

        let db_path = db_dir.join("mesh_cache.sqlite");
        let flags = OpenFlags::SQLITE_OPEN_CREATE
            | OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_FULL_MUTEX;

        let conn = open_and_bootstrap(&db_path, flags, &db_dir)?;
        Ok(Cache { conn, enabled: true })
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
        let result: rusqlite::Result<Vec<u8>> = self.conn.query_row(
            "SELECT entries_blob FROM name_status_cache \
             WHERE parent_sha = ?1 AND commit_sha = ?2 AND copy_detection = ?3",
            rusqlite::params![parent, commit, cd_int],
            |row| row.get(0),
        );
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
            txn.execute(
                "INSERT OR REPLACE INTO name_status_cache \
                 (parent_sha, commit_sha, copy_detection, entries_blob) \
                 VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![parent, commit, cd_int, blob],
            )
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
        let txn = Transaction::new_unchecked(&self.conn, rusqlite::TransactionBehavior::Immediate)
            .map_err(|e| crate::Error::Git(format!("cache begin txn: {e}")))?;
        let result = f(&txn)?;
        txn.commit()
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
        let result: rusqlite::Result<Vec<u8>> = self.conn.query_row(
            "SELECT hunks_blob FROM blob_diff_cache \
             WHERE old_blob_sha = ?1 AND new_blob_sha = ?2",
            rusqlite::params![old_blob, new_blob],
            |row| row.get(0),
        );
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
        txn.execute(
            "INSERT OR REPLACE INTO blob_diff_cache \
             (old_blob_sha, new_blob_sha, hunks_blob) \
             VALUES (?1, ?2, ?3)",
            rusqlite::params![old, new, blob],
        )
        .map_err(|e| crate::Error::Git(format!("blob_diff insert: {e}")))?;
        Ok(())
    }

    // ── Tier 3: grouped_walk ────────────────────────────────────────────────

    pub(crate) fn grouped_walk_get_exact(&self, key: &GroupedWalkKey) -> Option<GroupedWalk> {
        if !self.enabled {
            return None;
        }
        let cd_int = copy_detection_to_int(key.copy_detection);
        let rename_budget = key.rename_budget as i64;
        let result: rusqlite::Result<Vec<u8>> = self.conn.query_row(
            "SELECT walk_blob FROM grouped_walk_cache \
             WHERE anchor_sha = ?1 AND head_sha = ?2 AND copy_detection = ?3 \
               AND seed_hash = ?4 AND replace_refs_hash = ?5 \
               AND git_config_hash = ?6 AND rename_budget = ?7",
            rusqlite::params![
                key.anchor_sha,
                key.head_sha,
                cd_int,
                key.seed_hash.as_ref(),
                key.replace_refs_hash.as_ref(),
                key.git_config_hash.as_ref(),
                rename_budget,
            ],
            |row| row.get(0),
        );
        match result {
            Ok(blob) => bincode::deserialize::<GroupedWalk>(&blob).ok(),
            Err(rusqlite::Error::QueryReturnedNoRows) => None,
            Err(_) => None,
        }
    }

    pub(crate) fn grouped_walk_put_exact(
        &self,
        txn: &Transaction,
        key: &GroupedWalkKey,
        walk: &GroupedWalk,
    ) -> Result<()> {
        if !self.enabled {
            return Ok(());
        }
        let cd_int = copy_detection_to_int(key.copy_detection);
        let rename_budget = key.rename_budget as i64;
        let blob = bincode::serialize(walk)
            .map_err(|e| crate::Error::Git(format!("bincode serialize grouped_walk: {e}")))?;
        txn.execute(
            "INSERT OR REPLACE INTO grouped_walk_cache \
             (anchor_sha, head_sha, copy_detection, seed_hash, replace_refs_hash, \
              git_config_hash, rename_budget, walk_blob) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            rusqlite::params![
                key.anchor_sha,
                key.head_sha,
                cd_int,
                key.seed_hash.as_ref(),
                key.replace_refs_hash.as_ref(),
                key.git_config_hash.as_ref(),
                rename_budget,
                blob,
            ],
        )
        .map_err(|e| crate::Error::Git(format!("grouped_walk insert: {e}")))?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn grouped_walk_get_ancestor(
        &self,
        anchor: &str,
        cd: CopyDetection,
        seed_hash: &[u8],
        replace_refs_hash: &[u8],
        git_config_hash: &[u8],
        rename_budget: i64,
        head: &str,
        repo: &gix::Repository,
    ) -> Option<(String /* cached_head */, GroupedWalk)> {
        if !self.enabled {
            return None;
        }
        let cd_int = copy_detection_to_int(cd);
        // Query all candidate rows for this anchor + CopyDetection + key hashes.
        let mut stmt = self.conn.prepare(
            "SELECT head_sha, walk_blob FROM grouped_walk_cache \
             WHERE anchor_sha = ?1 AND copy_detection = ?2 \
               AND seed_hash = ?3 AND replace_refs_hash = ?4 \
               AND git_config_hash = ?5 AND rename_budget = ?6",
        ).ok()?;
        let rows: Vec<(String, Vec<u8>)> = stmt.query_map(
            rusqlite::params![anchor, cd_int, seed_hash, replace_refs_hash, git_config_hash, rename_budget],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, Vec<u8>>(1)?)),
        ).ok()?
        .filter_map(|r| r.ok())
        .collect();

        use std::str::FromStr;
        let head_oid = gix::ObjectId::from_str(head).ok()?;

        for (cached_head_sha, walk_blob) in rows {
            let cached_oid = match gix::ObjectId::from_str(&cached_head_sha) {
                Ok(id) => id,
                Err(_) => continue,
            };
            // Skip if cached_head == head (that's an exact hit, not ancestor).
            if cached_oid == head_oid {
                continue;
            }
            // Check: is cached_head an ancestor of head?
            // merge_base(A, B) == A means A is an ancestor of B.
            let is_ancestor = match repo.merge_base(cached_oid, head_oid) {
                Ok(base) => base.detach() == cached_oid,
                Err(_) => false,
            };
            if !is_ancestor {
                continue;
            }
            // Decode the walk; skip on failure.
            let walk = match bincode::deserialize::<GroupedWalk>(&walk_blob) {
                Ok(w) => w,
                Err(_) => continue,
            };
            return Some((cached_head_sha, walk));
        }
        None
    }

    pub(crate) fn grouped_walk_replace(
        &self,
        txn: &Transaction,
        old_head: Option<&str>,
        key: &GroupedWalkKey,
        walk: &GroupedWalk,
    ) -> Result<()> {
        if !self.enabled {
            return Ok(());
        }
        let cd_int = copy_detection_to_int(key.copy_detection);
        let rename_budget = key.rename_budget as i64;
        // DELETE old row if old_head is provided.
        if let Some(old_h) = old_head {
            txn.execute(
                "DELETE FROM grouped_walk_cache \
                 WHERE anchor_sha = ?1 AND copy_detection = ?2 \
                   AND seed_hash = ?3 AND replace_refs_hash = ?4 \
                   AND git_config_hash = ?5 AND rename_budget = ?6 \
                   AND head_sha = ?7",
                rusqlite::params![
                    key.anchor_sha,
                    cd_int,
                    key.seed_hash.as_ref(),
                    key.replace_refs_hash.as_ref(),
                    key.git_config_hash.as_ref(),
                    rename_budget,
                    old_h,
                ],
            )
            .map_err(|e| crate::Error::Git(format!("grouped_walk delete old: {e}")))?;
        }
        // INSERT new row.
        let blob = bincode::serialize(walk)
            .map_err(|e| crate::Error::Git(format!("bincode serialize grouped_walk: {e}")))?;
        txn.execute(
            "INSERT OR REPLACE INTO grouped_walk_cache \
             (anchor_sha, head_sha, copy_detection, seed_hash, replace_refs_hash, \
              git_config_hash, rename_budget, walk_blob) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            rusqlite::params![
                key.anchor_sha,
                key.head_sha,
                cd_int,
                key.seed_hash.as_ref(),
                key.replace_refs_hash.as_ref(),
                key.git_config_hash.as_ref(),
                rename_budget,
                blob,
            ],
        )
        .map_err(|e| crate::Error::Git(format!("grouped_walk insert: {e}")))?;
        Ok(())
    }

    // ── Tier 4: rename_trail ────────────────────────────────────────────────

    pub(crate) fn rename_trail_get(&self, key: &TrailCacheKey) -> Option<TrailCacheEntry> {
        if !self.enabled {
            return None;
        }
        let cd_int = copy_detection_to_int(key.copy_detection);
        let rename_budget = key.rename_budget as i64;
        let result: rusqlite::Result<Vec<u8>> = self.conn.query_row(
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
        );
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
        .map_err(|e| crate::Error::Git(format!("rename_trail insert: {e}")))?;
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
        // Primary key includes more columns; we identify dead rows by
        // anchor_sha + head_sha (the two commit-SHA columns).
        #[allow(clippy::type_complexity)]
        let dead_gw: Vec<(String, String, i32, Vec<u8>, Vec<u8>, Vec<u8>, i64)> = {
            let mut stmt = self.conn.prepare(
                "SELECT anchor_sha, head_sha, copy_detection, \
                        seed_hash, replace_refs_hash, git_config_hash, rename_budget \
                 FROM grouped_walk_cache",
            )
            .map_err(|e| crate::Error::Git(format!("gc: prepare grouped_walk scan: {e}")))?;
            stmt.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i32>(2)?,
                    row.get::<_, Vec<u8>>(3)?,
                    row.get::<_, Vec<u8>>(4)?,
                    row.get::<_, Vec<u8>>(5)?,
                    row.get::<_, i64>(6)?,
                ))
            })
            .map_err(|e| crate::Error::Git(format!("gc: grouped_walk scan: {e}")))?
            .filter_map(|r| r.ok())
            .filter(|(anchor, head, _, _, _, _, _)| {
                !live.contains(anchor) || !live.contains(head)
            })
            .collect()
        };

        let grouped_walk_removed = dead_gw.len();
        for chunk in dead_gw.chunks(5000) {
            let txn = Transaction::new_unchecked(&self.conn, rusqlite::TransactionBehavior::Immediate)
                .map_err(|e| crate::Error::Git(format!("gc: begin txn grouped_walk: {e}")))?;
            for (anchor, head, cd, seed, replace_refs, git_config, budget) in chunk {
                txn.execute(
                    "DELETE FROM grouped_walk_cache \
                     WHERE anchor_sha = ?1 AND head_sha = ?2 AND copy_detection = ?3 \
                       AND seed_hash = ?4 AND replace_refs_hash = ?5 \
                       AND git_config_hash = ?6 AND rename_budget = ?7",
                    rusqlite::params![anchor, head, cd, seed, replace_refs, git_config, budget],
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

        Ok(GcStats {
            name_status_removed,
            blob_diff_removed,
            grouped_walk_removed,
            rename_trail_removed,
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
) -> Result<Connection> {
    let conn = Connection::open_with_flags(db_path, flags)
        .map_err(|e| crate::Error::Git(format!("sqlite open: {e}")))?;

    apply_pragmas(&conn)?;

    let version: i32 = conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .map_err(|e| crate::Error::Git(format!("read user_version: {e}")))?;

    if version != 0 && version != SCHEMA_VERSION {
        // Version mismatch — drop and rebuild silently.
        drop(conn);
        let _ = fs::remove_file(db_path);
        let _ = fs::remove_file(db_dir.join("mesh_cache.sqlite-wal"));
        let _ = fs::remove_file(db_dir.join("mesh_cache.sqlite-shm"));

        let conn = Connection::open_with_flags(db_path, flags)
            .map_err(|e| crate::Error::Git(format!("sqlite reopen: {e}")))?;
        apply_pragmas(&conn)?;
        bootstrap_schema(&conn)?;
        return Ok(conn);
    }

    bootstrap_schema(&conn)?;
    Ok(conn)
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

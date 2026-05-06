//! SQLite-backed content-addressed cache for `git mesh stale`.
//!
//! Three tiers of caches share a single database at
//! `<git_dir>/mesh/cache/mesh_cache.sqlite`:
//!
//! - **Tier 1** — `name_status_cache`: per-commit-pair `Vec<NS>` blobs.
//! - **Tier 2** — `blob_diff_cache`: per-blob-pair hunk lists.
//! - **Tier 3** — `grouped_walk_cache`: full `GroupedWalk` materializations.
//!
//! Phase 1 establishes the contract: types, signatures, schema bootstrap,
//! and serde derives.  No tier probes are wired yet.

use crate::Result;
use crate::git;
use crate::resolver::session::GroupedWalk;
use crate::resolver::trail_cache::TrailCacheKey;
use crate::resolver::walker::NS;
use crate::types::CopyDetection;
use rusqlite::{Connection, OpenFlags, Transaction};
use std::fs;

pub const SCHEMA_VERSION: i32 = 1;

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
";

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
    pub(crate) fn from_trail_key(trail: &TrailCacheKey, head_sha: String) -> Self {
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
        _parent: &str,
        _commit: &str,
        _cd: CopyDetection,
    ) -> Option<Vec<NS>> {
        if !self.enabled {
            return None;
        }
        None
    }

    pub(crate) fn name_status_put_batch(
        &self,
        _txn: &Transaction,
        _rows: &[(&str, &str, CopyDetection, Vec<NS>)],
    ) -> Result<()> {
        Ok(())
    }

    // ── Tier 2: blob_diff ───────────────────────────────────────────────────

    pub(crate) fn blob_diff_get(
        &self,
        _old_blob: &str,
        _new_blob: &str,
    ) -> Option<Vec<(u32, u32, u32, u32)>> {
        if !self.enabled {
            return None;
        }
        None
    }

    pub(crate) fn blob_diff_put(
        &self,
        _txn: &Transaction,
        _old: &str,
        _new: &str,
        _hunks: &[(u32, u32, u32, u32)],
    ) -> Result<()> {
        Ok(())
    }

    // ── Tier 3: grouped_walk ────────────────────────────────────────────────

    pub(crate) fn grouped_walk_get_exact(&self, _key: &GroupedWalkKey) -> Option<GroupedWalk> {
        if !self.enabled {
            return None;
        }
        None
    }

    pub(crate) fn grouped_walk_get_ancestor(
        &self,
        _anchor: &str,
        _cd: CopyDetection,
        _head: &str,
        _repo: &gix::Repository,
    ) -> Option<(String /* cached_head */, GroupedWalk)> {
        if !self.enabled {
            return None;
        }
        None
    }

    pub(crate) fn grouped_walk_replace(
        &self,
        _txn: &Transaction,
        _old_head: Option<&str>,
        _key: &GroupedWalkKey,
        _walk: &GroupedWalk,
    ) -> Result<()> {
        Ok(())
    }

    // ── GC ──────────────────────────────────────────────────────────────────

    pub(crate) fn gc(&self, _repo: &gix::Repository) -> Result<GcStats> {
        Ok(GcStats::default())
    }
}

// ── internals ───────────────────────────────────────────────────────────────

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

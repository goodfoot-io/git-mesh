//! SQLite open + schema bootstrap for the Phase 3 cache.
//!
//! The database lives at `$GIT_COMMON_DIR/mesh/stale-cache.db` and is
//! opened in WAL mode with `synchronous=NORMAL`. WAL allows concurrent
//! readers and one writer; `synchronous=NORMAL` trades a tiny amount of
//! durability for higher throughput, which is acceptable for a cache
//! whose entries can always be rebuilt.

use crate::{Error, Result};
use rusqlite::Connection;
use std::path::{Path, PathBuf};

/// Database file basename under `<common_dir>/mesh/`.
pub(crate) const DB_BASENAME: &str = "stale-cache.db";

/// Resolve the cache database path for a repository. The parent
/// directory is created lazily by [`open_store`].
pub(crate) fn db_path(repo: &gix::Repository) -> PathBuf {
    crate::git::common_dir(repo).join("mesh").join(DB_BASENAME)
}

/// Open (or create) the Phase 3 cache database, applying the schema
/// and WAL pragmas. Returns a thin wrapper that can hand out the
/// underlying [`rusqlite::Connection`].
pub(crate) fn open_store(repo: &gix::Repository) -> Result<Phase3Store> {
    let path = db_path(repo);
    open_store_at(&path)
}

/// Like [`open_store`] but takes a raw filesystem path; used by tests
/// that don't want a full `gix::Repository`.
pub(crate) fn open_store_at(path: &Path) -> Result<Phase3Store> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| Error::Git(format!("create phase3 dir `{}`: {e}", parent.display())))?;
    }
    let conn = Connection::open(path)
        .map_err(|e| Error::Git(format!("open phase3 db `{}`: {e}", path.display())))?;
    // WAL + NORMAL synchronous, per the plan. `journal_mode` is a
    // query pragma that returns the new mode on success; we don't
    // assert on the return value because some filesystems (e.g.
    // network mounts) silently fall back to TRUNCATE, which is still
    // safe.
    conn.pragma_update(None, "journal_mode", "WAL")
        .map_err(|e| Error::Git(format!("set journal_mode=WAL: {e}")))?;
    conn.pragma_update(None, "synchronous", "NORMAL")
        .map_err(|e| Error::Git(format!("set synchronous=NORMAL: {e}")))?;
    // `busy_timeout` keeps concurrent writers from immediately failing
    // on `SQLITE_BUSY` while WAL hand-off completes. The cache is
    // best-effort, so 1s is plenty.
    conn.busy_timeout(std::time::Duration::from_millis(1_000))
        .map_err(|e| Error::Git(format!("set busy_timeout: {e}")))?;
    apply_schema(&conn)?;
    Ok(Phase3Store { conn, path: path.to_path_buf() })
}

fn apply_schema(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS path_anchor_index (
            catalog_tree_oid TEXT NOT NULL,
            key_salt         INTEGER NOT NULL,
            payload          BLOB NOT NULL,
            created_at       INTEGER NOT NULL,
            PRIMARY KEY (catalog_tree_oid, key_salt)
        );

        CREATE TABLE IF NOT EXISTS committed_baseline (
            catalog_tree_oid    TEXT NOT NULL,
            head_oid            TEXT NOT NULL,
            filter_config_hash  TEXT NOT NULL,
            key_salt            INTEGER NOT NULL,
            payload             BLOB NOT NULL,
            created_at          INTEGER NOT NULL,
            PRIMARY KEY (catalog_tree_oid, head_oid, filter_config_hash, key_salt)
        );

        CREATE TABLE IF NOT EXISTS dirty_overlay (
            overlay_key BLOB PRIMARY KEY,
            payload     BLOB NOT NULL,
            created_at  INTEGER NOT NULL
        );
        "#,
    )
    .map_err(|e| Error::Git(format!("apply phase3 schema: {e}")))?;
    Ok(())
}

/// Open Phase 3 cache handle.
pub(crate) struct Phase3Store {
    pub(crate) conn: Connection,
    pub(crate) path: PathBuf,
}

impl Phase3Store {
    /// Number of rows in each table — used by gc and tests.
    pub(crate) fn row_counts(&self) -> Result<RowCounts> {
        let q = |sql: &str| -> Result<i64> {
            self.conn
                .query_row(sql, [], |r| r.get::<_, i64>(0))
                .map_err(|e| Error::Git(format!("phase3 count: {e}")))
        };
        Ok(RowCounts {
            path_anchor_index: q("SELECT COUNT(*) FROM path_anchor_index")?,
            committed_baseline: q("SELECT COUNT(*) FROM committed_baseline")?,
            dirty_overlay: q("SELECT COUNT(*) FROM dirty_overlay")?,
        })
    }

    /// Best-effort gc that drops rows whose `key_salt` is not the
    /// current [`super::KEY_SALT`]. Rows in `dirty_overlay` carry the
    /// salt inside their digest, so they are pruned by age instead
    /// (older than `max_age_secs`).
    pub(crate) fn gc(&self, max_age_secs: i64) -> Result<()> {
        let salt = super::KEY_SALT as i64;
        self.conn
            .execute(
                "DELETE FROM path_anchor_index WHERE key_salt != ?1",
                [salt],
            )
            .map_err(|e| Error::Git(format!("phase3 gc path_anchor_index: {e}")))?;
        self.conn
            .execute(
                "DELETE FROM committed_baseline WHERE key_salt != ?1",
                [salt],
            )
            .map_err(|e| Error::Git(format!("phase3 gc committed_baseline: {e}")))?;
        let cutoff = now_secs() - max_age_secs.max(0);
        self.conn
            .execute(
                "DELETE FROM dirty_overlay WHERE created_at < ?1",
                [cutoff],
            )
            .map_err(|e| Error::Git(format!("phase3 gc dirty_overlay: {e}")))?;
        Ok(())
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct RowCounts {
    pub(crate) path_anchor_index: i64,
    pub(crate) committed_baseline: i64,
    pub(crate) dirty_overlay: i64,
}

pub(crate) fn now_secs() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

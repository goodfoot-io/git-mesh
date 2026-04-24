//! Session store: open, initialize, and version-guard the per-session SQLite DB.

use anyhow::{Context, Result, bail};
use rusqlite::Connection;
use std::path::PathBuf;

/// Expected schema version stored in `schema_meta`.
pub const SCHEMA_VERSION: i64 = 1;

/// Base directory for all session DB and audit files.
pub const SESSION_DIR: &str = "/tmp/git-mesh-claude-code";

/// Sanitize a session ID: replace any character that is not ASCII alphanumeric,
/// hyphen, dot, or underscore with `_`. Prevents path traversal and
/// shell-special characters from reaching the filesystem.
pub fn sanitize_session_id(id: &str) -> String {
    id.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || "-._".contains(c) {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Path of the `.db` file for `session_id` (already sanitized).
pub fn db_path(sanitized_id: &str) -> PathBuf {
    PathBuf::from(SESSION_DIR).join(format!("{sanitized_id}.db"))
}

/// Path of the `.jsonl` audit file for `session_id` (already sanitized).
pub fn jsonl_path(sanitized_id: &str) -> PathBuf {
    PathBuf::from(SESSION_DIR).join(format!("{sanitized_id}.jsonl"))
}

/// Open (or create) the session SQLite DB for `session_id`.
///
/// On first open: creates the directory with mode 0o700, creates the DB
/// file with mode 0o600, initializes the schema, and writes
/// `schema_meta(version=1)`.
///
/// On subsequent opens: verifies `schema_meta.version == SCHEMA_VERSION`.
/// Mismatch → loud error, no silent re-init (fail-closed: stale schema
/// producing wrong dedup is worse than a clear error).
pub fn open_store(session_id: &str) -> Result<Connection> {
    let sanitized = sanitize_session_id(session_id);
    let dir = PathBuf::from(SESSION_DIR);
    let path = db_path(&sanitized);

    ensure_session_dir(&dir)?;
    let conn = open_db_file(&path)?;
    init_or_verify_schema(&conn)?;
    Ok(conn)
}

fn ensure_session_dir(dir: &std::path::Path) -> Result<()> {
    use std::fs;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    if !dir.exists() {
        fs::create_dir_all(dir).with_context(|| format!("create dir {}", dir.display()))?;
    }
    #[cfg(unix)]
    {
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))
            .with_context(|| format!("set permissions on {}", dir.display()))?;
    }
    Ok(())
}

fn open_db_file(path: &std::path::Path) -> Result<Connection> {
    #[cfg(unix)]
    use std::os::unix::fs::OpenOptionsExt;

    // On Unix create the file with 0o600 before SQLite opens it, so the
    // DB is never world-readable even momentarily.
    #[cfg(unix)]
    {
        if !path.exists() {
            std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(false)
                .mode(0o600)
                .open(path)
                .with_context(|| format!("create db file {}", path.display()))?;
        }
    }

    let conn =
        Connection::open(path).with_context(|| format!("open db {}", path.display()))?;

    conn.execute_batch("PRAGMA journal_mode=WAL;")
        .context("set WAL mode")?;

    Ok(conn)
}

/// Exposed for tests in sibling modules.
#[cfg(test)]
pub fn init_or_verify_schema_pub(conn: &Connection) -> Result<()> {
    init_or_verify_schema(conn)
}

fn init_or_verify_schema(conn: &Connection) -> Result<()> {
    // Check whether schema_meta exists yet (first open vs re-open).
    let table_exists: bool = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='schema_meta'",
            [],
            |r| r.get::<_, i64>(0),
        )
        .context("check schema_meta table")?
        != 0;

    if table_exists {
        // Re-open: verify version, fail-closed on mismatch.
        let stored: i64 = conn
            .query_row("SELECT version FROM schema_meta LIMIT 1", [], |r| {
                r.get(0)
            })
            .context("read schema_meta version")?;
        if stored != SCHEMA_VERSION {
            bail!(
                "advice DB schema version mismatch: expected {SCHEMA_VERSION}, found {stored}. \
                 Remove the stale .db file and retry."
            );
        }
        return Ok(());
    }

    // First open: create all tables.
    conn.execute_batch(SCHEMA_DDL).context("initialize schema")?;
    conn.execute(
        "INSERT INTO schema_meta (version) VALUES (?1)",
        rusqlite::params![SCHEMA_VERSION],
    )
    .context("insert schema_meta")?;

    Ok(())
}

const SCHEMA_DDL: &str = "
CREATE TABLE IF NOT EXISTS schema_meta (version INTEGER NOT NULL);

CREATE TABLE IF NOT EXISTS events (
  id      INTEGER PRIMARY KEY,
  kind    TEXT    NOT NULL,
  ts      TEXT    NOT NULL,
  payload TEXT    NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_events_kind_ts ON events(kind, ts);

CREATE TABLE IF NOT EXISTS read_events (
  event_id   INTEGER PRIMARY KEY,
  path       TEXT,
  start_line INTEGER,
  end_line   INTEGER
);

CREATE TABLE IF NOT EXISTS write_events (
  event_id   INTEGER PRIMARY KEY,
  path       TEXT,
  start_line INTEGER,
  end_line   INTEGER,
  pre_blob   TEXT,
  post_blob  TEXT
);

CREATE TABLE IF NOT EXISTS commit_events (
  event_id INTEGER PRIMARY KEY,
  sha      TEXT
);

CREATE TABLE IF NOT EXISTS snapshot_events (
  event_id  INTEGER PRIMARY KEY,
  tree_sha  TEXT,
  index_sha TEXT
);

CREATE TABLE IF NOT EXISTS flush_events (
  event_id   INTEGER PRIMARY KEY,
  output_sha TEXT
);

CREATE TABLE IF NOT EXISTS flush_additions (
  flush_event_id INTEGER NOT NULL REFERENCES flush_events(event_id),
  mesh           TEXT    NOT NULL,
  reason_kind    TEXT    NOT NULL,
  range_path     TEXT    NOT NULL,
  start_line     INTEGER,
  end_line       INTEGER,
  trigger_path   TEXT    NOT NULL DEFAULT '',
  PRIMARY KEY (mesh, reason_kind, range_path, start_line, end_line, trigger_path)
);

CREATE TABLE IF NOT EXISTS flush_doc_topics (
  flush_event_id INTEGER NOT NULL REFERENCES flush_events(event_id),
  doc_topic      TEXT    NOT NULL,
  PRIMARY KEY (doc_topic)
);

CREATE TABLE IF NOT EXISTS mesh_ranges (
  mesh       TEXT    NOT NULL,
  path       TEXT    NOT NULL,
  start_line INTEGER,
  end_line   INTEGER,
  status     TEXT,
  source     TEXT,
  ack        INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS idx_mesh_ranges_path ON mesh_ranges(path);
CREATE INDEX IF NOT EXISTS idx_mesh_ranges_mesh ON mesh_ranges(mesh);
";

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use tempfile::TempDir;

    /// Override SESSION_DIR for tests by opening directly via path.
    fn open_store_at(dir: &Path, session_id: &str) -> Result<Connection> {
        let sanitized = sanitize_session_id(session_id);
        ensure_session_dir(dir)?;
        let path = dir.join(format!("{sanitized}.db"));
        let conn = open_db_file(&path)?;
        init_or_verify_schema(&conn)?;
        Ok(conn)
    }

    #[test]
    fn store_opens_and_creates_dir_and_file() {
        let td = TempDir::new().unwrap();
        let dir = td.path().join("sessions");
        let conn = open_store_at(&dir, "test-session").unwrap();
        assert!(dir.join("test-session.db").exists());
        // Verify schema_meta row present.
        let v: i64 = conn
            .query_row("SELECT version FROM schema_meta", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, SCHEMA_VERSION);
    }

    #[cfg(unix)]
    #[test]
    fn dir_has_correct_mode() {
        use std::os::unix::fs::PermissionsExt;
        let td = TempDir::new().unwrap();
        let dir = td.path().join("sessions");
        open_store_at(&dir, "s").unwrap();
        let mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700, "dir mode should be 0o700, got {mode:o}");
    }

    #[cfg(unix)]
    #[test]
    fn db_file_has_correct_mode() {
        use std::os::unix::fs::PermissionsExt;
        let td = TempDir::new().unwrap();
        let dir = td.path().join("sessions");
        open_store_at(&dir, "s").unwrap();
        let db = dir.join("s.db");
        let mode = std::fs::metadata(&db).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "db file mode should be 0o600, got {mode:o}");
    }

    #[test]
    fn schema_version_mismatch_is_loud_error() {
        let td = TempDir::new().unwrap();
        let dir = td.path().join("sessions");
        let conn = open_store_at(&dir, "s").unwrap();
        // Corrupt the schema version.
        conn.execute("UPDATE schema_meta SET version = 99", []).unwrap();
        drop(conn);
        let err = open_store_at(&dir, "s").unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("schema version mismatch"),
            "expected version mismatch error, got: {msg}"
        );
    }

    #[test]
    fn session_id_sanitization() {
        assert_eq!(sanitize_session_id("abc-123"), "abc-123");
        // '.' is allowed; '/' and extra '.' in path traversal attempts become '_'
        assert_eq!(sanitize_session_id("../evil"), ".._evil");
        assert_eq!(sanitize_session_id("a/b"), "a_b");
        assert_eq!(sanitize_session_id("a b"), "a_b");
        assert_eq!(sanitize_session_id("ok.fine_here-too"), "ok.fine_here-too");
    }

    #[test]
    fn second_open_same_schema_succeeds() {
        let td = TempDir::new().unwrap();
        let dir = td.path().join("sessions");
        open_store_at(&dir, "s").unwrap();
        // Second open should succeed without re-initializing.
        open_store_at(&dir, "s").unwrap();
    }
}

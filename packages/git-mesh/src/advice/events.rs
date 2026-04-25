//! Typed event append helpers for the advice session store.
//!
//! Slice 2: the canonical `payload` JSON for every event is built once,
//! stored verbatim in `events.payload`, and returned to the caller as an
//! [`AuditRecord`] so the CLI can append a byte-identical JSON object
//! to the audit log. Per `docs/advice-notes.md` §7, the audit line is
//! `{"id": …, "kind": …, "ts": …, "payload": <same object>}` and the
//! audit log is rebuildable from the SQL store.

use anyhow::{Context, Result};
use rusqlite::Connection;
use serde_json::{Value, json};
use std::path::Path;

use crate::git;

/// Maximum size, in bytes, of a `--pre` or `--post` content blob. Larger
/// writes are out of advice's scope: the heuristics that consume blobs
/// operate on small ranges, and the audit line must stay tractable.
pub const CONTENT_BYTE_CAP: usize = 1024 * 1024; // 1 MiB

/// Snapshot of a single event suitable for audit-log emission.
///
/// `payload` is the *same* JSON value that was stored in `events.payload`;
/// the audit line is a strict superset (`id`/`kind`/`ts` plus this object).
#[derive(Debug)]
pub struct AuditRecord {
    pub id: i64,
    pub kind: &'static str,
    pub ts: String,
    pub payload: Value,
}

fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339()
}

/// Insert one row into `events`, returning the new id and the timestamp
/// that was stored. Caller is responsible for inserting into the per-kind
/// flat table and emitting the audit line.
fn insert_event(conn: &Connection, kind: &str, payload: &Value) -> Result<(i64, String)> {
    let ts = now_rfc3339();
    let payload_str = payload.to_string();
    conn.execute(
        "INSERT INTO events (kind, ts, payload) VALUES (?1, ?2, ?3)",
        rusqlite::params![kind, ts, payload_str],
    )
    .context("insert event")?;
    Ok((conn.last_insert_rowid(), ts))
}

/// Normalize `path` to a repo-relative string by stripping the repo's
/// working directory prefix. Rejects paths outside the repo.
fn normalize_path(repo: &gix::Repository, path: &str) -> Result<String> {
    let p = Path::new(path);
    if p.is_absolute() {
        let wd = repo
            .workdir()
            .ok_or_else(|| anyhow::anyhow!("bare repository not supported"))?;
        let rel = p.strip_prefix(wd).map_err(|_| {
            anyhow::anyhow!(
                "path `{}` is outside the repository at `{}`",
                path,
                wd.display()
            )
        })?;
        Ok(rel.to_string_lossy().into_owned())
    } else {
        Ok(path.to_string())
    }
}

/// Parse an optional `#Ls-Le` suffix from a path spec like `foo.ts#L1-L10`.
fn parse_path_range(spec: &str) -> (&str, Option<(i64, i64)>) {
    if let Some(hash_pos) = spec.rfind('#') {
        let (path, frag) = spec.split_at(hash_pos);
        let frag = &frag[1..]; // strip '#'
        let frag = frag.strip_prefix('L').unwrap_or(frag);
        if let Some((s, e)) = frag.split_once('-') {
            let s = s.strip_prefix('L').unwrap_or(s);
            let e = e.strip_prefix('L').unwrap_or(e);
            if let (Ok(sl), Ok(el)) = (s.parse::<i64>(), e.parse::<i64>()) {
                return (path, Some((sl, el)));
            }
        }
    }
    (spec, None)
}

// ---------------------------------------------------------------------------
// Public append helpers
// ---------------------------------------------------------------------------

/// Append a `read` event. Returns the audit record so the CLI can mirror
/// the same payload to the JSONL.
pub fn append_read(
    conn: &Connection,
    repo: &gix::Repository,
    spec: &str,
) -> Result<AuditRecord> {
    let (raw_path, range) = parse_path_range(spec);
    let path = normalize_path(repo, raw_path)?;
    let (start_line, end_line) = range
        .map(|(s, e)| (Some(s), Some(e)))
        .unwrap_or((None, None));

    let payload = json!({
        "end_line": end_line,
        "path": path,
        "start_line": start_line,
    });
    let (event_id, ts) = insert_event(conn, "read", &payload)?;
    conn.execute(
        "INSERT INTO read_events (event_id, path, start_line, end_line) VALUES (?1, ?2, ?3, ?4)",
        rusqlite::params![event_id, path, start_line, end_line],
    )
    .context("insert read_event")?;
    Ok(AuditRecord {
        id: event_id,
        kind: "read",
        ts,
        payload,
    })
}

/// Append a `write` event with explicit pre/post content (slice 2).
///
/// Both `pre` and `post` are optional. Each, when supplied, must be valid
/// UTF-8 and at most [`CONTENT_BYTE_CAP`] bytes; binary or oversize input
/// is rejected at the CLI boundary, so this helper trusts the caller.
///
/// The same `path`, `start_line`, `end_line`, `pre_blob`, `post_blob`
/// payload is written to `events.payload`, the per-kind flat table, and
/// the audit line — they are byte-identical by construction.
pub fn append_write(
    conn: &Connection,
    repo: &gix::Repository,
    spec: &str,
    pre: Option<String>,
    post: Option<String>,
) -> Result<AuditRecord> {
    let (raw_path, range) = parse_path_range(spec);
    let path = normalize_path(repo, raw_path)?;
    let (start_line, end_line) = range
        .map(|(s, e)| (Some(s), Some(e)))
        .unwrap_or((None, None));

    let payload = json!({
        "end_line": end_line,
        "path": path,
        "post_blob": post,
        "pre_blob": pre,
        "start_line": start_line,
    });
    let (event_id, ts) = insert_event(conn, "write", &payload)?;
    conn.execute(
        "INSERT INTO write_events (event_id, path, start_line, end_line, pre_blob, post_blob) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        rusqlite::params![event_id, path, start_line, end_line, pre, post],
    )
    .context("insert write_event")?;
    Ok(AuditRecord {
        id: event_id,
        kind: "write",
        ts,
        payload,
    })
}

/// Append a `commit` event. Validates the SHA via gix — fails if unresolvable.
pub fn append_commit(
    conn: &Connection,
    repo: &gix::Repository,
    sha: &str,
) -> Result<AuditRecord> {
    let resolved = repo
        .rev_parse_single(sha)
        .map_err(|_| anyhow::anyhow!("commit `{sha}` not found in the object database"))?
        .detach()
        .to_string();

    let payload = json!({ "sha": resolved });
    let (event_id, ts) = insert_event(conn, "commit", &payload)?;
    conn.execute(
        "INSERT INTO commit_events (event_id, sha) VALUES (?1, ?2)",
        rusqlite::params![event_id, resolved],
    )
    .context("insert commit_event")?;
    Ok(AuditRecord {
        id: event_id,
        kind: "commit",
        ts,
        payload,
    })
}

/// Append a `snapshot` event. Computes `tree_sha` and `index_sha` via gix.
pub fn append_snapshot(conn: &Connection, repo: &gix::Repository) -> Result<AuditRecord> {
    let tree_sha = repo
        .head_commit()
        .ok()
        .and_then(|c| c.tree_id().ok())
        .map(|id| id.detach().to_string());

    let index_sha = repo
        .open_index()
        .ok()
        .and_then(|idx| idx.checksum().map(|c| c.to_string()));

    let payload = json!({
        "index_sha": index_sha,
        "tree_sha": tree_sha,
    });
    let (event_id, ts) = insert_event(conn, "snapshot", &payload)?;
    conn.execute(
        "INSERT INTO snapshot_events (event_id, tree_sha, index_sha) VALUES (?1, ?2, ?3)",
        rusqlite::params![event_id, tree_sha, index_sha],
    )
    .context("insert snapshot_event")?;
    Ok(AuditRecord {
        id: event_id,
        kind: "snapshot",
        ts,
        payload,
    })
}

// ---------------------------------------------------------------------------
// Internal blob helpers (no longer used by `append_write`; kept available
// for the prior-write fallback heuristic if a future caller wants it).
// ---------------------------------------------------------------------------

#[allow(dead_code)]
fn read_worktree_text(repo: &gix::Repository, rel_path: &str) -> Option<String> {
    git::read_worktree_bytes(repo, rel_path)
        .ok()
        .and_then(|b| String::from_utf8(b).ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::advice::db::init_or_verify_schema_pub;
    use std::process::Command;
    use tempfile::TempDir;

    fn seed_repo(td: &TempDir) -> gix::Repository {
        let dir = td.path();
        let run = |args: &[&str]| {
            Command::new("git")
                .current_dir(dir)
                .args(args)
                .output()
                .unwrap();
        };
        run(&["init", "--initial-branch=main"]);
        run(&["config", "user.email", "t@t"]);
        run(&["config", "user.name", "t"]);
        run(&["config", "commit.gpgsign", "false"]);
        std::fs::write(dir.join("foo.txt"), "line1\nline2\nline3\n").unwrap();
        run(&["add", "."]);
        run(&["commit", "-m", "init"]);
        gix::open(dir).unwrap()
    }

    fn open_test_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        init_or_verify_schema_pub(&conn).unwrap();
        conn
    }

    #[test]
    fn read_event_appended() {
        let td = TempDir::new().unwrap();
        let repo = seed_repo(&td);
        let conn = open_test_conn();

        let rec = append_read(&conn, &repo, "foo.txt#L1-L3").unwrap();
        assert_eq!(rec.kind, "read");
        assert_eq!(rec.payload["path"], "foo.txt");
        assert_eq!(rec.payload["start_line"], 1);
        assert_eq!(rec.payload["end_line"], 3);

        // SQL `events.payload` matches `rec.payload` byte-for-byte.
        let stored: String = conn
            .query_row("SELECT payload FROM events WHERE id = ?1", [rec.id], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(stored, rec.payload.to_string());
    }

    #[test]
    fn write_event_with_explicit_content() {
        let td = TempDir::new().unwrap();
        let repo = seed_repo(&td);
        let conn = open_test_conn();

        let pre = Some("a\n".to_string());
        let post = Some("a\nb\n".to_string());
        let rec = append_write(&conn, &repo, "foo.txt#L1-L2", pre.clone(), post.clone()).unwrap();
        assert_eq!(rec.payload["pre_blob"], "a\n");
        assert_eq!(rec.payload["post_blob"], "a\nb\n");

        let (sql_pre, sql_post): (Option<String>, Option<String>) = conn
            .query_row(
                "SELECT pre_blob, post_blob FROM write_events",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(sql_pre, pre);
        assert_eq!(sql_post, post);

        // events.payload byte-equals payload.
        let stored: String = conn
            .query_row("SELECT payload FROM events WHERE id=?1", [rec.id], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(stored, rec.payload.to_string());
    }

    #[test]
    fn write_event_without_content_stores_null() {
        let td = TempDir::new().unwrap();
        let repo = seed_repo(&td);
        let conn = open_test_conn();

        let rec = append_write(&conn, &repo, "foo.txt", None, None).unwrap();
        assert!(rec.payload["pre_blob"].is_null());
        assert!(rec.payload["post_blob"].is_null());
    }

    #[test]
    fn commit_event_resolves_sha() {
        let td = TempDir::new().unwrap();
        let repo = seed_repo(&td);
        let conn = open_test_conn();

        let head = repo.head_id().unwrap().detach().to_string();
        let rec = append_commit(&conn, &repo, &head).unwrap();
        assert_eq!(rec.payload["sha"], head);
    }

    #[test]
    fn commit_event_rejects_bad_sha() {
        let td = TempDir::new().unwrap();
        let repo = seed_repo(&td);
        let conn = open_test_conn();

        let err = append_commit(&conn, &repo, "deadbeef").unwrap_err();
        assert!(
            err.to_string().contains("not found"),
            "expected 'not found' error, got: {err}"
        );
    }

    #[test]
    fn snapshot_event_includes_shas_in_payload() {
        let td = TempDir::new().unwrap();
        let repo = seed_repo(&td);
        let conn = open_test_conn();

        let rec = append_snapshot(&conn, &repo).unwrap();
        assert!(rec.payload.get("tree_sha").is_some());
        assert!(rec.payload.get("index_sha").is_some());
    }
}

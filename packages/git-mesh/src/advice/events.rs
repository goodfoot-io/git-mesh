//! Typed event append helpers for the advice session store.

use anyhow::{Context, Result};
use rusqlite::Connection;
use serde_json::{Value, json};
use std::path::Path;

use crate::git;

const BLOB_CAP: usize = 65_536; // 64 KiB

/// Cap a string to BLOB_CAP bytes at a UTF-8 character boundary.
fn cap_blob(s: String) -> String {
    if s.len() <= BLOB_CAP {
        s
    } else {
        // Truncate at a char boundary.
        let mut end = BLOB_CAP;
        while !s.is_char_boundary(end) {
            end -= 1;
        }
        s[..end].to_string()
    }
}

/// Returns `Some(text)` if `bytes` are valid UTF-8; `None` for binary.
fn decode_utf8(bytes: Vec<u8>) -> Option<String> {
    std::str::from_utf8(&bytes).ok().map(str::to_string)
}

fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339()
}

fn insert_event(conn: &Connection, kind: &str, payload: &Value) -> Result<i64> {
    let ts = now_rfc3339();
    let payload_str = payload.to_string();
    conn.execute(
        "INSERT INTO events (kind, ts, payload) VALUES (?1, ?2, ?3)",
        rusqlite::params![kind, ts, payload_str],
    )
    .context("insert event")?;
    Ok(conn.last_insert_rowid())
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
        // Accept `L1-L10` or `1-10`.
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

/// Append a `read` event.
pub fn append_read(conn: &Connection, repo: &gix::Repository, spec: &str) -> Result<()> {
    let (raw_path, range) = parse_path_range(spec);
    let path = normalize_path(repo, raw_path)?;
    let (start_line, end_line) = range
        .map(|(s, e)| (Some(s), Some(e)))
        .unwrap_or((None, None));

    let payload = json!({
        "path": path,
        "start_line": start_line,
        "end_line": end_line,
    });
    let event_id = insert_event(conn, "read", &payload)?;
    conn.execute(
        "INSERT INTO read_events (event_id, path, start_line, end_line) VALUES (?1, ?2, ?3, ?4)",
        rusqlite::params![event_id, path, start_line, end_line],
    )
    .context("insert read_event")?;
    Ok(())
}

/// Append a `write` event.
///
/// `post_blob` is read from the worktree at invocation time; `pre_blob` is
/// the most recent prior `write_events.post_blob` for this path in this
/// session (per-edit baseline), falling back to HEAD's blob when no prior
/// write exists.  Both are capped at 64 KiB.  Binary content → NULL.
pub fn append_write(conn: &Connection, repo: &gix::Repository, spec: &str) -> Result<()> {
    let (raw_path, range) = parse_path_range(spec);
    let path = normalize_path(repo, raw_path)?;
    let (start_line, end_line) = range
        .map(|(s, e)| (Some(s), Some(e)))
        .unwrap_or((None, None));

    // post_blob: current worktree content.
    let post_blob = read_worktree_blob(repo, &path);

    // pre_blob: prefer most recent prior write's post_blob for same path
    // (per-edit baseline); fall back to HEAD blob.
    // prior_write_post_blob returns Some(Option<String>) when a prior write
    // exists (the inner Option is the blob value, which may be NULL).
    // Fall back to HEAD blob only when no prior write row was found at all.
    let pre_blob = match prior_write_post_blob(conn, &path)? {
        Some(inner) => inner,
        None => head_blob(repo, &path),
    };

    // Payload omits blob fields (DB is source of truth; JSONL lines must
    // stay under PIPE_BUF for O_APPEND atomicity).
    let payload = json!({
        "path": path,
        "start_line": start_line,
        "end_line": end_line,
    });
    let event_id = insert_event(conn, "write", &payload)?;
    conn.execute(
        "INSERT INTO write_events (event_id, path, start_line, end_line, pre_blob, post_blob) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        rusqlite::params![event_id, path, start_line, end_line, pre_blob, post_blob],
    )
    .context("insert write_event")?;
    Ok(())
}

/// Append a `commit` event. Validates the SHA via gix — fails if unresolvable.
pub fn append_commit(conn: &Connection, repo: &gix::Repository, sha: &str) -> Result<()> {
    let resolved = repo
        .rev_parse_single(sha)
        .map_err(|_| anyhow::anyhow!("commit `{sha}` not found in the object database"))?
        .detach()
        .to_string();

    let payload = json!({ "sha": resolved });
    let event_id = insert_event(conn, "commit", &payload)?;
    conn.execute(
        "INSERT INTO commit_events (event_id, sha) VALUES (?1, ?2)",
        rusqlite::params![event_id, resolved],
    )
    .context("insert commit_event")?;
    Ok(())
}

/// Append a `snapshot` event. Computes `tree_sha` and `index_sha` via gix.
pub fn append_snapshot(conn: &Connection, repo: &gix::Repository) -> Result<()> {
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
        "tree_sha": tree_sha,
        "index_sha": index_sha,
    });
    let event_id = insert_event(conn, "snapshot", &payload)?;
    conn.execute(
        "INSERT INTO snapshot_events (event_id, tree_sha, index_sha) VALUES (?1, ?2, ?3)",
        rusqlite::params![event_id, tree_sha, index_sha],
    )
    .context("insert snapshot_event")?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Internal blob helpers
// ---------------------------------------------------------------------------

fn read_worktree_blob(repo: &gix::Repository, rel_path: &str) -> Option<String> {
    git::read_worktree_bytes(repo, rel_path)
        .ok()
        .and_then(decode_utf8)
        .map(cap_blob)
}

fn head_blob(repo: &gix::Repository, rel_path: &str) -> Option<String> {
    let head = repo.head_commit().ok()?;
    let head_oid = head.id.to_string();
    let blob_oid = git::path_blob_at(repo, &head_oid, rel_path).ok()?;
    git::read_git_text(repo, &blob_oid)
        .ok()
        .map(cap_blob)
        // fall back to None on binary (read_git_text already does UTF-8 check)
}

/// Query the most recent `post_blob` for `path` among prior write events in
/// this session. Returns `None` when no prior write exists.
fn prior_write_post_blob(conn: &Connection, path: &str) -> Result<Option<Option<String>>> {
    let mut stmt = conn.prepare(
        "SELECT post_blob FROM write_events \
         INNER JOIN events ON write_events.event_id = events.id \
         WHERE write_events.path = ?1 \
         ORDER BY write_events.event_id DESC LIMIT 1",
    )?;
    let mut rows = stmt.query(rusqlite::params![path])?;
    if let Some(row) = rows.next()? {
        let val: Option<String> = row.get(0)?;
        Ok(Some(val))
    } else {
        Ok(None)
    }
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

        append_read(&conn, &repo, "foo.txt#L1-L3").unwrap();

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM read_events", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1);
        let (path, sl, el): (String, Option<i64>, Option<i64>) = conn
            .query_row(
                "SELECT path, start_line, end_line FROM read_events",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(path, "foo.txt");
        assert_eq!(sl, Some(1));
        assert_eq!(el, Some(3));
    }

    #[test]
    fn write_event_appended_with_blobs() {
        let td = TempDir::new().unwrap();
        let repo = seed_repo(&td);
        let conn = open_test_conn();

        append_write(&conn, &repo, "foo.txt").unwrap();

        let (post, pre): (Option<String>, Option<String>) = conn
            .query_row(
                "SELECT post_blob, pre_blob FROM write_events",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        // post_blob should be the worktree content.
        assert!(post.is_some(), "post_blob should be set");
        // pre_blob comes from HEAD (first write in session).
        assert!(pre.is_some(), "pre_blob should be HEAD content");
    }

    #[test]
    fn second_write_uses_prior_post_blob_as_pre() {
        let td = TempDir::new().unwrap();
        let repo = seed_repo(&td);
        let conn = open_test_conn();

        // First write.
        append_write(&conn, &repo, "foo.txt").unwrap();

        // Modify the worktree file.
        std::fs::write(td.path().join("foo.txt"), "changed\n").unwrap();

        // Second write: pre_blob should equal first write's post_blob.
        append_write(&conn, &repo, "foo.txt").unwrap();

        let rows: Vec<(Option<String>, Option<String>)> = {
            let mut stmt = conn
                .prepare("SELECT pre_blob, post_blob FROM write_events ORDER BY event_id")
                .unwrap();
            stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
                .unwrap()
                .map(|r| r.unwrap())
                .collect()
        };
        assert_eq!(rows.len(), 2);
        let (_, first_post) = &rows[0];
        let (second_pre, _) = &rows[1];
        assert_eq!(
            second_pre, first_post,
            "second write's pre_blob should equal first write's post_blob"
        );
    }

    #[test]
    fn commit_event_resolves_sha() {
        let td = TempDir::new().unwrap();
        let repo = seed_repo(&td);
        let conn = open_test_conn();

        let head = repo.head_id().unwrap().detach().to_string();
        append_commit(&conn, &repo, &head).unwrap();

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM commit_events", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1);
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
    fn snapshot_event_appended() {
        let td = TempDir::new().unwrap();
        let repo = seed_repo(&td);
        let conn = open_test_conn();

        append_snapshot(&conn, &repo).unwrap();

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM snapshot_events", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn blob_capped_at_64kib() {
        let td = TempDir::new().unwrap();
        let repo = seed_repo(&td);
        let conn = open_test_conn();

        // Write a 200 KiB file.
        let big = "x".repeat(200 * 1024);
        std::fs::write(td.path().join("big.txt"), &big).unwrap();

        append_write(&conn, &repo, "big.txt").unwrap();

        let post: Option<String> = conn
            .query_row("SELECT post_blob FROM write_events", [], |r| r.get(0))
            .unwrap();
        let len = post.map(|s| s.len()).unwrap_or(0);
        assert!(
            len <= BLOB_CAP,
            "post_blob should be capped at {BLOB_CAP} bytes, got {len}"
        );
    }

    #[test]
    fn binary_file_blobs_are_null() {
        let td = TempDir::new().unwrap();
        let repo = seed_repo(&td);
        let conn = open_test_conn();

        // Write a binary (non-UTF-8) file.
        let binary: Vec<u8> = vec![0xFF, 0xFE, 0x00, 0x01, 0x80];
        std::fs::write(td.path().join("bin.bin"), &binary).unwrap();

        append_write(&conn, &repo, "bin.bin").unwrap();

        let (pre, post): (Option<String>, Option<String>) = conn
            .query_row(
                "SELECT pre_blob, post_blob FROM write_events",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert!(pre.is_none(), "binary pre_blob should be NULL");
        assert!(post.is_none(), "binary post_blob should be NULL");
    }
}

//! Slice 2 — JSONL audit log mirrors `events.payload`, `--pre`/`--post`
//! flags accept content for write events, and `--rebuild-audit-from-db`
//! regenerates the audit log deterministically from SQL.

mod support;

use anyhow::Result;
use serde_json::Value;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use support::TestRepo;
use uuid::Uuid;

const SESSION_DIR: &str = "/tmp/git-mesh-claude-code";

struct Session {
    id: String,
}

impl Session {
    fn new(prefix: &str) -> Self {
        let id = format!("slice2-{prefix}-{}", Uuid::new_v4());
        let s = Self { id };
        s.cleanup();
        s
    }
    fn db_path(&self) -> PathBuf {
        PathBuf::from(SESSION_DIR).join(format!("{}.db", self.id))
    }
    fn jsonl_path(&self) -> PathBuf {
        PathBuf::from(SESSION_DIR).join(format!("{}.jsonl", self.id))
    }
    fn cleanup(&self) {
        let _ = std::fs::remove_file(self.db_path());
        let _ = std::fs::remove_file(self.db_path().with_extension("db-wal"));
        let _ = std::fs::remove_file(self.db_path().with_extension("db-shm"));
        let _ = std::fs::remove_file(self.jsonl_path());
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        self.cleanup();
    }
}

fn run_advice(repo: &TestRepo, session: &Session, extra: &[&str]) -> Result<Output> {
    let mut args: Vec<String> = vec!["advice".into(), session.id.clone()];
    for a in extra {
        args.push((*a).into());
    }
    repo.run_mesh(args)
}

fn assert_ok(out: &Output) {
    assert!(
        out.status.success(),
        "expected success, code={:?} stderr={} stdout={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&out.stdout)
    );
}

fn read_jsonl(session: &Session) -> Vec<Value> {
    let s = std::fs::read_to_string(session.jsonl_path()).unwrap_or_default();
    s.lines()
        .map(|l| serde_json::from_str::<Value>(l).expect("valid jsonl line"))
        .collect()
}

fn read_sql_events(session: &Session) -> Vec<(i64, String, String, String)> {
    let conn = rusqlite::Connection::open(session.db_path()).unwrap();
    let mut stmt = conn
        .prepare("SELECT id, kind, ts, payload FROM events ORDER BY id ASC")
        .unwrap();
    let rows = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
            ))
        })
        .unwrap();
    rows.map(|r| r.unwrap()).collect()
}

// ---------------------------------------------------------------------------
// (A) audit line shape: id/kind/ts/payload, payload byte-equal to SQL.
// ---------------------------------------------------------------------------

#[test]
fn audit_line_mirrors_sql_payload_for_each_kind() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let session = Session::new("mirror");

    // read
    assert_ok(&run_advice(&repo, &session, &["add", "--read", "file1.txt#L1-L3"])?);
    // write without content
    assert_ok(&run_advice(&repo, &session, &["add", "--write", "file2.txt"])?);
    // write with content
    let pre = repo.path().join(".pre");
    let post = repo.path().join(".post");
    std::fs::write(&pre, "alpha\nbeta\n")?;
    std::fs::write(&post, "alpha\nBETA\n")?;
    assert_ok(&run_advice(
        &repo,
        &session,
        &[
            "add", "--write", "file2.txt#L1-L2", "--pre",
            pre.to_str().unwrap(), "--post", post.to_str().unwrap(),
        ],
    )?);
    // commit
    let head = repo.head_sha()?;
    assert_ok(&run_advice(&repo, &session, &["add", "--commit", &head])?);
    // snapshot
    assert_ok(&run_advice(&repo, &session, &["add", "--snapshot"])?);
    // flush (records its own event)
    assert_ok(&run_advice(&repo, &session, &[])?);

    let lines = read_jsonl(&session);
    let sql = read_sql_events(&session);

    assert_eq!(
        lines.len(),
        sql.len(),
        "one audit line per SQL event\nsql={sql:#?}\nlines={lines:#?}"
    );

    for (i, (sid, skind, sts, spayload)) in sql.iter().enumerate() {
        let line = &lines[i];
        // top-level shape
        assert_eq!(line["id"], serde_json::json!(*sid), "line {i} id mismatch");
        assert_eq!(line["kind"], serde_json::json!(skind), "line {i} kind mismatch");
        assert_eq!(line["ts"], serde_json::json!(sts), "line {i} ts mismatch");
        // payload byte-for-byte equality
        let line_payload_str = line["payload"].to_string();
        assert_eq!(
            line_payload_str, *spayload,
            "line {i} ({skind}) payload bytes diverge:\nsql=    {spayload}\nline=   {line_payload_str}"
        );
        // ts parses as RFC3339
        chrono::DateTime::parse_from_rfc3339(sts)
            .unwrap_or_else(|e| panic!("ts {sts} not RFC3339: {e}"));
    }

    // Per-kind payload requirements.
    let by_kind = |k: &str| -> &Value {
        &lines
            .iter()
            .find(|l| l["kind"] == k)
            .unwrap_or_else(|| panic!("no {k} line: {lines:#?}"))
            ["payload"]
    };
    let read_p = by_kind("read");
    assert_eq!(read_p["path"], "file1.txt");
    assert_eq!(read_p["start_line"], 1);
    assert_eq!(read_p["end_line"], 3);

    // Two writes; check the one with explicit content (file2.txt#L1-L2).
    let write_with_content = lines
        .iter()
        .find(|l| l["kind"] == "write" && l["payload"]["pre_blob"] != Value::Null)
        .expect("a write with pre_blob");
    let wp = &write_with_content["payload"];
    assert_eq!(wp["path"], "file2.txt");
    assert_eq!(wp["pre_blob"], "alpha\nbeta\n");
    assert_eq!(wp["post_blob"], "alpha\nBETA\n");

    let write_no_content = lines
        .iter()
        .find(|l| l["kind"] == "write" && l["payload"]["pre_blob"] == Value::Null)
        .expect("a write with null pre_blob");
    assert_eq!(write_no_content["payload"]["post_blob"], Value::Null);

    let snap_p = by_kind("snapshot");
    assert!(snap_p.get("tree_sha").is_some(), "snapshot tree_sha missing");
    assert!(snap_p.get("index_sha").is_some(), "snapshot index_sha missing");

    let flush_p = by_kind("flush");
    assert_eq!(flush_p["documentation"], false);
    assert!(flush_p.get("output_sha").is_some(), "flush output_sha missing");
    assert!(flush_p.get("output_len").is_some(), "flush output_len missing");

    Ok(())
}

// ---------------------------------------------------------------------------
// (A) round-trip: live JSONL == rebuilt-from-DB JSONL (byte equal).
// ---------------------------------------------------------------------------

#[test]
fn audit_log_round_trips_via_rebuild() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let session = Session::new("roundtrip");

    // Drive a small session.
    assert_ok(&run_advice(&repo, &session, &["add", "--read", "file1.txt"])?);
    assert_ok(&run_advice(&repo, &session, &["add", "--write", "file2.txt"])?);
    let head = repo.head_sha()?;
    assert_ok(&run_advice(&repo, &session, &["add", "--commit", &head])?);
    assert_ok(&run_advice(&repo, &session, &["add", "--snapshot"])?);
    assert_ok(&run_advice(&repo, &session, &[])?); // flush

    let original = std::fs::read(session.jsonl_path())?;

    // Rebuild from the existing DB; must reproduce byte-equal output.
    std::fs::remove_file(session.jsonl_path())?;
    assert_ok(&run_advice(&repo, &session, &["--rebuild-audit-from-db"])?);
    let rebuilt = std::fs::read(session.jsonl_path())?;
    assert_eq!(
        rebuilt, original,
        "rebuild-from-DB must reproduce the JSONL byte-for-byte\noriginal={}\nrebuilt={}",
        String::from_utf8_lossy(&original),
        String::from_utf8_lossy(&rebuilt)
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// (B) --pre/--post round-trip into both stores.
// ---------------------------------------------------------------------------

#[test]
fn pre_and_post_files_round_trip_into_sql_and_audit() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let session = Session::new("prepost");

    let pre = repo.path().join("p.pre");
    let post = repo.path().join("p.post");
    std::fs::write(&pre, "one\ntwo\nthree\n")?;
    std::fs::write(&post, "one\nTWO\n")?;

    assert_ok(&run_advice(
        &repo,
        &session,
        &[
            "add", "--write", "file1.txt#L1-L3",
            "--pre", pre.to_str().unwrap(),
            "--post", post.to_str().unwrap(),
        ],
    )?);

    let sql = read_sql_events(&session);
    assert_eq!(sql.len(), 1);
    let (id, kind, _, payload_str) = &sql[0];
    assert_eq!(kind, "write");
    let payload: Value = serde_json::from_str(payload_str)?;
    assert_eq!(payload["pre_blob"], "one\ntwo\nthree\n");
    assert_eq!(payload["post_blob"], "one\nTWO\n");

    // SQL flat table got the same content.
    let conn = rusqlite::Connection::open(session.db_path())?;
    let (sql_pre, sql_post): (String, String) = conn.query_row(
        "SELECT pre_blob, post_blob FROM write_events WHERE event_id=?1",
        [*id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    assert_eq!(sql_pre, "one\ntwo\nthree\n");
    assert_eq!(sql_post, "one\nTWO\n");

    Ok(())
}

#[test]
fn oversize_pre_content_is_rejected() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let session = Session::new("oversize");

    let big = repo.path().join("big.pre");
    let bytes = vec![b'x'; 1024 * 1024 + 1]; // 1 MiB + 1 byte
    std::fs::write(&big, &bytes)?;

    let out = run_advice(
        &repo,
        &session,
        &[
            "add", "--write", "file1.txt",
            "--pre", big.to_str().unwrap(),
        ],
    )?;
    assert!(!out.status.success(), "expected failure on oversize");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("1 MiB") || stderr.contains("byte cap") || stderr.contains("exceeds"),
        "expected size-cap error, got: {stderr}"
    );
    Ok(())
}

#[test]
fn post_dash_reads_from_stdin() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let session = Session::new("stdin");

    // Drive the binary directly so we can pipe stdin.
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_git-mesh"));
    cmd.current_dir(repo.path());
    cmd.args([
        "advice", &session.id, "add", "--write", "file1.txt", "--post", "-",
    ]);
    cmd.stdin(Stdio::piped());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    let mut child = cmd.spawn()?;
    {
        use std::io::Write;
        child
            .stdin
            .as_mut()
            .unwrap()
            .write_all(b"streamed\ncontent\n")?;
    }
    let out = child.wait_with_output()?;
    assert_ok(&out);

    let sql = read_sql_events(&session);
    assert_eq!(sql.len(), 1);
    let payload: Value = serde_json::from_str(&sql[0].3)?;
    assert_eq!(payload["post_blob"], "streamed\ncontent\n");
    assert!(payload["pre_blob"].is_null());
    Ok(())
}

#[test]
fn pre_dash_is_rejected_to_avoid_two_stdin_inputs() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let session = Session::new("predash");

    let out = run_advice(
        &repo,
        &session,
        &["add", "--write", "file1.txt", "--pre", "-"],
    )?;
    assert!(!out.status.success(), "expected failure for --pre -");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("stdin"),
        "expected stdin-not-allowed error, got: {stderr}"
    );
    Ok(())
}

#[test]
fn pre_line_count_overrides_worktree_for_range_validation() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let session = Session::new("prelines");

    // The --write range describes the PRE extent (the bytes about to be
    // overwritten). file2 has 16 worktree lines, but a 3-line --pre
    // narrows the bound to 3: ranges up to L3 succeed, L4+ must fail.
    let pre = repo.path().join("short.pre");
    std::fs::write(&pre, "a\nb\nc\n")?;
    assert_ok(&run_advice(
        &repo,
        &session,
        &[
            "add", "--write", "file2.txt#L1-L3",
            "--pre", pre.to_str().unwrap(),
        ],
    )?);

    let out = run_advice(
        &repo,
        &session,
        &[
            "add", "--write", "file2.txt#L1-L4",
            "--pre", pre.to_str().unwrap(),
        ],
    )?;
    assert!(!out.status.success(), "expected past-EOF rejection against --pre");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("past EOF"), "expected past EOF error: {stderr}");
    Ok(())
}

#[test]
fn post_line_count_is_not_a_range_upper_bound() -> Result<()> {
    // T4 ("range collapse") requires that the recorded --write range can
    // exceed the post line count: the range describes what was
    // overwritten, the post is what replaced it. file2 has 16 worktree
    // lines; a 1-line post must NOT make `--write file2.txt#L1-L5` fail.
    let repo = TestRepo::seeded()?;
    let session = Session::new("postnotbound");

    let post = repo.path().join("tiny.post");
    std::fs::write(&post, "only\n")?; // 1 line
    let out = run_advice(
        &repo,
        &session,
        &[
            "add", "--write", "file2.txt#L1-L5",
            "--post", post.to_str().unwrap(),
        ],
    )?;
    assert!(
        out.status.success(),
        "post must not bound the --write range; stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    Ok(())
}

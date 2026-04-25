//! JSONL audit log helper.
//!
//! Per `docs/advice-notes.md` §7, every audit line is a strict mirror of
//! the SQL `events` row plus the per-kind payload:
//!
//! ```json
//! {"id": <int>, "kind": "<kind>", "ts": "<rfc3339>", "payload": <object>}
//! ```
//!
//! `payload` is the *same* JSON object that was stored in
//! `events.payload`; the round-trip property is load-bearing
//! (`--rebuild-audit-from-db` regenerates the JSONL deterministically
//! from SQL alone).

use anyhow::{Context, Result};
use rusqlite::Connection;
use serde_json::{Value, json};
use std::io::Write;
use std::path::Path;

use crate::advice::events::AuditRecord;

/// Build the canonical audit-line JSON object for an event.
///
/// Keys are emitted in alphabetical order (serde_json's default `Map` is
/// `BTreeMap`-backed when the `preserve_order` feature is off, which it
/// is in this crate). The string form is therefore deterministic.
pub fn audit_line(rec: &AuditRecord) -> Value {
    json!({
        "id": rec.id,
        "kind": rec.kind,
        "payload": rec.payload,
        "ts": rec.ts,
    })
}

/// Append `line` (a JSON value) as a single newline-terminated entry to the
/// JSONL file at `path`. Creates the file with mode 0o600 if it doesn't exist.
pub fn append_jsonl(path: &Path, line: &Value) -> Result<()> {
    let text = format!("{}\n", line);

    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .create(true)
            .mode(0o600)
            .open(path)?;
        f.write_all(text.as_bytes())?;
    }

    #[cfg(not(unix))]
    {
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(path)?;
        f.write_all(text.as_bytes())?;
    }

    Ok(())
}

/// Append the audit line for `rec` to the JSONL file at `path`.
pub fn append_record(path: &Path, rec: &AuditRecord) -> Result<()> {
    append_jsonl(path, &audit_line(rec))
}

/// Regenerate the JSONL audit log at `path` from the SQL `events` table.
///
/// Truncates `path` first (mode 0o600 on Unix) so the result is exactly
/// the concatenation of canonical audit lines, ordered by `events.id`.
/// The file is byte-identical to one assembled live, because both paths
/// use the same canonical payload bytes stored in `events.payload`.
pub fn rebuild_from_db(conn: &Connection, path: &Path) -> Result<()> {
    // Truncate (or create) with 0o600 on Unix.
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)
            .with_context(|| format!("truncate audit {}", path.display()))?;
    }
    #[cfg(not(unix))]
    {
        std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(path)?;
    }

    let mut stmt = conn
        .prepare("SELECT id, kind, ts, payload FROM events ORDER BY id ASC")
        .context("prepare events scan")?;
    let rows = stmt
        .query_map([], |r| {
            let id: i64 = r.get(0)?;
            let kind: String = r.get(1)?;
            let ts: String = r.get(2)?;
            let payload_str: String = r.get(3)?;
            Ok((id, kind, ts, payload_str))
        })
        .context("scan events")?;

    for row in rows {
        let (id, kind, ts, payload_str) = row?;
        let payload: Value = serde_json::from_str(&payload_str)
            .with_context(|| format!("parse stored payload for event id {id}"))?;
        // Build the audit line manually so the bytes match what
        // `audit_line` would have produced live.
        let line = json!({
            "id": id,
            "kind": kind,
            "payload": payload,
            "ts": ts,
        });
        append_jsonl(path, &line)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::TempDir;

    #[test]
    fn jsonl_line_appended() {
        let td = TempDir::new().unwrap();
        let path = td.path().join("test.jsonl");

        let line = json!({"kind": "read", "path": "foo.ts"});
        append_jsonl(&path, &line).unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("\"read\""));
        assert!(content.contains("\"foo.ts\""));
        assert!(content.ends_with('\n'));
    }

    #[test]
    fn multiple_lines_each_on_own_line() {
        let td = TempDir::new().unwrap();
        let path = td.path().join("test.jsonl");

        append_jsonl(&path, &json!({"n": 1})).unwrap();
        append_jsonl(&path, &json!({"n": 2})).unwrap();
        append_jsonl(&path, &json!({"n": 3})).unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 3);
        for (i, line) in lines.iter().enumerate() {
            let v: Value = serde_json::from_str(line).unwrap();
            assert_eq!(v["n"], i as i64 + 1);
        }
    }

    #[cfg(unix)]
    #[test]
    fn jsonl_file_mode_is_0600() {
        use std::os::unix::fs::PermissionsExt;
        let td = TempDir::new().unwrap();
        let path = td.path().join("test.jsonl");
        append_jsonl(&path, &json!({"x": 1})).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "jsonl file mode should be 0o600, got {mode:o}");
    }

    #[test]
    fn audit_line_shape() {
        let rec = AuditRecord {
            id: 7,
            kind: "read",
            ts: "2026-04-25T00:00:00+00:00".into(),
            payload: json!({"path": "foo.ts", "start_line": null, "end_line": null}),
        };
        let line = audit_line(&rec);
        assert_eq!(line["id"], 7);
        assert_eq!(line["kind"], "read");
        assert_eq!(line["ts"], "2026-04-25T00:00:00+00:00");
        assert_eq!(line["payload"]["path"], "foo.ts");
    }
}

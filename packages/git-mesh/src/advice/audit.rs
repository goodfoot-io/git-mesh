//! JSONL audit log helper.
//!
//! Appends one JSON line per event to the parallel `.jsonl` file. Blob fields
//! (`pre_blob`, `post_blob`) are intentionally omitted: the DB is the source
//! of truth for blob content; excluding them keeps lines under PIPE_BUF
//! (4 KiB) so O_APPEND writes are atomic on POSIX systems.

use anyhow::Result;
use serde_json::Value;
use std::io::Write;
use std::path::Path;

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
}

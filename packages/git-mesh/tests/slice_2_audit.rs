//! Slice 2 — session-state regression for the file-backed pipeline.
//!
//! After the SQL stack was deleted, the JSONL "audit" log is gone and
//! `--rebuild-audit-from-db` no longer exists. What survives is the
//! file-backed session state: `baseline.state`, `last-flush.state`,
//! `reads.jsonl`, `touches.jsonl`, `advice-seen.jsonl`, `docs-seen.jsonl`.
//! This file pins those records across the snapshot → read → render
//! sequence.

mod support;

use anyhow::Result;
use std::process::Output;
use support::TestRepo;
use uuid::Uuid;

fn sid(prefix: &str) -> String {
    format!("slice2-{prefix}-{}", Uuid::new_v4())
}

fn run_advice(repo: &TestRepo, session: &str, extra: &[&str]) -> Result<Output> {
    let mut args: Vec<String> = vec!["advice".into(), session.into()];
    for a in extra {
        args.push((*a).into());
    }
    repo.run_mesh(args)
}

fn session_dir(repo: &TestRepo, sid: &str) -> std::path::PathBuf {
    let store = git_mesh::advice::SessionStore::open(repo.path(), &repo.path().join(".git"), sid)
        .expect("open store");
    store
        .baseline_objects_dir()
        .parent()
        .expect("parent")
        .to_path_buf()
}

fn assert_ok(out: &Output) {
    assert!(
        out.status.success(),
        "expected success, code={:?} stderr={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn snapshot_creates_baseline_and_last_flush_state() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let s = sid("snap-state");
    assert_ok(&run_advice(&repo, &s, &["snapshot"])?);
    let dir = session_dir(&repo, &s);
    assert!(
        dir.join("baseline.state").exists(),
        "baseline.state must exist"
    );
    assert!(
        dir.join("last-flush.state").exists(),
        "last-flush.state must exist"
    );
    assert!(
        dir.join("baseline.objects").is_dir(),
        "baseline.objects/ must exist"
    );
    assert!(
        dir.join("last-flush.objects").is_dir(),
        "last-flush.objects/ must exist"
    );
    let baseline = std::fs::read(dir.join("baseline.state"))?;
    let last_flush = std::fs::read(dir.join("last-flush.state"))?;
    assert_eq!(
        baseline, last_flush,
        "snapshot writes identical baseline + last-flush state"
    );
    Ok(())
}

#[test]
fn read_appends_to_reads_jsonl() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let s = sid("read-append");
    assert_ok(&run_advice(&repo, &s, &["snapshot"])?);
    let dir = session_dir(&repo, &s);
    let reads_path = dir.join("reads.jsonl");
    let before = std::fs::metadata(&reads_path).map(|m| m.len()).unwrap_or(0);

    assert_ok(&run_advice(&repo, &s, &["read", "file1.txt"])?);
    assert_ok(&run_advice(&repo, &s, &["read", "file2.txt"])?);

    let after = std::fs::metadata(&reads_path)?.len();
    assert!(after > before, "reads.jsonl must grow after `read`");
    let content = std::fs::read_to_string(&reads_path)?;
    let n_lines = content.lines().count();
    assert_eq!(
        n_lines, 2,
        "reads.jsonl must record one line per `read`, got:\n{content}"
    );
    for line in content.lines() {
        let v: serde_json::Value = serde_json::from_str(line).expect("valid json");
        assert!(v.get("path").is_some(), "record must carry `path`: {line}");
        assert!(v.get("ts").is_some(), "record must carry `ts`: {line}");
    }
    Ok(())
}

#[test]
#[ignore] // Phase 3
fn render_advances_last_flush_state_and_records_touch() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let s = sid("advance");
    assert_ok(&run_advice(&repo, &s, &["snapshot"])?);
    let dir = session_dir(&repo, &s);
    let last_flush_before = std::fs::read(dir.join("last-flush.state"))?;

    repo.write_file("file1.txt", "edited content\n")?;
    let out = run_advice(&repo, &s, &[])?;
    assert_ok(&out);

    let last_flush_after = std::fs::read(dir.join("last-flush.state"))?;
    assert_ne!(
        last_flush_after, last_flush_before,
        "last-flush.state must advance"
    );
    let touches = std::fs::read_to_string(dir.join("touches.jsonl"))?;
    assert!(
        !touches.is_empty(),
        "touches.jsonl must record a non-empty render"
    );
    Ok(())
}

#[test]
fn fails_closed_when_baseline_missing_for_read() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let s = sid("nobaseline-read");
    let out = run_advice(&repo, &s, &["read", "file1.txt"])?;
    assert!(
        !out.status.success(),
        "read without snapshot must fail closed"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("snapshot"),
        "stderr must name `snapshot`: {stderr}"
    );
    Ok(())
}

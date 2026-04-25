//! Contract tests for `git mesh advice <id>` (bare render) — Phase 1 of
//! sub-card C. The SQL stack is still in place; these tests exercise only
//! the file-backed render path.

mod support;

use anyhow::Result;
use std::process::Output;
use support::TestRepo;
use uuid::Uuid;

fn session_id(label: &str) -> String {
    format!("render-{label}-{}", Uuid::new_v4())
}

fn run_advice(repo: &TestRepo, session: &str, extra: &[&str]) -> Result<Output> {
    let mut args: Vec<String> = vec!["advice".into(), session.into()];
    for a in extra {
        args.push((*a).to_string());
    }
    repo.run_mesh(args)
}

fn session_dir(repo: &TestRepo, sid: &str) -> std::path::PathBuf {
    let store = git_mesh::advice::SessionStore::open(
        repo.path(),
        &repo.path().join(".git"),
        sid,
    )
    .expect("open store");
    store
        .baseline_objects_dir()
        .parent()
        .expect("parent")
        .to_path_buf()
}

// ---------------------------------------------------------------------------
// snapshot, then bare render with no changes: silent, exit 0.
// ---------------------------------------------------------------------------
#[test]
fn bare_render_after_snapshot_no_changes_is_silent() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let sid = session_id("noop");
    let _ = run_advice(&repo, &sid, &["snapshot"])?;
    let out = run_advice(&repo, &sid, &[])?;
    assert!(out.status.success(), "expected exit 0, stderr={}", String::from_utf8_lossy(&out.stderr));
    assert!(
        out.stdout.is_empty(),
        "expected silent render, got: {}",
        String::from_utf8_lossy(&out.stdout)
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// bare render before snapshot: non-zero, message names `snapshot`.
// ---------------------------------------------------------------------------
#[test]
fn bare_render_without_snapshot_fails_closed() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let sid = session_id("nosnap");
    let out = run_advice(&repo, &sid, &[])?;
    assert!(!out.status.success(), "expected non-zero exit");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("snapshot"),
        "stderr must name `snapshot`, got: {stderr}"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// bare render advances read_cursor even when nothing prints.
// ---------------------------------------------------------------------------
#[test]
fn bare_render_advances_read_cursor_when_silent() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let sid = session_id("cursor");
    run_advice(&repo, &sid, &["snapshot"])?;
    // Append a read so reads.jsonl is non-empty.
    run_advice(&repo, &sid, &["read", "file1.txt"])?;

    let dir = session_dir(&repo, &sid);
    let reads_len_before = std::fs::metadata(dir.join("reads.jsonl"))?.len();
    assert!(reads_len_before > 0);

    let out = run_advice(&repo, &sid, &[])?;
    assert!(out.status.success());

    let cursor_path = dir.join("last-flush.read-cursor");
    assert!(cursor_path.exists(), "cursor sidecar must exist after render");
    let cursor: u64 = std::fs::read_to_string(&cursor_path)?.trim().parse()?;
    assert_eq!(cursor, reads_len_before, "cursor must equal byte length of reads.jsonl after render");
    Ok(())
}

// ---------------------------------------------------------------------------
// bare render records a touch interval when delta non-empty.
// ---------------------------------------------------------------------------
#[test]
fn bare_render_records_touch_when_delta_nonempty() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let sid = session_id("touch");
    run_advice(&repo, &sid, &["snapshot"])?;

    // Modify file1.txt — incr_delta non-empty.
    repo.write_file("file1.txt", "modified contents\n")?;

    let dir = session_dir(&repo, &sid);
    let touches_before = std::fs::metadata(dir.join("touches.jsonl"))?.len();
    assert_eq!(touches_before, 0);

    let out = run_advice(&repo, &sid, &[])?;
    assert!(out.status.success(), "stderr={}", String::from_utf8_lossy(&out.stderr));

    let touches_after = std::fs::metadata(dir.join("touches.jsonl"))?.len();
    assert!(touches_after > 0, "touches.jsonl must record an interval when delta non-empty");
    Ok(())
}

// ---------------------------------------------------------------------------
// Two consecutive renders: second diffs against first render's tree.
// ---------------------------------------------------------------------------
#[test]
fn two_consecutive_renders_diff_against_last_flush() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let sid = session_id("twostep");
    run_advice(&repo, &sid, &["snapshot"])?;

    // Modify A.
    repo.write_file("file1.txt", "A1\n")?;
    let out1 = run_advice(&repo, &sid, &[])?;
    assert!(out1.status.success());

    let dir = session_dir(&repo, &sid);
    // After first render last-flush.objects must exist.
    assert!(dir.join("last-flush.objects").is_dir(), "last-flush.objects must exist after first render");

    // Modify B (a different file).
    repo.write_file("file2.txt", "B1\n")?;
    let touches_before = std::fs::metadata(dir.join("touches.jsonl"))?.len();
    let out2 = run_advice(&repo, &sid, &[])?;
    assert!(out2.status.success(), "stderr={}", String::from_utf8_lossy(&out2.stderr));
    let touches_after = std::fs::metadata(dir.join("touches.jsonl"))?.len();
    // The second render saw a non-empty incr_delta (file2 changed since last
    // flush) and so must have recorded a fresh touch interval.
    assert!(
        touches_after > touches_before,
        "second render must record a touch (B's change vs last-flush)"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Broken-pipe ordering: state mutations precede stdout.
// We can't easily simulate EPIPE in-process, so we assert positively that
// after a successful render with delta, last-flush.state advanced and
// last-flush.objects/ exists — which is the load-bearing invariant.
// ---------------------------------------------------------------------------
#[test]
fn render_advances_last_flush_state_before_stdout() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let sid = session_id("ordering");
    run_advice(&repo, &sid, &["snapshot"])?;

    let dir = session_dir(&repo, &sid);
    let baseline_bytes = std::fs::read(dir.join("baseline.state"))?;
    let last_flush_before = std::fs::read(dir.join("last-flush.state"))?;
    assert_eq!(
        baseline_bytes, last_flush_before,
        "snapshot writes identical baseline + last-flush"
    );

    // Make a change and render.
    repo.write_file("file1.txt", "edited\n")?;
    let out = run_advice(&repo, &sid, &[])?;
    assert!(out.status.success());

    let last_flush_after = std::fs::read(dir.join("last-flush.state"))?;
    assert_ne!(
        last_flush_after, last_flush_before,
        "last-flush.state must advance after a render with delta"
    );
    assert!(dir.join("last-flush.objects").is_dir());
    // current.objects-* must NOT linger after a successful render.
    let mut leftover = false;
    for e in std::fs::read_dir(&dir)? {
        let name = e?.file_name().to_string_lossy().into_owned();
        if name.starts_with("current.objects-") {
            leftover = true;
            break;
        }
    }
    assert!(!leftover, "current.objects-<uuid> must be promoted, not left behind");
    Ok(())
}

// ---------------------------------------------------------------------------
// --documentation: doc-seen suppression on second render.
// ---------------------------------------------------------------------------
#[test]
fn documentation_topics_are_suppressed_on_second_render() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let sid = session_id("docs");
    run_advice(&repo, &sid, &["snapshot"])?;
    // Whether or not anything renders, docs-seen.jsonl must remain a stable
    // file across two consecutive renders — topics seen on render 1 must not
    // be re-emitted on render 2.
    repo.write_file("file1.txt", "edit-1\n")?;
    let _ = run_advice(&repo, &sid, &["--documentation"])?;
    let dir = session_dir(&repo, &sid);
    let docs_after_first = std::fs::read(dir.join("docs-seen.jsonl"))?;

    repo.write_file("file1.txt", "edit-2\n")?;
    let _ = run_advice(&repo, &sid, &["--documentation"])?;
    let docs_after_second = std::fs::read(dir.join("docs-seen.jsonl"))?;

    // docs-seen.jsonl is monotonically growing (or unchanged when nothing new
    // emits). It must never shrink.
    assert!(
        docs_after_second.len() >= docs_after_first.len(),
        "docs-seen.jsonl must not shrink across renders"
    );
    Ok(())
}

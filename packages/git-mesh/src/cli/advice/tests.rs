//! Tests for the per-`tool_use_id` `mark`/`flush` CLI surface.

#![allow(unused_imports, dead_code)]

use anyhow::Result;

use super::{
    TouchKindArg, collect_touched_paths, run_advice_end, run_advice_flush, run_advice_mark,
    run_advice_read, run_advice_touch,
};

struct FixtureRepo {
    dir: tempfile::TempDir,
}

impl FixtureRepo {
    fn new() -> Result<Self> {
        let dir = tempfile::tempdir()?;
        let path = dir.path();
        Self::git(path, &["init", "--initial-branch=main"])?;
        Self::git(path, &["config", "user.email", "t@t"])?;
        Self::git(path, &["config", "user.name", "T"])?;
        Self::git(path, &["config", "commit.gpgsign", "false"])?;
        std::fs::write(
            path.join("file1.txt"),
            "line1\nline2\nline3\nline4\nline5\nline6\nline7\nline8\nline9\nline10\n",
        )?;
        std::fs::write(
            path.join("file2.txt"),
            "line1\nline2\nline3\nline4\nline5\nline6\nline7\nline8\nline9\nline10\n",
        )?;
        Self::git(path, &["add", "."])?;
        Self::git(path, &["commit", "-m", "init"])?;
        Ok(Self { dir })
    }

    fn path(&self) -> &std::path::Path {
        self.dir.path()
    }

    fn git(dir: &std::path::Path, args: &[&str]) -> Result<()> {
        let out = std::process::Command::new("git")
            .current_dir(dir)
            .args(args)
            .output()?;
        anyhow::ensure!(
            out.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&out.stderr)
        );
        Ok(())
    }

    fn gix_repo(&self) -> Result<gix::Repository> {
        Ok(gix::open(self.path())?)
    }

    fn sid(label: &str) -> String {
        format!("unit-{label}-{}", uuid::Uuid::new_v4())
    }

    fn session_dir(&self, session_id: &str) -> std::path::PathBuf {
        use crate::advice::session::store::session_dir;
        let gix = gix::open(self.path()).unwrap();
        session_dir(self.path(), gix.git_dir(), session_id)
    }
}

fn touches_for(session_dir: &std::path::Path) -> Vec<crate::advice::session::state::TouchInterval> {
    let path = session_dir.join("touches.jsonl");
    let Ok(contents) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    contents
        .lines()
        .filter(|l| !l.is_empty())
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

#[test]
fn mark_flush_records_modified_file_with_id() -> Result<()> {
    let repo = FixtureRepo::new()?;
    let s = FixtureRepo::sid("modify");
    let gix = repo.gix_repo()?;

    // A read of the path must precede flush for the touch to pass the gate.
    run_advice_read(&gix, s.clone(), "file1.txt".into(), None)?;
    run_advice_mark(&gix, s.clone(), "tool-1".into())?;
    std::fs::write(repo.path().join("file1.txt"), "edited\n")?;
    run_advice_flush(&gix, s.clone(), "tool-1".into())?;

    let touches = touches_for(&repo.session_dir(&s));
    assert_eq!(touches.len(), 1, "got: {touches:?}");
    let t = &touches[0];
    assert_eq!(t.path, "file1.txt");
    assert_eq!(t.id, "tool-1");
    assert!(matches!(
        t.kind,
        crate::advice::session::state::TouchKind::Modified
    ));
    Ok(())
}

#[test]
fn mark_flush_records_added_untracked_with_id() -> Result<()> {
    let repo = FixtureRepo::new()?;
    let s = FixtureRepo::sid("add");
    let gix = repo.gix_repo()?;

    // Write the file first so `read` can validate it exists, then mark/flush.
    std::fs::write(repo.path().join("new.txt"), "hello\n")?;
    // A read of the new path must precede flush for the touch to pass the gate.
    run_advice_read(&gix, s.clone(), "new.txt".into(), None)?;
    // Re-mark after the read so the snapshot captures the pre-change state.
    // For this test we just need to verify the touch is recorded; we can
    // mark → flush without a working-tree change (the file was written before mark).
    run_advice_mark(&gix, s.clone(), "tool-A".into())?;
    // Touch the file to produce a diff from the snapshot perspective (it's untracked).
    std::fs::write(repo.path().join("new.txt"), "hello world\n")?;
    run_advice_flush(&gix, s.clone(), "tool-A".into())?;

    let touches = touches_for(&repo.session_dir(&s));
    assert!(
        touches.iter().any(|t| t.path == "new.txt" && t.id == "tool-A"),
        "expected a touch for new.txt: {touches:?}"
    );
    Ok(())
}

#[test]
fn flush_is_noop_when_mark_missing() -> Result<()> {
    let repo = FixtureRepo::new()?;
    let s = FixtureRepo::sid("noop");
    let gix = repo.gix_repo()?;

    let code = run_advice_flush(&gix, s.clone(), "never-marked".into())?;
    assert_eq!(code, 0);
    let touches = touches_for(&repo.session_dir(&s));
    assert!(touches.is_empty(), "expected no touches: {touches:?}");
    Ok(())
}

#[test]
fn read_only_idle_session_produces_no_touches() -> Result<()> {
    let repo = FixtureRepo::new()?;
    let s = FixtureRepo::sid("idle");
    let gix = repo.gix_repo()?;

    // Idle: simulate a read-only tool by marking and flushing without
    // touching the working tree.
    run_advice_mark(&gix, s.clone(), "read-only-tool".into())?;
    run_advice_flush(&gix, s.clone(), "read-only-tool".into())?;

    let touches = touches_for(&repo.session_dir(&s));
    assert!(touches.is_empty(), "expected no touches: {touches:?}");
    Ok(())
}

#[test]
fn touched_lists_added_modified_deleted_dedup_first_seen_skipping_modechange() -> Result<()> {
    use crate::advice::session::state::{TouchInterval, TouchKind};
    let repo = FixtureRepo::new()?;
    let s = FixtureRepo::sid("touched");
    let gix = repo.gix_repo()?;

    // Force the session directory into existence.
    run_advice_mark(&gix, s.clone(), "seed".into())?;
    run_advice_flush(&gix, s.clone(), "seed".into())?;

    let session_dir = repo.session_dir(&s);
    let touches_path = session_dir.join("touches.jsonl");
    let entries = vec![
        TouchInterval {
            path: "a.rs".into(),
            kind: TouchKind::Added,
            id: "t1".into(),
            ts: "t".into(),
            start: None,
            end: None,
        },
        TouchInterval {
            path: "b.rs".into(),
            kind: TouchKind::Modified,
            id: "t1".into(),
            ts: "t".into(),
            start: None,
            end: None,
        },
        TouchInterval {
            path: "b.rs".into(),
            kind: TouchKind::Modified,
            id: "t2".into(),
            ts: "t".into(),
            start: None,
            end: None,
        },
        TouchInterval {
            path: "c.rs".into(),
            kind: TouchKind::Deleted,
            id: "t2".into(),
            ts: "t".into(),
            start: None,
            end: None,
        },
        TouchInterval {
            path: "script.sh".into(),
            kind: TouchKind::ModeChange,
            id: "t3".into(),
            ts: "t".into(),
            start: None,
            end: None,
        },
    ];
    let mut body = String::new();
    for e in &entries {
        body.push_str(&serde_json::to_string(e)?);
        body.push('\n');
    }
    std::fs::write(&touches_path, body)?;

    let paths = collect_touched_paths(&touches_path)?;
    assert_eq!(paths, vec!["a.rs", "b.rs", "c.rs"]);
    Ok(())
}

#[test]
fn touched_returns_empty_when_no_touches_file() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let paths = collect_touched_paths(&dir.path().join("touches.jsonl"))?;
    assert!(paths.is_empty());
    Ok(())
}

#[test]
fn read_records_optional_id_correlation() -> Result<()> {
    let repo = FixtureRepo::new()?;
    let s = FixtureRepo::sid("read");
    let gix = repo.gix_repo()?;

    run_advice_read(
        &gix,
        s.clone(),
        "file1.txt#L1-L5".into(),
        Some("read-tool".into()),
    )?;

    let reads_path = repo.session_dir(&s).join("reads.jsonl");
    let contents = std::fs::read_to_string(&reads_path)?;
    let line = contents.lines().next().expect("reads.jsonl is empty");
    let rec: crate::advice::session::state::ReadRecord = serde_json::from_str(line)?;
    assert_eq!(rec.path, "file1.txt");
    assert_eq!(rec.start_line, Some(1));
    assert_eq!(rec.end_line, Some(5));
    assert_eq!(rec.id.as_deref(), Some("read-tool"));
    Ok(())
}

// ── touch tests ──────────────────────────────────────────────────────────────

#[test]
fn touch_line_anchored_modified_appends_touch_with_range() -> Result<()> {
    let repo = FixtureRepo::new()?;
    let s = FixtureRepo::sid("touch-mod");
    let gix = repo.gix_repo()?;

    run_advice_touch(
        &gix,
        s.clone(),
        "tuid-1".into(),
        "file1.txt#L2-L5".into(),
        TouchKindArg::Modified,
    )?;

    let touches = touches_for(&repo.session_dir(&s));
    assert_eq!(touches.len(), 1, "expected one touch: {touches:?}");
    let t = &touches[0];
    assert_eq!(t.path, "file1.txt");
    assert_eq!(t.id, "tuid-1");
    assert!(matches!(
        t.kind,
        crate::advice::session::state::TouchKind::Modified
    ));
    assert_eq!(t.start, Some(2));
    assert_eq!(t.end, Some(5));
    Ok(())
}

#[test]
fn touch_whole_file_added_appends_touch_with_no_range() -> Result<()> {
    let repo = FixtureRepo::new()?;
    let s = FixtureRepo::sid("touch-add");
    let gix = repo.gix_repo()?;

    run_advice_touch(
        &gix,
        s.clone(),
        "tuid-2".into(),
        "file1.txt".into(),
        TouchKindArg::Added,
    )?;

    let touches = touches_for(&repo.session_dir(&s));
    assert_eq!(touches.len(), 1, "expected one touch: {touches:?}");
    let t = &touches[0];
    assert_eq!(t.path, "file1.txt");
    assert_eq!(t.id, "tuid-2");
    assert!(matches!(
        t.kind,
        crate::advice::session::state::TouchKind::Added
    ));
    assert_eq!(t.start, None);
    assert_eq!(t.end, None);
    Ok(())
}

/// Verify that a line-anchored touch records the correct range in touches.jsonl.
/// Mesh emission is an integration-level concern; here we confirm start/end
/// routing through process_touches is correct.
#[test]
fn touch_line_anchored_range_routing() -> Result<()> {
    let repo = FixtureRepo::new()?;
    let gix = repo.gix_repo()?;

    // Overlapping touch within file bounds (file has 10 lines).
    let s_overlap = FixtureRepo::sid("touch-route-overlap");
    run_advice_touch(
        &gix,
        s_overlap.clone(),
        "tuid-route".into(),
        "file1.txt#L5-L10".into(),
        TouchKindArg::Modified,
    )?;
    let touches = touches_for(&repo.session_dir(&s_overlap));
    assert_eq!(touches.len(), 1);
    assert_eq!(touches[0].start, Some(5));
    assert_eq!(touches[0].end, Some(10));

    // Non-overlapping touch in a different range.
    let s_no_overlap = FixtureRepo::sid("touch-route-no-overlap");
    let gix2 = repo.gix_repo()?;
    run_advice_touch(
        &gix2,
        s_no_overlap.clone(),
        "tuid-no-route".into(),
        "file1.txt#L1-L4".into(),
        TouchKindArg::Modified,
    )?;
    let touches2 = touches_for(&repo.session_dir(&s_no_overlap));
    assert_eq!(touches2.len(), 1);
    assert_eq!(touches2[0].start, Some(1));
    assert_eq!(touches2[0].end, Some(4));

    Ok(())
}

#[test]
fn touch_does_not_create_snapshot_files() -> Result<()> {
    let repo = FixtureRepo::new()?;
    let s = FixtureRepo::sid("touch-no-snap");
    let gix = repo.gix_repo()?;

    run_advice_touch(
        &gix,
        s.clone(),
        "tuid-snap".into(),
        "file1.txt#L1-L3".into(),
        TouchKindArg::Modified,
    )?;

    let session_dir = repo.session_dir(&s);
    let snapshots_dir = session_dir.join("snapshots");
    if snapshots_dir.exists() {
        let entries: Vec<_> = std::fs::read_dir(&snapshots_dir)?
            .flatten()
            .collect();
        assert!(
            entries.is_empty(),
            "snapshots dir should be empty after touch, found: {entries:?}"
        );
    }
    // If snapshots dir doesn't exist, that's also fine.
    Ok(())
}

// ── pending-touch gate tests ─────────────────────────────────────────────────

/// Helper: read `pending_touches.jsonl` from a session directory.
fn pending_touches_for(
    session_dir: &std::path::Path,
) -> Vec<crate::advice::session::state::TouchInterval> {
    let path = session_dir.join("pending_touches.jsonl");
    let Ok(contents) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    contents
        .lines()
        .filter(|l| !l.is_empty())
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

/// A flush whose path was read (via a prior `read` call) emits normally and
/// writes to `touches.jsonl`; nothing ends up in `pending_touches.jsonl`.
#[test]
fn flush_after_read_writes_to_touches_not_pending() -> Result<()> {
    let repo = FixtureRepo::new()?;
    let s = FixtureRepo::sid("read-then-flush");
    let gix = repo.gix_repo()?;

    // Read first — this seeds reads.jsonl.
    run_advice_read(&gix, s.clone(), "file1.txt".into(), None)?;

    // Now mark + modify + flush.
    run_advice_mark(&gix, s.clone(), "tool-after-read".into())?;
    std::fs::write(repo.path().join("file1.txt"), "changed\n")?;
    run_advice_flush(&gix, s.clone(), "tool-after-read".into())?;

    let session_dir = repo.session_dir(&s);

    // Touch recorded in touches.jsonl (matched path).
    let touches = touches_for(&session_dir);
    assert_eq!(
        touches.len(),
        1,
        "expected one touch in touches.jsonl: {touches:?}"
    );
    assert_eq!(touches[0].path, "file1.txt");

    // Nothing parked.
    let pending = pending_touches_for(&session_dir);
    assert!(
        pending.is_empty(),
        "pending_touches.jsonl should be empty: {pending:?}"
    );
    Ok(())
}


// ── pending-touches-unbounded-growth ─────────────────────────────────────────

/// Appending the same `(path, kind, start, end)` twice must result in only
/// one row in `pending_touches.jsonl`.
#[test]
fn append_pending_touch_is_idempotent_on_same_key() -> Result<()> {
    use crate::advice::session::SessionStore;
    use crate::advice::session::state::{TouchInterval, TouchKind};

    let repo = FixtureRepo::new()?;
    let s = FixtureRepo::sid("dedup-pending");
    let gix = repo.gix_repo()?;

    let store = SessionStore::open(repo.path(), gix.git_dir(), &s)?;
    store.ensure_initialized()?;

    let t = TouchInterval {
        path: "file1.txt".into(),
        kind: TouchKind::Modified,
        id: "tuid-dedup".into(),
        ts: chrono::Utc::now().to_rfc3339(),
        start: None,
        end: None,
    };
    store.append_pending_touch(&t)?;
    // Second append with same (path, kind, start, end) — different id and ts.
    let t2 = TouchInterval {
        path: "file1.txt".into(),
        kind: TouchKind::Modified,
        id: "tuid-dedup-2".into(),
        ts: chrono::Utc::now().to_rfc3339(),
        start: None,
        end: None,
    };
    store.append_pending_touch(&t2)?;

    let session_dir = repo.session_dir(&s);
    let pending = pending_touches_for(&session_dir);
    assert_eq!(
        pending.len(),
        1,
        "duplicate (path,kind,start,end) must not create a second row: {pending:?}"
    );
    Ok(())
}

/// Different `(path, kind)` pairs must each get their own row.
#[test]
fn append_pending_touch_distinct_keys_both_land() -> Result<()> {
    use crate::advice::session::SessionStore;
    use crate::advice::session::state::{TouchInterval, TouchKind};

    let repo = FixtureRepo::new()?;
    let s = FixtureRepo::sid("dedup-distinct");
    let gix = repo.gix_repo()?;

    let store = SessionStore::open(repo.path(), gix.git_dir(), &s)?;
    store.ensure_initialized()?;

    let ts = chrono::Utc::now().to_rfc3339();
    store.append_pending_touch(&TouchInterval {
        path: "file1.txt".into(),
        kind: TouchKind::Modified,
        id: "a".into(),
        ts: ts.clone(),
        start: None,
        end: None,
    })?;
    store.append_pending_touch(&TouchInterval {
        path: "file2.txt".into(),
        kind: TouchKind::Modified,
        id: "b".into(),
        ts: ts.clone(),
        start: None,
        end: None,
    })?;

    let session_dir = repo.session_dir(&s);
    let pending = pending_touches_for(&session_dir);
    assert_eq!(
        pending.len(),
        2,
        "distinct paths must each get a row: {pending:?}"
    );
    Ok(())
}

// ── gate-path-equality-misses-case-normalization-variants ────────────────────

/// `canonicalize_repo_relative_path` strips a leading `./`.
#[test]
fn canonicalize_strips_leading_dot_slash() -> Result<()> {
    use super::canonicalize_repo_relative_path;
    let repo = FixtureRepo::new()?;
    let wd = repo.path();
    let normalized = canonicalize_repo_relative_path(wd, "./file1.txt")?;
    assert_eq!(normalized, "file1.txt");
    Ok(())
}

/// `canonicalize_repo_relative_path` resolves a `..` component lexically.
#[test]
fn canonicalize_resolves_dotdot() -> Result<()> {
    use super::canonicalize_repo_relative_path;
    let repo = FixtureRepo::new()?;
    let wd = repo.path();
    // Create a subdirectory so the path exists.
    std::fs::create_dir_all(wd.join("sub"))?;
    std::fs::write(wd.join("sub").join("f.txt"), "x")?;
    let normalized = canonicalize_repo_relative_path(wd, "sub/../file1.txt")?;
    assert_eq!(normalized, "file1.txt");
    Ok(())
}

/// A path that does not exist is normalized lexically (no error).
#[test]
fn canonicalize_nonexistent_path_is_lexical() -> Result<()> {
    use super::canonicalize_repo_relative_path;
    let repo = FixtureRepo::new()?;
    let wd = repo.path();
    let normalized = canonicalize_repo_relative_path(wd, "./does-not-exist.rs")?;
    assert_eq!(normalized, "does-not-exist.rs");
    Ok(())
}

/// A path that escapes the working directory must return an error.
#[test]
fn canonicalize_escape_returns_err() {
    use super::canonicalize_repo_relative_path;
    let repo = FixtureRepo::new().unwrap();
    let wd = repo.path();
    let result = canonicalize_repo_relative_path(wd, "../../escape");
    assert!(
        result.is_err(),
        "a path escaping wd must return Err, got: {result:?}"
    );
}

// ── symlinked-wd canonicalization tests ──────────────────────────────────────

/// On a symlinked workspace root both existing and non-existing paths under
/// that root must canonicalize to the same repo-relative form.
#[cfg(unix)]
#[test]
fn canonicalize_symmetric_under_symlinked_wd() -> Result<()> {
    use super::canonicalize_repo_relative_path;

    let real_dir = tempfile::tempdir()?;
    let real_path = real_dir.path();

    // Create an existing file.
    std::fs::write(real_path.join("existing.txt"), "hello")?;

    // Create a symlinked alias of the tempdir.
    let link_dir = tempfile::tempdir()?;
    let link_path = link_dir.path().join("symlinked-wd");
    std::os::unix::fs::symlink(real_path, &link_path)?;

    // Use the symlinked alias as the working directory.
    let wd = &link_path;

    // Both calls must succeed.
    let existing = canonicalize_repo_relative_path(wd, "existing.txt")?;
    let missing = canonicalize_repo_relative_path(wd, "nonexistent.rs")?;

    assert_eq!(existing, "existing.txt");
    assert_eq!(missing, "nonexistent.rs");
    Ok(())
}

// ── end tests ────────────────────────────────────────────────────────────────

#[test]
fn end_removes_session_dir_and_is_idempotent() -> Result<()> {
    let repo = FixtureRepo::new()?;
    let s = FixtureRepo::sid("end-test");
    let gix = repo.gix_repo()?;

    // Create a session with a touch so the dir exists.
    run_advice_touch(
        &gix,
        s.clone(),
        "tuid-end".into(),
        "file1.txt".into(),
        TouchKindArg::Added,
    )?;
    let session_dir = repo.session_dir(&s);
    assert!(session_dir.exists(), "session dir should exist after touch");

    let code = run_advice_end(&gix, s.clone())?;
    assert_eq!(code, 0);
    assert!(
        !session_dir.exists(),
        "session dir should be removed after end"
    );

    // Second call is a no-op (idempotent).
    let code2 = run_advice_end(&gix, s.clone())?;
    assert_eq!(code2, 0);
    Ok(())
}

#[test]
fn end_sweeps_leftover_snapshots() -> Result<()> {
    let repo = FixtureRepo::new()?;
    let s = FixtureRepo::sid("end-snap");
    let gix = repo.gix_repo()?;

    // Create a mark (which creates a snapshot) but don't flush.
    run_advice_mark(&gix, s.clone(), "orphan-snap".into())?;
    let session_dir = repo.session_dir(&s);
    let snapshots_dir = session_dir.join("snapshots");
    assert!(snapshots_dir.exists(), "snapshots dir should exist after mark");
    let snap_count = std::fs::read_dir(&snapshots_dir)?.count();
    assert!(snap_count > 0, "should have at least one snapshot file");

    let code = run_advice_end(&gix, s.clone())?;
    assert_eq!(code, 0);
    assert!(!session_dir.exists(), "session dir should be gone after end");
    Ok(())
}

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

    run_advice_mark(&gix, s.clone(), "tool-A".into())?;
    std::fs::write(repo.path().join("new.txt"), "hello\n")?;
    run_advice_flush(&gix, s.clone(), "tool-A".into())?;

    let touches = touches_for(&repo.session_dir(&s));
    assert!(
        touches.iter().any(|t| t.path == "new.txt"
            && t.id == "tool-A"
            && matches!(t.kind, crate::advice::session::state::TouchKind::Added)),
        "expected Added new.txt: {touches:?}"
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

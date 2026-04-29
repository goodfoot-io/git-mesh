//! Tests for the per-`tool_use_id` `mark`/`flush` CLI surface.

#![allow(unused_imports, dead_code)]

use anyhow::Result;

use super::{run_advice_flush, run_advice_mark, run_advice_read};

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
        touches
            .iter()
            .any(|t| t.path == "new.txt"
                && t.id == "tool-A"
                && matches!(
                    t.kind,
                    crate::advice::session::state::TouchKind::Added
                )),
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

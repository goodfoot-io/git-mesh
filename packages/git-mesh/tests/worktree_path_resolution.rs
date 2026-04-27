//! Regression: `git mesh` operations fail when run from a linked
//! worktree because `.git` there is a pointer file, not a directory.
//!
//! The bug is `wd.join(".git").join("mesh")...` — in a worktree, that
//! traverses *into* the `.git` file and returns `ENOTDIR`. Mesh state
//! must be anchored to `repo.git_dir()` instead.

mod support;

use anyhow::Result;
use git_mesh::staging::read_staging;
use git_mesh::{append_add, append_remove};
use std::process::Command;
use support::TestRepo;

/// Create a linked worktree off `repo` at HEAD on a new branch and
/// return its path. The worktree directory is inside the temp repo so
/// it's cleaned up with the parent.
fn add_worktree(repo: &TestRepo, name: &str) -> Result<std::path::PathBuf> {
    let wt = repo.path().parent().unwrap().join(format!("wt-{name}"));
    repo.run_git([
        "worktree",
        "add",
        "-b",
        name,
        wt.to_str().unwrap(),
        "HEAD",
    ])?;
    Ok(wt)
}

#[test]
fn append_add_works_from_worktree() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let wt = add_worktree(&repo, "wt1")?;
    let gix = gix::open(&wt)?;
    append_add(&gix, "m", "file1.txt", 1, 5, None)?;
    let s = read_staging(&gix, "m")?;
    assert_eq!(s.adds.len(), 1);
    assert_eq!(s.adds[0].path, "file1.txt");
    Ok(())
}

#[test]
fn append_remove_works_from_worktree() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let wt = add_worktree(&repo, "wt2")?;
    let gix = gix::open(&wt)?;
    append_remove(&gix, "m", "file1.txt", 1, 5)?;
    Ok(())
}

#[test]
fn cli_mesh_add_works_from_worktree() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let wt = add_worktree(&repo, "wt3")?;
    let out = Command::new(env!("CARGO_BIN_EXE_git-mesh"))
        .current_dir(&wt)
        .args(["add", "doc/feature", "file1.txt#L1-L5"])
        .output()?;
    assert!(
        out.status.success(),
        "git-mesh add from worktree failed (code {:?}): stdout={} stderr={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    Ok(())
}

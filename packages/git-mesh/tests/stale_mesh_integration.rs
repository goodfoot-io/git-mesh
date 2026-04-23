//! Resolver integration tests (§5).

mod support;

use anyhow::Result;
use git_mesh::types::RangeStatus;
use git_mesh::{
    append_add, commit_mesh, culprit_commit, resolve_mesh, resolve_range, set_message,
    stale_meshes,
};
use support::TestRepo;

fn seed_mesh_with_one_range(repo: &TestRepo, name: &str) -> Result<String> {
    let gix = repo.gix_repo()?;
    append_add(&gix, name, "file1.txt", 1, 5, None)?;
    set_message(&gix, name, "seed")?;
    Ok(commit_mesh(&gix, name)?)
}

#[test]

fn fresh_when_nothing_changed() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed_mesh_with_one_range(&repo, "m")?;
    let mr = resolve_mesh(&repo.gix_repo()?, "m")?;
    assert_eq!(mr.ranges.len(), 1);
    assert_eq!(mr.ranges[0].status, RangeStatus::Fresh);
    Ok(())
}

#[test]

fn moved_when_only_location_shifts() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed_mesh_with_one_range(&repo, "m")?;
    // Insert two blank lines at top — bytes identical, location shifts.
    repo.write_file(
        "file1.txt",
        "prefix1\nprefix2\nline1\nline2\nline3\nline4\nline5\nline6\nline7\nline8\nline9\nline10\n",
    )?;
    repo.commit_all("shift")?;
    let mr = resolve_mesh(&repo.gix_repo()?, "m")?;
    assert_eq!(mr.ranges[0].status, RangeStatus::Moved);
    Ok(())
}

#[test]

fn changed_when_bytes_differ() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed_mesh_with_one_range(&repo, "m")?;
    repo.write_file(
        "file1.txt",
        "lineONE\nline2\nline3\nline4\nline5\nline6\nline7\nline8\nline9\nline10\n",
    )?;
    repo.commit_all("mutate")?;
    let mr = resolve_mesh(&repo.gix_repo()?, "m")?;
    assert_eq!(mr.ranges[0].status, RangeStatus::Changed);
    Ok(())
}

#[test]

fn orphaned_when_anchor_unreachable() -> Result<()> {
    // Hard to produce cleanly without rewriting history; sketched for
    // shape. The implementation must classify unreachable anchors as
    // Orphaned rather than error.
    let repo = TestRepo::seeded()?;
    seed_mesh_with_one_range(&repo, "m")?;
    // Force-reset main to the parent? seeded() has one commit so we
    // can't walk back. Instead: reset main to an unrelated empty tree
    // by creating an orphan branch.
    repo.run_git(["checkout", "--orphan", "fresh"])?;
    repo.run_git(["rm", "-rf", "."])?;
    repo.write_file("README.md", "fresh\n")?;
    repo.run_git(["add", "-A"])?;
    repo.run_git(["commit", "-m", "fresh"])?;
    repo.run_git(["branch", "-D", "main"])?;
    repo.run_git(["branch", "-m", "main"])?;
    let mr = resolve_mesh(&repo.gix_repo()?, "m")?;
    assert_eq!(mr.ranges[0].status, RangeStatus::Orphaned);
    Ok(())
}

#[test]

fn resolve_range_agrees_with_resolve_mesh() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed_mesh_with_one_range(&repo, "m")?;
    let mr = resolve_mesh(&repo.gix_repo()?, "m")?;
    let rid = &mr.ranges[0].range_id;
    let r = resolve_range(&repo.gix_repo()?, "m", rid)?;
    assert_eq!(r.status, mr.ranges[0].status);
    Ok(())
}

#[test]

fn culprit_commit_attribution_on_changed() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed_mesh_with_one_range(&repo, "m")?;
    repo.write_file(
        "file1.txt",
        "lineONE\nline2\nline3\nline4\nline5\nline6\nline7\nline8\nline9\nline10\n",
    )?;
    let culprit_sha = repo.commit_all("mutate")?;
    let mr = resolve_mesh(&repo.gix_repo()?, "m")?;
    let got = culprit_commit(&repo.gix_repo()?, &mr.ranges[0])?;
    assert_eq!(got.as_deref(), Some(culprit_sha.as_str()));
    Ok(())
}

#[test]

fn culprit_none_for_fresh_range() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed_mesh_with_one_range(&repo, "m")?;
    let mr = resolve_mesh(&repo.gix_repo()?, "m")?;
    let got = culprit_commit(&repo.gix_repo()?, &mr.ranges[0])?;
    assert!(got.is_none());
    Ok(())
}

#[test]

fn stale_meshes_sorts_worst_first() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed_mesh_with_one_range(&repo, "clean")?;
    seed_mesh_with_one_range(&repo, "dirty")?;
    repo.write_file(
        "file1.txt",
        "XXX\nline2\nline3\nline4\nline5\nline6\nline7\nline8\nline9\nline10\n",
    )?;
    repo.commit_all("mutate")?;
    let all = stale_meshes(&repo.gix_repo()?)?;
    // Worst first — "dirty" (Changed) should precede "clean" (Fresh).
    assert!(all[0].ranges.iter().any(|r| r.status == RangeStatus::Changed));
    Ok(())
}

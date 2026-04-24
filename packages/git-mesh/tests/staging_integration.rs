//! Staging area tests (§6.3, §6.4).

mod support;

use anyhow::Result;
use git_mesh::staging::StagedConfig;
use git_mesh::types::CopyDetection;
use git_mesh::{append_add, append_config, append_remove, clear_staging, read_staging, set_why};
use support::TestRepo;

#[test]

fn append_add_creates_ops_and_sidecar() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let gix = repo.gix_repo()?;
    append_add(&gix, "m", "file1.txt", 1, 5, None)?;
    let s = read_staging(&gix, "m")?;
    assert_eq!(s.adds.len(), 1);
    assert_eq!(s.adds[0].path, "file1.txt");
    assert_eq!((s.adds[0].start(), s.adds[0].end()), (1, 5));
    assert!(s.adds[0].anchor.is_none());
    // Sidecar present — N=1 for first staged add line.
    let sidecar = repo
        .path()
        .join(".git/mesh/staging")
        .join(format!("m.{}", s.adds[0].line_number));
    assert!(
        sidecar.exists(),
        "§6.3 sidecar must exist at .git/mesh/staging/<name>.<N>"
    );
    Ok(())
}

#[test]

fn append_add_with_explicit_anchor_records_sha() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let head = repo.head_sha()?;
    let gix = repo.gix_repo()?;
    append_add(&gix, "m", "file1.txt", 1, 5, Some(&head))?;
    let s = read_staging(&gix, "m")?;
    assert_eq!(s.adds[0].anchor.as_deref(), Some(head.as_str()));
    Ok(())
}

#[test]
fn append_add_rejects_missing_worktree_path() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let gix = repo.gix_repo()?;
    let err = append_add(&gix, "m", "no/such.txt", 1, 1, None).unwrap_err();
    assert!(matches!(
        err,
        git_mesh::Error::Io(_) | git_mesh::Error::PathNotInTree { .. }
    ));
    Ok(())
}

#[test]
fn append_add_rejects_end_past_eof() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let gix = repo.gix_repo()?;
    let err = append_add(&gix, "m", "file1.txt", 1, 999, None).unwrap_err();
    assert!(matches!(err, git_mesh::Error::InvalidRange { .. }));
    Ok(())
}

#[test]
fn append_add_round_trips_paths_with_spaces() -> Result<()> {
    let repo = TestRepo::seeded()?;
    repo.write_file("dir with spaces/file 3.txt", "line1\nline2\nline3\n")?;
    let gix = repo.gix_repo()?;
    append_add(&gix, "m", "dir with spaces/file 3.txt", 1, 2, None)?;
    let s = read_staging(&gix, "m")?;
    assert_eq!(s.adds.len(), 1);
    assert_eq!(s.adds[0].path, "dir with spaces/file 3.txt");
    Ok(())
}

#[test]

fn append_remove_records_line() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let gix = repo.gix_repo()?;
    append_remove(&gix, "m", "file1.txt", 1, 5)?;
    let s = read_staging(&gix, "m")?;
    assert_eq!(s.removes.len(), 1);
    assert_eq!(s.removes[0].path, "file1.txt");
    Ok(())
}

#[test]

fn append_config_records_entries() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let gix = repo.gix_repo()?;
    append_config(
        &gix,
        "m",
        &StagedConfig::CopyDetection(CopyDetection::AnyFileInRepo),
    )?;
    append_config(&gix, "m", &StagedConfig::IgnoreWhitespace(true))?;
    let s = read_staging(&gix, "m")?;
    assert_eq!(s.configs.len(), 2);
    Ok(())
}

#[test]

fn set_why_persists_file() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let gix = repo.gix_repo()?;
    set_why(&gix, "m", "Subject\n\nBody\n")?;
    let s = read_staging(&gix, "m")?;
    assert_eq!(s.why.as_deref(), Some("Subject\n\nBody\n"));
    Ok(())
}

#[test]

fn clear_staging_removes_all_files() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let gix = repo.gix_repo()?;
    append_add(&gix, "m", "file1.txt", 1, 5, None)?;
    set_why(&gix, "m", "msg")?;
    clear_staging(&gix, "m")?;
    let s = read_staging(&gix, "m")?;
    assert!(s.adds.is_empty() && s.why.is_none());
    Ok(())
}

#[test]

fn read_staging_empty_when_no_file() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let s = read_staging(&repo.gix_repo()?, "never-touched")?;
    assert!(s.adds.is_empty());
    assert!(s.removes.is_empty());
    assert!(s.configs.is_empty());
    assert!(s.why.is_none());
    Ok(())
}

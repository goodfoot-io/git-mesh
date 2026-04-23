//! Staging area tests (§6.3, §6.4).

mod support;

use anyhow::Result;
use git_mesh::staging::{StagedConfig, StatusView};
use git_mesh::types::CopyDetection;
use git_mesh::{
    append_add, append_config, append_remove, clear_staging, drift_check, read_staging,
    set_message, status_view,
};
use support::TestRepo;

#[test]

fn append_add_creates_ops_and_sidecar() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let gix = repo.gix_repo()?;
    append_add(&gix, "m", "file1.txt", 1, 5, None)?;
    let s = read_staging(&gix, "m")?;
    assert_eq!(s.adds.len(), 1);
    assert_eq!(s.adds[0].path, "file1.txt");
    assert_eq!((s.adds[0].start, s.adds[0].end), (1, 5));
    assert!(s.adds[0].anchor.is_none());
    // Sidecar present — N=1 for first staged add line.
    let sidecar = repo
        .path()
        .join(".git/mesh/staging")
        .join(format!("m.{}", s.adds[0].line_number));
    assert!(sidecar.exists(), "§6.3 sidecar must exist at .git/mesh/staging/<name>.<N>");
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

fn set_message_persists_file() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let gix = repo.gix_repo()?;
    set_message(&gix, "m", "Subject\n\nBody\n")?;
    let s = read_staging(&gix, "m")?;
    assert_eq!(s.message.as_deref(), Some("Subject\n\nBody\n"));
    Ok(())
}

#[test]

fn clear_staging_removes_all_files() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let gix = repo.gix_repo()?;
    append_add(&gix, "m", "file1.txt", 1, 5, None)?;
    set_message(&gix, "m", "msg")?;
    clear_staging(&gix, "m")?;
    let s = read_staging(&gix, "m")?;
    assert!(s.adds.is_empty() && s.message.is_none());
    Ok(())
}

#[test]

fn read_staging_empty_when_no_file() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let s = read_staging(&repo.gix_repo()?, "never-touched")?;
    assert!(s.adds.is_empty());
    assert!(s.removes.is_empty());
    assert!(s.configs.is_empty());
    assert!(s.message.is_none());
    Ok(())
}

#[test]

fn drift_check_negative_when_worktree_unchanged() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let gix = repo.gix_repo()?;
    append_add(&gix, "m", "file1.txt", 1, 5, None)?;
    assert!(drift_check(&gix, "m")?.is_empty());
    Ok(())
}

#[test]

fn drift_check_positive_when_worktree_mutated() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let gix = repo.gix_repo()?;
    append_add(&gix, "m", "file1.txt", 1, 5, None)?;
    // Mutate worktree AFTER staging.
    repo.write_file(
        "file1.txt",
        "DRIFT\nline2\nline3\nline4\nline5\nline6\nline7\nline8\nline9\nline10\n",
    )?;
    let findings = drift_check(&gix, "m")?;
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].path, "file1.txt");
    Ok(())
}

#[test]

fn drift_check_skips_explicit_anchor_adds() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let head = repo.head_sha()?;
    let gix = repo.gix_repo()?;
    append_add(&gix, "m", "file1.txt", 1, 5, Some(&head))?;
    repo.write_file(
        "file1.txt",
        "DRIFT\nline2\nline3\nline4\nline5\nline6\nline7\nline8\nline9\nline10\n",
    )?;
    // Explicit anchor => skipped by drift_check per §6.3.
    assert!(drift_check(&gix, "m")?.is_empty());
    Ok(())
}

#[test]

fn status_view_assembles_staging_and_drift() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let gix = repo.gix_repo()?;
    append_add(&gix, "m", "file1.txt", 1, 5, None)?;
    let v: StatusView = status_view(&gix, "m")?;
    assert_eq!(v.name, "m");
    assert_eq!(v.staging.adds.len(), 1);
    Ok(())
}

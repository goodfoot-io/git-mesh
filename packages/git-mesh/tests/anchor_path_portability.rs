//! Cross-OS mesh portability: anchor paths must persist in the canonical
//! POSIX, forward-slash, repo-relative form regardless of the separator the
//! author typed, so a mesh created on one OS resolves on the other.
//!
//! "Both directions":
//!  - Linux-authored (forward slash) — the canonical form — round-trips and
//!    resolves (this is what a Windows checkout reads).
//!  - Windows-authored (backslash) — normalized on write to forward slash —
//!    round-trips and resolves (this is what a Linux checkout reads).
//!
//! The git tree/index is forward-slash on every platform; a stored backslash
//! path would fail to resolve everywhere, so normalization is required at the
//! write boundary.

mod support;

use anyhow::Result;
use git_mesh::types::{AnchorStatus, EngineOptions};
use git_mesh::{append_add, append_add_whole, commit_mesh, read_mesh, resolve_mesh, set_why};
use support::TestRepo;

/// Seed a repo with a nested file so the separator actually matters
/// (`sub/dir/file.txt`), committed at HEAD.
fn seeded_nested() -> Result<TestRepo> {
    let repo = TestRepo::new()?;
    repo.write_file("sub/dir/file.txt", "a\nb\nc\nd\ne\n")?;
    repo.commit_all("seed nested file")?;
    Ok(repo)
}

fn stored_path(repo: &TestRepo, mesh: &str) -> Result<String> {
    let m = read_mesh(&repo.gix_repo()?, mesh)?;
    assert_eq!(m.anchors_v2.len(), 1, "expected exactly one anchor");
    Ok(m.anchors_v2[0].1.path.clone())
}

fn only_status(repo: &TestRepo, mesh: &str) -> Result<AnchorStatus> {
    // The layered engine requires a commit-graph (with changed-path bloom
    // filters); write it after all commits (file + mesh refs) exist.
    repo.write_commit_graph()?;
    let mr = resolve_mesh(&repo.gix_repo()?, mesh, EngineOptions::full())?;
    assert_eq!(mr.anchors.len(), 1, "expected exactly one resolved anchor");
    Ok(mr.anchors[0].status.clone())
}

/// Direction 1: forward-slash (Linux-authored / canonical) line anchor.
/// Stored verbatim and resolves Fresh.
#[test]
fn forward_slash_line_anchor_round_trips_and_resolves() -> Result<()> {
    let repo = seeded_nested()?;
    let gix = repo.gix_repo()?;
    append_add(&gix, "fwd", "sub/dir/file.txt", 1, 3, None)?;
    set_why(&gix, "fwd", "linux authored")?;
    commit_mesh(&gix, "fwd")?;

    assert_eq!(stored_path(&repo, "fwd")?, "sub/dir/file.txt");
    assert_eq!(only_status(&repo, "fwd")?, AnchorStatus::Fresh);
    Ok(())
}

/// Direction 2: backslash (Windows-authored) line anchor must be normalized
/// to forward slash on write — never persisted with a backslash — and must
/// resolve against the forward-slash git tree.
#[test]
fn backslash_line_anchor_is_normalized_and_resolves() -> Result<()> {
    let repo = seeded_nested()?;
    let gix = repo.gix_repo()?;
    append_add(&gix, "bwd", "sub\\dir\\file.txt", 1, 3, None)?;
    set_why(&gix, "bwd", "windows authored")?;
    commit_mesh(&gix, "bwd")?;

    let stored = stored_path(&repo, "bwd")?;
    assert!(
        !stored.contains('\\'),
        "stored anchor path must not contain a backslash, got `{stored}`"
    );
    assert_eq!(stored, "sub/dir/file.txt");
    assert_eq!(only_status(&repo, "bwd")?, AnchorStatus::Fresh);
    Ok(())
}

/// Both separator spellings must converge on the identical stored anchor so
/// the same logical anchor is portable across OSes (and last-write-wins /
/// supersede keys match).
#[test]
fn both_separators_produce_identical_canonical_storage() -> Result<()> {
    let repo = seeded_nested()?;
    let gix = repo.gix_repo()?;
    append_add(&gix, "fwd", "sub/dir/file.txt", 2, 4, None)?;
    set_why(&gix, "fwd", "fwd")?;
    commit_mesh(&gix, "fwd")?;
    append_add(&gix, "bwd", "sub\\dir\\file.txt", 2, 4, None)?;
    set_why(&gix, "bwd", "bwd")?;
    commit_mesh(&gix, "bwd")?;

    assert_eq!(stored_path(&repo, "fwd")?, stored_path(&repo, "bwd")?);
    assert_eq!(only_status(&repo, "fwd")?, AnchorStatus::Fresh);
    assert_eq!(only_status(&repo, "bwd")?, AnchorStatus::Fresh);
    Ok(())
}

/// Whole-file anchors travel the same `prepare_add` boundary, so a
/// backslash-authored whole-file pin is normalized and resolves too.
#[test]
fn backslash_whole_file_anchor_is_normalized_and_resolves() -> Result<()> {
    let repo = seeded_nested()?;
    let gix = repo.gix_repo()?;
    append_add_whole(&gix, "whole", "sub\\dir\\file.txt", None)?;
    set_why(&gix, "whole", "windows authored whole-file")?;
    commit_mesh(&gix, "whole")?;

    assert_eq!(stored_path(&repo, "whole")?, "sub/dir/file.txt");
    assert_eq!(only_status(&repo, "whole")?, AnchorStatus::Fresh);
    Ok(())
}

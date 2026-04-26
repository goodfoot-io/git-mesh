//! CLI commit-ish resolution surfaces curated error messages that
//! never leak `gix-revision` source paths or line numbers.

mod support;

use anyhow::Result;
use support::TestRepo;

fn assert_no_internal_leak(stderr: &str) {
    assert!(
        !stderr.contains("/.cargo/"),
        "stderr leaks Cargo registry path: {stderr}"
    );
    assert!(
        !stderr.contains(".rs:"),
        "stderr leaks Rust source file:line: {stderr}"
    );
    assert!(
        !stderr.contains("gix-revision"),
        "stderr leaks gix-revision crate name: {stderr}"
    );
}

#[test]
fn stale_since_unparseable_revision_is_curated() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let out = repo.run_mesh(["stale", "--since", "nonexistent"])?;
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert_no_internal_leak(&err);
    assert!(err.contains("--since"), "stderr should name --since: {err}");
    assert!(
        err.contains("nonexistent"),
        "stderr should name the input: {err}"
    );
    assert!(
        err.contains("not a valid revision"),
        "stderr should classify as unparseable: {err}"
    );
    Ok(())
}

#[test]
fn stale_since_nth_ancestor_overflow_is_curated() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let out = repo.run_mesh(["stale", "--since", "HEAD~99"])?;
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert_no_internal_leak(&err);
    assert!(err.contains("--since"), "stderr should name --since: {err}");
    assert!(
        err.contains("fewer than 99 ancestors"),
        "stderr should describe NthAncestor overflow: {err}"
    );
    Ok(())
}

#[test]
fn add_at_unparseable_revision_is_curated() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let out = repo.run_mesh(["add", "m", "file1.txt#L1-L1", "--at", "nonexistent"])?;
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert_no_internal_leak(&err);
    assert!(
        err.contains("not a valid revision"),
        "stderr should classify as unparseable: {err}"
    );
    Ok(())
}

#[test]
fn add_at_nth_ancestor_overflow_is_curated() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let out = repo.run_mesh(["add", "m", "file1.txt#L1-L1", "--at", "HEAD~99"])?;
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert_no_internal_leak(&err);
    assert!(
        err.contains("fewer than 99 ancestors"),
        "stderr should describe NthAncestor overflow: {err}"
    );
    Ok(())
}

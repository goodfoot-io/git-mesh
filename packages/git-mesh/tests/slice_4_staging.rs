//! Slice 4 — staging-area awareness in `git mesh advice` (file-backed).
//!
//! T8 (staging cross-cut) and T9 (empty-mesh risk) translated to the
//! file-backed pipeline. Detector inputs are loaded from
//! `.git/mesh/staging` directly by `run_advice_render`.

mod support;

use anyhow::Result;
use git_mesh::staging::{append_add, append_remove};
use git_mesh::{commit_mesh, set_why};
use std::process::Output;
use support::TestRepo;
use uuid::Uuid;

fn sid(prefix: &str) -> String {
    format!("slice4-{prefix}-{}", Uuid::new_v4())
}

fn ok(out: &Output) {
    assert!(
        out.status.success(),
        "code={:?} stderr={} stdout={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&out.stdout)
    );
}

fn run_advice(repo: &TestRepo, s: &str, extra: &[&str]) -> Result<Output> {
    let mut args: Vec<String> = vec!["advice".into(), s.into()];
    for a in extra {
        args.push((*a).to_string());
    }
    repo.run_mesh(args)
}

fn render(repo: &TestRepo, s: &str) -> Result<String> {
    let out = run_advice(repo, s, &[])?;
    ok(&out);
    Ok(String::from_utf8(out.stdout)?)
}

#[test]
fn t8_staging_cross_cut_surfaces() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let gix = repo.gix_repo()?;

    // Mesh-A owns file1#L1-L5.
    append_add(&gix, "mesh-a", "file1.txt", 1, 5, None)?;
    set_why(&gix, "mesh-a", "owner of file1 range")?;
    commit_mesh(&gix, "mesh-a")?;

    // Mesh-B exists; stages an overlapping add on mesh-a's file.
    append_add(&gix, "mesh-b", "file2.txt", 1, 5, None)?;
    set_why(&gix, "mesh-b", "second mesh")?;
    commit_mesh(&gix, "mesh-b")?;
    append_add(&gix, "mesh-b", "file1.txt", 3, 7, None)?;

    let s = sid("t8");
    ok(&run_advice(&repo, &s, &["snapshot"])?);
    ok(&run_advice(&repo, &s, &["read", "file1.txt"])?);

    let stdout = render(&repo, &s)?;
    // Staging cross-cut detector surfaces the overlapping mesh name.
    assert!(
        stdout.contains("mesh-a") || stdout.contains("mesh-b"),
        "expected staging cross-cut to surface either mesh, got:\n{stdout}"
    );
    Ok(())
}

#[test]
fn t9_empty_mesh_risk_when_remove_empties_mesh() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let gix = repo.gix_repo()?;
    append_add(&gix, "soon-empty", "file1.txt", 1, 5, None)?;
    set_why(&gix, "soon-empty", "single range")?;
    commit_mesh(&gix, "soon-empty")?;

    // Stage removal of the only range.
    append_remove(&gix, "soon-empty", "file1.txt", 1, 5)?;

    let s = sid("t9");
    ok(&run_advice(&repo, &s, &["snapshot"])?);
    ok(&run_advice(&repo, &s, &["read", "file1.txt"])?);

    let stdout = render(&repo, &s)?;
    assert!(
        stdout.contains("soon-empty") || stdout.contains("empty"),
        "expected empty-mesh hint, got:\n{stdout}"
    );
    Ok(())
}

#[test]
fn staged_overlap_dedups_within_session() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let gix = repo.gix_repo()?;

    append_add(&gix, "mesh-a", "file1.txt", 1, 5, None)?;
    set_why(&gix, "mesh-a", "owner")?;
    commit_mesh(&gix, "mesh-a")?;

    append_add(&gix, "mesh-b", "file2.txt", 1, 5, None)?;
    set_why(&gix, "mesh-b", "other")?;
    commit_mesh(&gix, "mesh-b")?;
    append_add(&gix, "mesh-b", "file1.txt", 3, 7, None)?;

    let s = sid("t8-dedup");
    ok(&run_advice(&repo, &s, &["snapshot"])?);
    ok(&run_advice(&repo, &s, &["read", "file1.txt"])?);
    let first = render(&repo, &s)?;
    assert!(!first.is_empty(), "first render must produce output");

    let second = render(&repo, &s)?;
    assert!(
        second.is_empty(),
        "second render with no new triggers must be silent, got:\n{second}"
    );
    Ok(())
}

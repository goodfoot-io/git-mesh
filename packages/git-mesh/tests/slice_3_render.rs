//! Render-output behaviors for the four-verb advice CLI: `read` surfaces
//! mesh partners on first hit, dedupes via `meshes-seen.jsonl` on the
//! second hit (within the same session), and isolates the seen set across
//! distinct sessions.

mod support;

use anyhow::Result;
use git_mesh::{append_add, commit_mesh, set_why};
use std::process::Output;
use support::TestRepo;
use uuid::Uuid;

fn sid(prefix: &str) -> String {
    format!("slice3-{prefix}-{}", Uuid::new_v4())
}

fn run_advice(repo: &TestRepo, session: &str, extra: &[&str]) -> Result<Output> {
    let mut args: Vec<String> = vec!["advice".into(), session.into()];
    for a in extra {
        args.push((*a).into());
    }
    repo.run_mesh(args)
}

fn ok(out: &Output) {
    assert!(
        out.status.success(),
        "expected success, code={:?} stderr={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn read_intersects_mesh_surfaces_partner() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let gix = repo.gix_repo()?;
    append_add(&gix, "m1", "file1.txt", 1, 5, None)?;
    append_add(&gix, "m1", "file2.txt", 1, 5, None)?;
    set_why(&gix, "m1", "two-file partnership")?;
    commit_mesh(&gix, "m1")?;

    let s = sid("partner");
    let out = run_advice(&repo, &s, &["read", "file1.txt#L1-L5"])?;
    ok(&out);
    let stdout = String::from_utf8(out.stdout)?;
    assert!(
        stdout.contains("is in the m1 mesh with:") && stdout.contains("two-file partnership"),
        "expected mesh why, got:\n{stdout}"
    );
    assert!(
        stdout.contains("- file2.txt#L1-L5"),
        "expected partner mention, got:\n{stdout}"
    );
    for line in stdout.lines() {
        assert!(!line.starts_with("# "), "line is `# `-prefixed: {line:?}");
    }
    Ok(())
}

#[test]
fn second_read_of_same_path_dedupes_via_meshes_seen() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let gix = repo.gix_repo()?;
    append_add(&gix, "dd", "file1.txt", 1, 5, None)?;
    append_add(&gix, "dd", "file2.txt", 1, 5, None)?;
    set_why(&gix, "dd", "dedup")?;
    commit_mesh(&gix, "dd")?;

    let s = sid("dedup-same");
    let first = run_advice(&repo, &s, &["read", "file1.txt#L1-L5"])?;
    ok(&first);
    let first_out = String::from_utf8(first.stdout)?;
    assert!(!first_out.is_empty(), "first read should produce output");

    let second = run_advice(&repo, &s, &["read", "file1.txt#L1-L5"])?;
    ok(&second);
    assert!(
        second.stdout.is_empty(),
        "second read of same path within session must dedupe via meshes-seen, got:\n{}",
        String::from_utf8_lossy(&second.stdout)
    );
    Ok(())
}

#[test]
fn read_of_partner_path_does_not_resurface_already_seen_mesh() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let gix = repo.gix_repo()?;
    append_add(&gix, "dd2", "file1.txt", 1, 5, None)?;
    append_add(&gix, "dd2", "file2.txt", 1, 5, None)?;
    set_why(&gix, "dd2", "new-trigger")?;
    commit_mesh(&gix, "dd2")?;

    let s = sid("new-trigger");
    let _ = run_advice(&repo, &s, &["read", "file1.txt#L1-L5"])?;

    let out = run_advice(&repo, &s, &["read", "file2.txt#L1-L5"])?;
    ok(&out);
    let stdout = String::from_utf8(out.stdout)?;
    assert!(
        stdout.is_empty(),
        "mesh already seen this session must not re-surface; got:\n{stdout}"
    );
    Ok(())
}

#[test]
fn read_with_no_meshes_renders_silent() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let s = sid("empty");
    repo.write_file("empty-target.txt", "x\n")?;
    let out = run_advice(&repo, &s, &["read", "empty-target.txt"])?;
    ok(&out);
    assert!(out.stdout.is_empty(), "no meshes → silent render");
    Ok(())
}

#[test]
fn isolated_sessions_do_not_share_seen_set() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let gix = repo.gix_repo()?;
    append_add(&gix, "iso", "file1.txt", 1, 5, None)?;
    append_add(&gix, "iso", "file2.txt", 1, 5, None)?;
    set_why(&gix, "iso", "isolation")?;
    commit_mesh(&gix, "iso")?;

    let s1 = sid("iso-a");
    let s2 = sid("iso-b");

    let a1 = run_advice(&repo, &s1, &["read", "file1.txt#L1-L5"])?;
    ok(&a1);
    assert!(
        !a1.stdout.is_empty(),
        "session A first read produces output"
    );

    let b1 = run_advice(&repo, &s2, &["read", "file1.txt#L1-L5"])?;
    ok(&b1);
    assert!(
        !b1.stdout.is_empty(),
        "session B should see fresh output despite A's prior render"
    );
    Ok(())
}

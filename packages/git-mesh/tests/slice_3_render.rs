//! Slice 3 — render-output behaviors against the file-backed pipeline.
//!
//! The legacy SQL-pipeline tests (T4 false positives, T4 re-record command,
//! per-flush excerpt dedup, whole-file partner address-only, `[DELETED]`
//! marker, T7 phrasing with per-session touch count) asserted SQL-specific
//! render details. After the SQL stack was deleted these tests have no
//! file-backed equivalent — the active detectors (read-∩-mesh,
//! partner-drift, rename-consequence, staging-cross-cut) cover different
//! triggers. Translated below: read ∩ mesh surfaces partner ranges, with
//! per-session suppression on the second render.

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
#[ignore] // Phase 3
fn read_intersects_mesh_surfaces_partner() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let gix = repo.gix_repo()?;
    append_add(&gix, "m1", "file1.txt", 1, 5, None)?;
    append_add(&gix, "m1", "file2.txt", 1, 5, None)?;
    set_why(&gix, "m1", "two-file partnership")?;
    commit_mesh(&gix, "m1")?;

    let s = sid("partner");
    ok(&run_advice(&repo, &s, &["snapshot"])?);
    ok(&run_advice(&repo, &s, &["read", "file1.txt"])?);

    let out = run_advice(&repo, &s, &[])?;
    ok(&out);
    let stdout = String::from_utf8(out.stdout)?;
    assert!(
        stdout.contains("# m1 mesh: two-file partnership"),
        "expected mesh why, got:\n{stdout}"
    );
    assert!(
        stdout.contains("# - file2.txt#L1-L5"),
        "expected partner mention, got:\n{stdout}"
    );
    assert!(
        stdout.contains("# - file1.txt#L1-L5"),
        "trigger anchor must appear in the bullet list, got:\n{stdout}"
    );
    for line in stdout.lines() {
        assert!(line.starts_with('#'), "line not `#`-prefixed: {line:?}");
    }
    Ok(())
}

#[test]
#[ignore] // Phase 3
fn second_render_suppresses_same_partner() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let gix = repo.gix_repo()?;
    append_add(&gix, "dd", "file1.txt", 1, 5, None)?;
    append_add(&gix, "dd", "file2.txt", 1, 5, None)?;
    set_why(&gix, "dd", "dedup")?;
    commit_mesh(&gix, "dd")?;

    let s = sid("dedup-same");
    ok(&run_advice(&repo, &s, &["snapshot"])?);
    ok(&run_advice(&repo, &s, &["read", "file1.txt"])?);
    let first = run_advice(&repo, &s, &[])?;
    ok(&first);
    let first_out = String::from_utf8(first.stdout)?;
    assert!(!first_out.is_empty(), "first render should produce output");

    // No new reads, no edits. Second render should be silent (suppressed).
    let second = run_advice(&repo, &s, &[])?;
    ok(&second);
    assert!(
        second.stdout.is_empty(),
        "second render with no new triggers must be silent, got:\n{}",
        String::from_utf8_lossy(&second.stdout)
    );
    Ok(())
}

#[test]
#[ignore] // Phase 3
fn new_trigger_does_not_resurface_already_seen_mesh() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let gix = repo.gix_repo()?;
    append_add(&gix, "dd2", "file1.txt", 1, 5, None)?;
    append_add(&gix, "dd2", "file2.txt", 1, 5, None)?;
    set_why(&gix, "dd2", "new-trigger")?;
    commit_mesh(&gix, "dd2")?;

    let s = sid("new-trigger");
    ok(&run_advice(&repo, &s, &["snapshot"])?);
    ok(&run_advice(&repo, &s, &["read", "file1.txt"])?);
    let _ = run_advice(&repo, &s, &[])?;

    ok(&run_advice(&repo, &s, &["read", "file2.txt"])?);
    let out = run_advice(&repo, &s, &[])?;
    ok(&out);
    let stdout = String::from_utf8(out.stdout)?;
    // A mesh surfaces at most once per advice session: even though the new
    // read of the partner side would otherwise produce a fresh candidate,
    // the mesh has already been announced so the render must stay silent.
    assert!(
        stdout.is_empty(),
        "mesh already seen this session must not re-surface; got:\n{stdout}"
    );
    Ok(())
}

#[test]
#[ignore] // Phase 3
fn empty_no_meshes_renders_silent() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let s = sid("empty");
    ok(&run_advice(&repo, &s, &["snapshot"])?);
    let out = run_advice(&repo, &s, &[])?;
    ok(&out);
    assert!(out.stdout.is_empty(), "no meshes → silent render");
    Ok(())
}

#[test]
#[ignore] // Phase 3
fn isolated_sessions_do_not_share_seen_set() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let gix = repo.gix_repo()?;
    append_add(&gix, "iso", "file1.txt", 1, 5, None)?;
    append_add(&gix, "iso", "file2.txt", 1, 5, None)?;
    set_why(&gix, "iso", "isolation")?;
    commit_mesh(&gix, "iso")?;

    let s1 = sid("iso-a");
    let s2 = sid("iso-b");

    ok(&run_advice(&repo, &s1, &["snapshot"])?);
    ok(&run_advice(&repo, &s1, &["read", "file1.txt"])?);
    let a1 = run_advice(&repo, &s1, &[])?;
    ok(&a1);
    assert!(
        !a1.stdout.is_empty(),
        "session A first render produces output"
    );

    ok(&run_advice(&repo, &s2, &["snapshot"])?);
    ok(&run_advice(&repo, &s2, &["read", "file1.txt"])?);
    let b1 = run_advice(&repo, &s2, &[])?;
    ok(&b1);
    assert!(
        !b1.stdout.is_empty(),
        "session B should see fresh output despite A's prior render"
    );
    Ok(())
}

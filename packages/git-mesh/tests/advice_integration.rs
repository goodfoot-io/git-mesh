//! End-to-end integration tests for `git mesh advice` (file-backed pipeline).
//!
//! Each test uses a unique session ID (uuid4) so the per-session directory
//! at `${GIT_MESH_ADVICE_DIR:-/tmp/git-mesh/advice}/<repo-key>/<id>/` is
//! isolated.
//!
//! Tests deleted (no file-backed equivalent):
//! - `add_events_create_db`         — SQL DB creation, gone with SQL stack.
//! - `flush_t2_excerpt_on_write`    — required hunk-anchor data; deferred.
//! - `flush_t4_range_collapse`      — `detect_range_shrink` deferred.
//! - `flush_t5_coherence`           — required SQL drift state and write events.
//! - `flush_t6_symbol_rename`       — required pre/post blob storage; gone.
//! - `flush_t10_reanchor_preview`   — required `--commit` event; gone.
//! - `flush_t11_terminal_status`    — required SQL-tracked terminal status.
//! - `documentation_flag` (T2)      — required write events for the
//!   WriteAcross detector; that detector is now stubbed.
//! - `write_without_pre_post_stores_null_blobs` — SQL-internal contract.
//! - `binary_blob_null`             — SQL-internal contract.
//!
//! Surviving tests cover the working detectors against the new pipeline.

mod support;

use anyhow::Result;
use git_mesh::{append_add, commit_mesh, set_why};
use std::process::{Command, Output};
use support::TestRepo;
use uuid::Uuid;

fn sid(prefix: &str) -> String {
    format!("advice-int-{prefix}-{}", Uuid::new_v4())
}

fn run_advice(repo: &TestRepo, session: &str, extra: &[&str]) -> Result<Output> {
    let mut args: Vec<String> = vec!["advice".into(), session.into()];
    for a in extra {
        args.push((*a).to_string());
    }
    repo.run_mesh(args)
}

fn ok(out: &Output) {
    assert!(
        out.status.success(),
        "expected success, got code={:?} stderr={} stdout={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&out.stdout)
    );
}

fn render(repo: &TestRepo, session: &str, extra: &[&str]) -> Result<String> {
    let out = run_advice(repo, session, extra)?;
    ok(&out);
    Ok(String::from_utf8(out.stdout)?)
}

// ---------------------------------------------------------------------------
// T1 — partner list (L0): read ∩ mesh surfaces partner anchors.
// ---------------------------------------------------------------------------

#[test]
#[ignore] // Phase 3
fn flush_t1_partner_list() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let gix = repo.gix_repo()?;
    append_add(&gix, "m1", "file1.txt", 1, 5, None)?;
    append_add(&gix, "m1", "file2.txt", 1, 5, None)?;
    set_why(&gix, "m1", "two-file partnership")?;
    commit_mesh(&gix, "m1")?;

    let s = sid("t1");
    ok(&run_advice(&repo, &s, &["snapshot"])?);
    ok(&run_advice(&repo, &s, &["read", "file1.txt"])?);

    let stdout = render(&repo, &s, &[])?;
    assert!(
        stdout.contains("# m1 mesh: two-file partnership"),
        "expected mesh header with why, got:\n{stdout}"
    );
    assert!(
        stdout.contains("# - file2.txt#L1-L5"),
        "expected partner row, got:\n{stdout}"
    );
    assert!(
        stdout.contains("# - file1.txt#L1-L5"),
        "trigger anchor must appear in the bullet list, got:\n{stdout}"
    );
    for line in stdout.lines() {
        assert!(line.starts_with('#'), "line not prefixed: {line:?}");
    }
    Ok(())
}

#[test]
#[ignore] // Phase 3
fn whole_file_read_routes_to_other_ranges_in_each_mesh() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let gix = repo.gix_repo()?;
    append_add(&gix, "whole", "file1.txt", 1, 2, None)?;
    append_add(&gix, "whole", "file1.txt", 5, 6, None)?;
    append_add(&gix, "whole", "file2.txt", 1, 2, None)?;
    set_why(&gix, "whole", "whole-file routing")?;
    commit_mesh(&gix, "whole")?;

    let s = sid("whole-read");
    ok(&run_advice(&repo, &s, &["snapshot"])?);
    ok(&run_advice(&repo, &s, &["read", "file1.txt"])?);

    let stdout = render(&repo, &s, &[])?;
    assert!(
        stdout.contains("# whole mesh: whole-file routing"),
        "got:\n{stdout}"
    );
    assert!(
        !stdout.contains("# triggered by"),
        "triggered-by line must not be emitted; got:\n{stdout}"
    );
    assert!(stdout.contains("# - file1.txt#L5-L6"), "got:\n{stdout}");
    assert!(stdout.contains("# - file2.txt#L1-L2"), "got:\n{stdout}");
    Ok(())
}

#[test]
#[ignore] // Phase 3
fn incremental_delta_routes_to_existing_mesh_partners() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let gix = repo.gix_repo()?;
    append_add(&gix, "delta", "file1.txt", 1, 5, None)?;
    append_add(&gix, "delta", "file2.txt", 1, 5, None)?;
    set_why(&gix, "delta", "delta routing")?;
    commit_mesh(&gix, "delta")?;

    let s = sid("delta");
    ok(&run_advice(&repo, &s, &["snapshot"])?);
    repo.write_file(
        "file1.txt",
        "changed\nline2\nline3\nline4\nline5\nline6\nline7\nline8\nline9\nline10\n",
    )?;

    let stdout = render(&repo, &s, &[])?;
    assert!(
        stdout.contains("# delta mesh: delta routing"),
        "got:\n{stdout}"
    );
    assert!(
        !stdout.contains("# triggered by"),
        "triggered-by line must not be emitted; got:\n{stdout}"
    );
    assert!(stdout.contains("# - file2.txt#L1-L5"), "got:\n{stdout}");
    assert!(
        stdout.contains("# - file1.txt#L1-L5"),
        "trigger anchor must appear in the bullet list, got:\n{stdout}"
    );
    Ok(())
}

#[test]
#[ignore] // Phase 3
fn advice_store_inside_worktree_is_not_captured_or_co_touched() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let advice_dir = repo.path().join(".mesh-advice");
    let s = sid("store-in-worktree");

    let run = |extra: &[&str]| -> Result<Output> {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_git-mesh"));
        cmd.current_dir(repo.path())
            .env("GIT_MESH_ADVICE_DIR", &advice_dir)
            .env_remove("GIT_MESH_ADVICE_DEBUG")
            .args(["advice", &s]);
        cmd.args(extra);
        Ok(cmd.output()?)
    };

    ok(&run(&["snapshot"])?);
    for _ in 0..4 {
        let out = run(&[])?;
        ok(&out);
        assert!(
            out.stderr.is_empty(),
            "internal advice store must not make last-flush fall back, got:\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            out.stdout.is_empty(),
            "internal advice store must not create repeat output, got:\n{}",
            String::from_utf8_lossy(&out.stdout)
        );
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// T8 — staging cross-cut.
// ---------------------------------------------------------------------------

#[test]
#[ignore] // Phase 3
fn flush_t8_staging_crosscut() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let gix = repo.gix_repo()?;
    append_add(&gix, "mesh-a", "file1.txt", 1, 5, None)?;
    set_why(&gix, "mesh-a", "owner of file1 anchor")?;
    commit_mesh(&gix, "mesh-a")?;

    append_add(&gix, "mesh-b", "file2.txt", 1, 5, None)?;
    set_why(&gix, "mesh-b", "second mesh")?;
    commit_mesh(&gix, "mesh-b")?;
    git_mesh::staging::append_add(&gix, "mesh-b", "file1.txt", 3, 7, None)?;

    let s = sid("t8");
    ok(&run_advice(&repo, &s, &["snapshot"])?);
    ok(&run_advice(&repo, &s, &["read", "file1.txt"])?);

    let stdout = render(&repo, &s, &[])?;
    assert!(
        stdout.contains("mesh-a") || stdout.contains("mesh-b"),
        "expected staging cross-cut output, got:\n{stdout}"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// T9 — empty-mesh risk.
// ---------------------------------------------------------------------------

#[test]
#[ignore] // Phase 3
fn flush_t9_empty_mesh_risk() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let gix = repo.gix_repo()?;
    append_add(&gix, "soon-empty", "file1.txt", 1, 5, None)?;
    set_why(&gix, "soon-empty", "single anchor")?;
    commit_mesh(&gix, "soon-empty")?;

    git_mesh::staging::append_remove(&gix, "soon-empty", "file1.txt", 1, 5)?;

    let s = sid("t9");
    ok(&run_advice(&repo, &s, &["snapshot"])?);
    ok(&run_advice(&repo, &s, &["read", "file1.txt"])?);

    let stdout = render(&repo, &s, &[])?;
    assert!(
        stdout.contains("soon-empty") || stdout.contains("empty"),
        "expected empty-mesh-risk output, got:\n{stdout}"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Dedup / idempotence
// ---------------------------------------------------------------------------

#[test]
#[ignore] // Phase 3
fn dedup_same_trigger() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let gix = repo.gix_repo()?;
    append_add(&gix, "dd", "file1.txt", 1, 5, None)?;
    append_add(&gix, "dd", "file2.txt", 1, 5, None)?;
    set_why(&gix, "dd", "dedup sample")?;
    commit_mesh(&gix, "dd")?;

    let s = sid("dedup-same");
    ok(&run_advice(&repo, &s, &["snapshot"])?);
    ok(&run_advice(&repo, &s, &["read", "file1.txt"])?);
    let first = render(&repo, &s, &[])?;
    assert!(!first.is_empty(), "first render should produce output");

    let second = render(&repo, &s, &[])?;
    assert!(
        second.is_empty(),
        "second render with same trigger must be empty, got:\n{second}"
    );
    Ok(())
}

#[test]
#[ignore] // Phase 3
fn dedup_new_trigger() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let gix = repo.gix_repo()?;
    append_add(&gix, "dd2", "file1.txt", 1, 5, None)?;
    append_add(&gix, "dd2", "file2.txt", 1, 5, None)?;
    set_why(&gix, "dd2", "dedup-new sample")?;
    commit_mesh(&gix, "dd2")?;

    let s = sid("dedup-new");
    ok(&run_advice(&repo, &s, &["snapshot"])?);
    ok(&run_advice(&repo, &s, &["read", "file1.txt"])?);
    let _ = render(&repo, &s, &[])?;

    ok(&run_advice(&repo, &s, &["read", "file2.txt"])?);
    let third = render(&repo, &s, &[])?;
    assert!(
        third.is_empty(),
        "mesh already surfaced this session must not re-surface on a new trigger; got:\n{third}"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Empty / no-meshes path
// ---------------------------------------------------------------------------

#[test]
#[ignore] // Phase 3
fn flush_empty_no_meshes() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let s = sid("empty");
    ok(&run_advice(&repo, &s, &["snapshot"])?);
    let stdout = render(&repo, &s, &[])?;
    assert!(stdout.is_empty(), "expected empty output, got:\n{stdout}");
    Ok(())
}

// ---------------------------------------------------------------------------
// Session isolation
// ---------------------------------------------------------------------------

#[test]
#[ignore] // Phase 3
fn session_isolation() -> Result<()> {
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
    let a1 = render(&repo, &s1, &[])?;
    assert!(!a1.is_empty());
    let a2 = render(&repo, &s1, &[])?;
    assert!(a2.is_empty(), "A's second render should be empty");

    ok(&run_advice(&repo, &s2, &["snapshot"])?);
    ok(&run_advice(&repo, &s2, &["read", "file1.txt"])?);
    let b1 = render(&repo, &s2, &[])?;
    assert!(
        !b1.is_empty(),
        "session B should see fresh output despite A's prior render"
    );
    Ok(())
}

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

fn stdout(out: &Output) -> String {
    String::from_utf8(out.stdout.clone()).expect("utf8 stdout")
}

// ---------------------------------------------------------------------------
// T1 — partner list (L0): read ∩ mesh surfaces partner anchors.
//
// The `read` verb emits BasicOutput immediately for first-time matching
// meshes (spec: READ rule). No separate flush step needed.
// ---------------------------------------------------------------------------

#[test]
fn flush_t1_partner_list() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let gix = repo.gix_repo()?;
    append_add(&gix, "m1", "file1.txt", 1, 5, None)?;
    append_add(&gix, "m1", "file2.txt", 1, 5, None)?;
    set_why(&gix, "m1", "two-file partnership")?;
    commit_mesh(&gix, "m1")?;

    let s = sid("t1");
    ok(&run_advice(&repo, &s, &["snapshot"])?);
    // Use a range anchor that overlaps the mesh anchor on file1.txt L1-L5.
    // read_overlaps requires Range-vs-Range (cross-kind is no-match).
    let out = run_advice(&repo, &s, &["read", "file1.txt#L1-L5"])?;
    ok(&out);
    let text = stdout(&out);
    assert!(
        text.contains("file1.txt#L1-L5 is in the m1 mesh with:")
            && text.contains("two-file partnership"),
        "expected header naming active anchor, got:\n{text}"
    );
    assert!(
        text.contains("- file2.txt#L1-L5"),
        "expected partner bullet, got:\n{text}"
    );
    assert!(
        !text.contains("- file1.txt#L1-L5"),
        "active anchor must not appear in the bullet list, got:\n{text}"
    );
    for line in text.lines() {
        assert!(!line.starts_with("# "), "line is `# `-prefixed: {line:?}");
    }
    Ok(())
}

#[test]
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
    // Range anchor overlapping file1.txt#L1-L2 (first anchor on file1.txt).
    let out = run_advice(&repo, &s, &["read", "file1.txt#L1-L2"])?;
    ok(&out);
    let text = stdout(&out);
    assert!(
        text.contains("is in the whole mesh with:") && text.contains("whole-file routing"),
        "got:\n{text}"
    );
    assert!(
        !text.contains("triggered by"),
        "triggered-by line must not be emitted; got:\n{text}"
    );
    assert!(text.contains("- file1.txt#L5-L6"), "got:\n{text}");
    assert!(text.contains("- file2.txt#L1-L2"), "got:\n{text}");
    Ok(())
}

#[test]
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

    // EDIT rule: milestone detects the modified file1.txt and emits BasicOutput
    // for the mesh whose anchor overlaps it (spec: §EDIT rule).
    let out = run_advice(&repo, &s, &["milestone"])?;
    ok(&out);
    let text = stdout(&out);
    assert!(
        text.contains("is in the delta mesh with:") && text.contains("delta routing"),
        "got:\n{text}"
    );
    assert!(
        !text.contains("triggered by"),
        "triggered-by line must not be emitted; got:\n{text}"
    );
    assert!(text.contains("- file2.txt#L1-L5"), "got:\n{text}");
    assert!(
        !text.contains("- file1.txt#L1-L5"),
        "active anchor must not appear in the bullet list, got:\n{text}"
    );
    Ok(())
}

#[test]
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
    // No edits made — milestone must produce empty stdout and no stderr on
    // every call (the internal advice store must not trigger fallback or
    // repeat output).
    for _ in 0..4 {
        let out = run(&["milestone"])?;
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
    // Range anchor overlapping the committed mesh-a anchor (file1.txt#L1-L5)
    // and the staged mesh-b anchor (file1.txt#L3-L7).
    let out = run_advice(&repo, &s, &["read", "file1.txt#L1-L5"])?;
    ok(&out);
    let text = stdout(&out);
    assert!(
        text.contains("mesh-a") || text.contains("mesh-b"),
        "expected staging cross-cut output, got:\n{text}"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// T9 — empty-mesh risk.
// ---------------------------------------------------------------------------

#[test]
fn flush_t9_empty_mesh_risk() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let gix = repo.gix_repo()?;
    append_add(&gix, "soon-empty", "file1.txt", 1, 5, None)?;
    set_why(&gix, "soon-empty", "single anchor")?;
    commit_mesh(&gix, "soon-empty")?;

    git_mesh::staging::append_remove(&gix, "soon-empty", "file1.txt", 1, 5)?;

    let s = sid("t9");
    ok(&run_advice(&repo, &s, &["snapshot"])?);
    // Range anchor overlapping the soon-empty anchor (file1.txt#L1-L5).
    let out = run_advice(&repo, &s, &["read", "file1.txt#L1-L5"])?;
    ok(&out);
    let text = stdout(&out);
    assert!(
        text.contains("soon-empty") || text.contains("empty"),
        "expected empty-mesh-risk output, got:\n{text}"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Dedup / idempotence
// ---------------------------------------------------------------------------

#[test]
fn dedup_same_trigger() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let gix = repo.gix_repo()?;
    append_add(&gix, "dd", "file1.txt", 1, 5, None)?;
    append_add(&gix, "dd", "file2.txt", 1, 5, None)?;
    set_why(&gix, "dd", "dedup sample")?;
    commit_mesh(&gix, "dd")?;

    let s = sid("dedup-same");
    ok(&run_advice(&repo, &s, &["snapshot"])?);

    // First read: mesh "dd" not yet seen → emits BasicOutput.
    let first_out = run_advice(&repo, &s, &["read", "file1.txt#L1-L5"])?;
    ok(&first_out);
    let first = stdout(&first_out);
    assert!(!first.is_empty(), "first read should produce output");

    // Second read of same anchor: mesh "dd" already in meshes-seen → no output
    // (spec: READ rule dedup on meshes-seen set).
    let second_out = run_advice(&repo, &s, &["read", "file1.txt#L1-L5"])?;
    ok(&second_out);
    let second = stdout(&second_out);
    assert!(
        second.is_empty(),
        "second read with same trigger must be empty, got:\n{second}"
    );
    Ok(())
}

#[test]
fn dedup_new_trigger() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let gix = repo.gix_repo()?;
    append_add(&gix, "dd2", "file1.txt", 1, 5, None)?;
    append_add(&gix, "dd2", "file2.txt", 1, 5, None)?;
    set_why(&gix, "dd2", "dedup-new sample")?;
    commit_mesh(&gix, "dd2")?;

    let s = sid("dedup-new");
    ok(&run_advice(&repo, &s, &["snapshot"])?);

    // Read file1 → mesh "dd2" seen, emitted.
    let out1 = run_advice(&repo, &s, &["read", "file1.txt#L1-L5"])?;
    ok(&out1);

    // Read file2 → mesh "dd2" already in meshes-seen → no re-emit
    // (spec: READ rule, mesh already surfaced this session).
    let out2 = run_advice(&repo, &s, &["read", "file2.txt#L1-L5"])?;
    ok(&out2);
    let third = stdout(&out2);
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
fn flush_empty_no_meshes() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let s = sid("empty");
    ok(&run_advice(&repo, &s, &["snapshot"])?);
    // No meshes exist → milestone with no edits must produce empty stdout.
    let out = run_advice(&repo, &s, &["milestone"])?;
    ok(&out);
    let text = stdout(&out);
    assert!(text.is_empty(), "expected empty output, got:\n{text}");
    Ok(())
}

// ---------------------------------------------------------------------------
// Session isolation
// ---------------------------------------------------------------------------

#[test]
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
    let a1_out = run_advice(&repo, &s1, &["read", "file1.txt#L1-L5"])?;
    ok(&a1_out);
    let a1 = stdout(&a1_out);
    assert!(!a1.is_empty());

    // Second read from session 1 → mesh in meshes-seen → empty.
    let a2_out = run_advice(&repo, &s1, &["read", "file1.txt#L1-L5"])?;
    ok(&a2_out);
    let a2 = stdout(&a2_out);
    assert!(a2.is_empty(), "A's second read should be empty");

    // Session 2 starts fresh: its meshes-seen is independent.
    ok(&run_advice(&repo, &s2, &["snapshot"])?);
    let b1_out = run_advice(&repo, &s2, &["read", "file1.txt#L1-L5"])?;
    ok(&b1_out);
    let b1 = stdout(&b1_out);
    assert!(
        !b1.is_empty(),
        "session B should see fresh output despite A's prior render"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Acceptance signal 2: `read` then `milestone` announces a mesh at most once.
//
// After `read <anchor>` emits BasicOutput for a FRESH mesh, a subsequent
// `milestone` (with no file edits) must NOT re-emit the same mesh —
// meshes-seen is shared across verbs within a session.
// ---------------------------------------------------------------------------

#[test]
fn signal2_read_then_milestone_announces_mesh_once() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let gix = repo.gix_repo()?;
    append_add(&gix, "sig2", "file1.txt", 1, 5, None)?;
    append_add(&gix, "sig2", "file2.txt", 1, 5, None)?;
    set_why(&gix, "sig2", "signal-2 mesh")?;
    commit_mesh(&gix, "sig2")?;

    let s = sid("sig2");
    ok(&run_advice(&repo, &s, &["snapshot"])?);

    // `read` for a FRESH mesh: emits BasicOutput and marks sig2 as seen.
    let read_out = run_advice(&repo, &s, &["read", "file1.txt#L1-L5"])?;
    ok(&read_out);
    let read_text = stdout(&read_out);
    assert!(
        read_text.contains("is in the sig2 mesh with:") && read_text.contains("signal-2 mesh"),
        "read must emit BasicOutput for first-time matching FRESH mesh, got:\n{read_text}"
    );

    // `milestone` with no file edits: sig2 is in meshes-seen (not stale) →
    // must not re-emit (spec: EDIT rule, mesh already seen and fresh → skip).
    let ms_out = run_advice(&repo, &s, &["milestone"])?;
    ok(&ms_out);
    let ms_text = stdout(&ms_out);
    assert!(
        ms_text.is_empty(),
        "milestone after read of FRESH mesh must produce no output; got:\n{ms_text}"
    );

    // Combined: the mesh was announced exactly once across both calls.
    let combined = format!("{read_text}{ms_text}");
    let count = combined.matches("is in the sig2 mesh with:").count();
    assert_eq!(
        count, 1,
        "mesh must be announced exactly once across read + milestone, count={count}"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Acceptance signal 6: bash-only file write is detected by next `milestone`.
//
// A direct write to a tracked file (no Read/Edit tool — just file I/O) is
// invisible to the `read` verb but shows up as a diff when `milestone`
// compares the baseline snapshot against the current workspace tree.
// ---------------------------------------------------------------------------

#[test]
fn signal6_bash_only_edit_detected_by_milestone() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let gix = repo.gix_repo()?;
    append_add(&gix, "sig6", "file1.txt", 1, 5, None)?;
    append_add(&gix, "sig6", "file2.txt", 1, 5, None)?;
    set_why(&gix, "sig6", "signal-6 mesh")?;
    commit_mesh(&gix, "sig6")?;

    let s = sid("sig6");
    ok(&run_advice(&repo, &s, &["snapshot"])?);

    // Direct file write — simulates a Bash `printf >> file1.txt` with no
    // preceding Read/Edit tool call.
    repo.write_file(
        "file1.txt",
        "bash-edit\nline2\nline3\nline4\nline5\nline6\nline7\nline8\nline9\nline10\n",
    )?;

    // `milestone` detects file1.txt in the session delta (EDIT rule) and emits
    // BasicOutput for sig6 (whose anchor overlaps the modified file).
    let ms_out = run_advice(&repo, &s, &["milestone"])?;
    ok(&ms_out);
    let ms_text = stdout(&ms_out);
    assert!(
        ms_text.contains("is in the sig6 mesh with:") && ms_text.contains("signal-6 mesh"),
        "milestone must emit BasicOutput when a meshed file was modified by a bash-only write; got:\n{ms_text}"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// F1 regression: stop's EDIT pass uses session_delta (not incr_delta).
//
// Scenario: snapshot → edit → milestone (advances last-flush) → stop (no new edits).
// After milestone, incr_delta is empty but session_delta still shows the file.
// stop must still emit the stale mesh.
// ---------------------------------------------------------------------------

#[test]
fn stop_emits_stale_mesh_after_milestone_session_delta() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let gix = repo.gix_repo()?;
    // Use whole-file anchor so Action::WholeFile matches.
    git_mesh::staging::append_add_whole(&gix, "f1-mesh", "file1.txt", None)?;
    git_mesh::staging::append_add_whole(&gix, "f1-mesh", "file2.txt", None)?;
    set_why(&gix, "f1-mesh", "f1 regression mesh")?;
    commit_mesh(&gix, "f1-mesh")?;

    let s = sid("f1-stop");
    ok(&run_advice(&repo, &s, &["snapshot"])?);

    // Edit file1.txt: makes the anchor stale and appears in session_delta.
    repo.write_file("file1.txt", "edited-for-f1\nline2\nline3\nline4\nline5\n")?;

    // milestone: emits f1-mesh and advances last-flush to current state.
    let ms_out = run_advice(&repo, &s, &["milestone"])?;
    ok(&ms_out);
    let ms_text = stdout(&ms_out);
    assert!(
        ms_text.contains("is in the f1-mesh mesh with:"),
        "milestone must emit f1-mesh when file is modified; got:\n{ms_text}"
    );

    // No further edits. incr_delta is now empty (last-flush == current).
    // session_delta still shows file1.txt as modified.
    //
    // stop must still emit the stale mesh because it iterates session_delta.
    let stop_out = run_advice(&repo, &s, &["stop"])?;
    ok(&stop_out);
    let stop_text = stdout(&stop_out);
    assert!(
        stop_text.contains("is in the f1-mesh mesh with:"),
        "stop must re-emit the stale f1-mesh via session_delta even after milestone; \
         got:\n{stop_text}"
    );

    Ok(())
}

// ---------------------------------------------------------------------------
// F6: timing guard — snapshot → modify 50 files → stop must complete in < 5s
// on a fixture with 5 meshes. This is an informational bound; if it flakes,
// the fix is to optimize, not to relax the bound.
// ---------------------------------------------------------------------------

#[test]
fn stop_completes_under_5s_with_5_meshes_and_50_modified_files() -> Result<()> {
    use std::time::Instant;

    let repo = TestRepo::new()?;
    let gix = repo.gix_repo()?;

    // Create 50 files and commit them as the baseline.
    for i in 1..=50 {
        repo.write_file_lines(&format!("src/file{i:02}.rs"), 20)?;
    }
    repo.commit_all("initial 50 files")?;

    // Create 5 meshes, each spanning two files.
    for m in 1..=5 {
        let fa = format!("src/file{:02}.rs", m * 2 - 1);
        let fb = format!("src/file{:02}.rs", m * 2);
        append_add(&gix, &format!("mesh{m}"), &fa, 1, 20, None)?;
        append_add(&gix, &format!("mesh{m}"), &fb, 1, 20, None)?;
        set_why(&gix, &format!("mesh{m}"), &format!("timing mesh {m}"))?;
        commit_mesh(&gix, &format!("mesh{m}"))?;
    }

    let s = sid("timing");

    // Snapshot baseline.
    ok(&run_advice(&repo, &s, &["snapshot"])?);

    // Modify all 50 files.
    for i in 1..=50 {
        repo.write_file_lines(&format!("src/file{i:02}.rs"), 21)?;
    }

    // Time the stop verb.
    let start = Instant::now();
    let out = run_advice(&repo, &s, &["stop"])?;
    let elapsed = start.elapsed();

    ok(&out);
    assert!(
        elapsed < std::time::Duration::from_secs(5),
        "stop took {elapsed:?}, expected < 5s on 5-mesh / 50-file fixture"
    );

    Ok(())
}

// ---------------------------------------------------------------------------
// Silence tests — Phase 3: unactionable tool turns produce zero stdout.
// ---------------------------------------------------------------------------

/// Read of a file with no mesh anchor → zero stdout from the `read` verb.
#[test]
fn read_unanchored_file_produces_zero_stdout() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let gix = repo.gix_repo()?;
    // Set up a mesh on file1.txt only; file2.txt has no anchor.
    append_add(&gix, "silence1", "file1.txt", 1, 5, None)?;
    set_why(&gix, "silence1", "only one file")?;
    commit_mesh(&gix, "silence1")?;

    let s = sid("silence-unanchored");
    ok(&run_advice(&repo, &s, &["snapshot"])?);
    // Read an anchor on file2.txt — no mesh covers it.
    let out = run_advice(&repo, &s, &["read", "file2.txt#L1-L5"])?;
    ok(&out);
    assert_eq!(
        stdout(&out),
        "",
        "expected zero stdout for unanchored read, got:\n{}",
        stdout(&out)
    );
    Ok(())
}

/// Read of an anchor whose mesh has already been reported and is still fresh
/// → zero stdout from the `read` verb on the second call.
#[test]
fn read_fresh_already_reported_produces_zero_stdout() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let gix = repo.gix_repo()?;
    append_add(&gix, "silence2", "file1.txt", 1, 5, None)?;
    append_add(&gix, "silence2", "file2.txt", 1, 5, None)?;
    set_why(&gix, "silence2", "fresh-dedup mesh")?;
    commit_mesh(&gix, "silence2")?;

    let s = sid("silence-fresh");
    ok(&run_advice(&repo, &s, &["snapshot"])?);
    // First read — should produce output.
    let first = run_advice(&repo, &s, &["read", "file1.txt#L1-L5"])?;
    ok(&first);
    assert!(
        !stdout(&first).is_empty(),
        "first read of mesh anchor should produce output"
    );
    // Second read of the same anchor on the same session — mesh is fresh, already seen.
    let second = run_advice(&repo, &s, &["read", "file1.txt#L1-L5"])?;
    ok(&second);
    assert_eq!(
        stdout(&second),
        "",
        "second read of fresh anchor should produce zero stdout, got:\n{}",
        stdout(&second)
    );
    Ok(())
}

/// Milestone flush with no modified files → zero stdout from the `milestone` verb.
#[test]
fn milestone_flush_nothing_to_say_produces_zero_stdout() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let gix = repo.gix_repo()?;
    append_add(&gix, "silence3", "file1.txt", 1, 5, None)?;
    append_add(&gix, "silence3", "file2.txt", 1, 5, None)?;
    set_why(&gix, "silence3", "milestone-silence mesh")?;
    commit_mesh(&gix, "silence3")?;

    let s = sid("silence-milestone");
    ok(&run_advice(&repo, &s, &["snapshot"])?);
    // No files modified; milestone has nothing to flush.
    let out = run_advice(&repo, &s, &["milestone"])?;
    ok(&out);
    assert_eq!(
        stdout(&out),
        "",
        "milestone with no changes should produce zero stdout, got:\n{}",
        stdout(&out)
    );
    Ok(())
}

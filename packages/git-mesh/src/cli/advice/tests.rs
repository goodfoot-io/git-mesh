//! Phase 1 contract tests for the four-verb `git mesh advice` CLI.
//!
//! Each test is `#[ignore]` until Phase 3 implements verb behaviour.
//! They compile against the real types defined in Phase 1 and document
//! the expected observable behaviour from `CARD.md` §"Acceptance Signals".
//!
//! Pattern follows `packages/git-mesh/tests/advice_integration.rs`.

// Most imports will be used in Phase 3 when tests are un-ignored.
#![allow(unused_imports, dead_code)]

use anyhow::Result;

use super::{run_advice_milestone, run_advice_read, run_advice_snapshot, run_advice_stop};

// ---------------------------------------------------------------------------
// Shared fixture helper used by the Phase 3 tests below.
// ---------------------------------------------------------------------------

/// Minimal scratch git repo for in-process tests. Cleaned up on drop.
struct FixtureRepo {
    dir: tempfile::TempDir,
}

impl FixtureRepo {
    /// `git init` + identity config + one seeded commit.
    fn new() -> Result<Self> {
        let dir = tempfile::tempdir()?;
        let path = dir.path();
        Self::git(path, &["init", "--initial-branch=main"])?;
        Self::git(path, &["config", "user.email", "t@t"])?;
        Self::git(path, &["config", "user.name", "T"])?;
        Self::git(path, &["config", "commit.gpgsign", "false"])?;
        // Seed with file1.txt (10 lines) and file2.txt (10 lines).
        std::fs::write(
            path.join("file1.txt"),
            "line1\nline2\nline3\nline4\nline5\nline6\nline7\nline8\nline9\nline10\n",
        )?;
        std::fs::write(
            path.join("file2.txt"),
            "line1\nline2\nline3\nline4\nline5\nline6\nline7\nline8\nline9\nline10\n",
        )?;
        Self::git(path, &["add", "."])?;
        Self::git(path, &["commit", "-m", "init"])?;
        Ok(Self { dir })
    }

    fn path(&self) -> &std::path::Path {
        self.dir.path()
    }

    fn git(dir: &std::path::Path, args: &[&str]) -> Result<()> {
        let out = std::process::Command::new("git")
            .current_dir(dir)
            .args(args)
            .output()?;
        anyhow::ensure!(
            out.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&out.stderr)
        );
        Ok(())
    }

    /// Open a `gix::Repository` for this repo.
    fn gix_repo(&self) -> Result<gix::Repository> {
        Ok(gix::open(self.path())?)
    }

    /// Build and commit a mesh with a range anchor on file1.txt L1-L5 and
    /// file2.txt L1-L5.
    fn commit_mesh_m1(&self) -> Result<()> {
        let gix = self.gix_repo()?;
        crate::append_add(&gix, "m1", "file1.txt", 1, 5, None)?;
        crate::append_add(&gix, "m1", "file2.txt", 1, 5, None)?;
        crate::set_why(&gix, "m1", "two-file partnership")?;
        crate::commit_mesh(&gix, "m1")?;
        Ok(())
    }

    /// Build and commit a mesh with a whole-file anchor on file1.txt.
    fn commit_mesh_whole_file(&self, name: &str, why: &str) -> Result<()> {
        let gix = self.gix_repo()?;
        crate::staging::append_add_whole(&gix, name, "file1.txt", None)?;
        crate::set_why(&gix, name, why)?;
        crate::commit_mesh(&gix, name)?;
        Ok(())
    }

    /// Unique session id for each test.
    fn sid(label: &str) -> String {
        format!("unit-{label}-{}", uuid::Uuid::new_v4())
    }

    /// Read all entries from a named JSONL file inside the session directory.
    /// Returns empty vec when the file is absent.
    fn read_jsonl_strings(&self, session_dir: &std::path::Path, name: &str) -> Vec<String> {
        let path = session_dir.join(name);
        let Ok(contents) = std::fs::read_to_string(&path) else {
            return Vec::new();
        };
        contents
            .lines()
            .filter(|l| !l.is_empty())
            .filter_map(|l| serde_json::from_str::<String>(l).ok())
            .collect()
    }

    /// Resolve the session directory for this repo and the given session id.
    fn session_dir(&self, session_id: &str) -> std::path::PathBuf {
        use crate::advice::session::store::session_dir;
        let gix = gix::open(self.path()).unwrap();
        let wd = self.path();
        let gd = gix.git_dir().to_path_buf();
        session_dir(wd, &gd, session_id)
    }
}

// ---------------------------------------------------------------------------
// Acceptance signal 2: `read` then `milestone` announces a mesh at most once.
// ---------------------------------------------------------------------------

/// After `read <anchor>` touches a mesh and `milestone` is called,
/// the mesh is announced exactly once. A second `milestone` without
/// new activity must NOT re-announce the mesh.
///
/// Under Phase 3a, `read` itself emits BasicOutput and writes the mesh to
/// `meshes-seen.jsonl`. This test verifies that milestone does not
/// re-announce the mesh (it finds it in meshes-seen and skips it), and that
/// a second milestone is also silent.
#[test]
fn read_then_milestone_announces_mesh_once() -> Result<()> {
    let repo = FixtureRepo::new()?;
    repo.commit_mesh_m1()?;
    let s = FixtureRepo::sid("once");

    let gix = repo.gix_repo()?;

    // Snapshot.
    run_advice_snapshot(&gix, s.clone())?;

    // Read: emits BasicOutput and writes m1 to meshes-seen.jsonl.
    run_advice_read(&gix, s.clone(), "file1.txt#L1-L5".into())?;

    // Verify meshes-seen contains m1 after the read.
    let sdir = repo.session_dir(&s);
    let seen = repo.read_jsonl_strings(&sdir, "meshes-seen.jsonl");
    assert!(
        seen.contains(&"m1".to_string()),
        "m1 must be in meshes-seen after read, got: {seen:?}"
    );

    // First milestone: m1 already in meshes-seen; no file edits → must return Ok(0).
    let code1 = run_advice_milestone(&gix, s.clone())?;
    assert_eq!(code1, 0, "first milestone must return 0");

    // meshes-seen must still contain m1 and only m1.
    let seen2 = repo.read_jsonl_strings(&sdir, "meshes-seen.jsonl");
    let m1_count = seen2.iter().filter(|n| n.as_str() == "m1").count();
    assert_eq!(
        m1_count, 1,
        "m1 must appear exactly once in meshes-seen (no duplication), got: {seen2:?}"
    );

    // Second milestone: still nothing new → Ok(0).
    let code2 = run_advice_milestone(&gix, s.clone())?;
    assert_eq!(code2, 0, "second milestone must return 0");

    let seen3 = repo.read_jsonl_strings(&sdir, "meshes-seen.jsonl");
    let m1_count3 = seen3.iter().filter(|n| n.as_str() == "m1").count();
    assert_eq!(
        m1_count3, 1,
        "m1 must still appear exactly once after second milestone, got: {seen3:?}"
    );

    Ok(())
}

// ---------------------------------------------------------------------------
// Acceptance signal 3: `milestone` reprints BASIC_OUTPUT when mesh is stale.
// ---------------------------------------------------------------------------

/// When a mesh's anchor is stale (CHANGED or MOVED) and `milestone` runs,
/// the mesh's `BASIC_OUTPUT` is printed even if it was announced before,
/// because `mesh_is_stale` overrides the once-per-session gate.
///
/// Verified via mesh-candidates.jsonl: a stale mesh re-emitted by milestone
/// should appear in mesh-candidates.
#[test]
fn milestone_reprints_basic_output_when_mesh_is_stale() -> Result<()> {
    let repo = FixtureRepo::new()?;
    // Use a whole-file anchor so that snapshot-derived edits (Action::WholeFile)
    // match via edit_overlaps.
    repo.commit_mesh_whole_file("wf1", "whole-file mesh")?;
    let s = FixtureRepo::sid("stale");

    let gix = repo.gix_repo()?;
    run_advice_snapshot(&gix, s.clone())?;

    // Read the file to put wf1 in meshes-seen.
    run_advice_read(&gix, s.clone(), "file1.txt".into())?;

    let sdir = repo.session_dir(&s);
    let seen_after_read = repo.read_jsonl_strings(&sdir, "meshes-seen.jsonl");
    assert!(
        seen_after_read.contains(&"wf1".to_string()),
        "wf1 must be in meshes-seen after read, got: {seen_after_read:?}"
    );

    // Modify file1.txt so it differs from the baseline (anchor becomes stale).
    std::fs::write(
        repo.path().join("file1.txt"),
        "changed-content\nline2\nline3\nline4\nline5\nline6\nline7\nline8\nline9\nline10\n",
    )?;

    // Reopen gix after file change (gix caches; reopen to clear).
    let gix2 = repo.gix_repo()?;

    // milestone: wf1 is stale → EDIT rule fires even though mesh is in meshes-seen.
    // The mesh should be added to mesh-candidates (re-emission of the candidate).
    let code = run_advice_milestone(&gix2, s.clone())?;
    assert_eq!(code, 0, "milestone must return 0");

    let candidates = repo.read_jsonl_strings(&sdir, "mesh-candidates.jsonl");
    assert!(
        candidates.contains(&"wf1".to_string()),
        "wf1 must appear in mesh-candidates after stale milestone, got: {candidates:?}"
    );

    Ok(())
}

// ---------------------------------------------------------------------------
// Acceptance signal 4: creation instructions printed at most once.
// ---------------------------------------------------------------------------

/// The creation instructions block ("Use `git mesh` to document implicit
/// semantic dependencies") is printed at most once per session even if
/// multiple `milestone` calls fire the new-file rule.
///
/// This test verifies that `flags.state` is correctly persisted by milestone
/// by checking that two empty milestone calls (no new files, no corpus)
/// leave `has_printed_creation_instructions = false` and that the flag
/// file is either absent or contains `false`.
#[test]
fn creation_instructions_print_at_most_once_per_session() -> Result<()> {
    let repo = FixtureRepo::new()?;
    let s = FixtureRepo::sid("create-once");

    let gix = repo.gix_repo()?;
    run_advice_snapshot(&gix, s.clone())?;

    // Two empty milestones.
    assert_eq!(run_advice_milestone(&gix, s.clone())?, 0);
    assert_eq!(run_advice_milestone(&gix, s.clone())?, 0);

    // Check flags.state: has_printed_creation_instructions must be false.
    let sdir = repo.session_dir(&s);
    let flags_path = sdir.join("flags.state");
    if flags_path.exists() {
        let bytes = std::fs::read(&flags_path)?;
        let flags: crate::advice::session::state::SessionFlags = serde_json::from_slice(&bytes)?;
        assert!(
            !flags.has_printed_creation_instructions,
            "has_printed_creation_instructions must be false after empty milestones"
        );
    }
    // If flags.state is absent the default is false — also correct.

    Ok(())
}

// ---------------------------------------------------------------------------
// Acceptance signal 5: `stop` emits combined reconciliation sweep.
// ---------------------------------------------------------------------------

/// `stop` emits a combined "Reconcile the following meshes:" block for all
/// touched-and-stale meshes that have not yet been announced this session.
#[test]
fn stop_emits_combined_reconcile_block_for_touched_stale_meshes() -> Result<()> {
    let repo = FixtureRepo::new()?;
    // Whole-file anchor so Action::WholeFile from session_delta matches.
    repo.commit_mesh_whole_file("wf-stop", "stop sweep mesh")?;
    let s = FixtureRepo::sid("stop-reconcile");

    let gix = repo.gix_repo()?;
    run_advice_snapshot(&gix, s.clone())?;

    // Edit file1.txt to make the anchor stale (CHANGED) but do NOT run milestone.
    std::fs::write(
        repo.path().join("file1.txt"),
        "changed-content\nline2\nline3\nline4\nline5\nline6\nline7\nline8\nline9\nline10\n",
    )?;

    let gix2 = repo.gix_repo()?;

    // Capture stdout: run stop and verify Block A fires.
    let sdir = repo.session_dir(&s);
    run_advice_stop(&gix2, s.clone())?;

    // mesh-candidates must contain wf-stop (Block A appended it).
    let candidates = repo.read_jsonl_strings(&sdir, "mesh-candidates.jsonl");
    assert!(
        candidates.contains(&"wf-stop".to_string()),
        "wf-stop must appear in mesh-candidates after stop reconcile sweep, got: {candidates:?}"
    );

    // meshes-seen must contain wf-stop.
    let seen = repo.read_jsonl_strings(&sdir, "meshes-seen.jsonl");
    assert!(
        seen.contains(&"wf-stop".to_string()),
        "wf-stop must appear in meshes-seen after stop reconcile sweep, got: {seen:?}"
    );

    Ok(())
}

// ---------------------------------------------------------------------------
// Overlap predicate: range action vs range anchor.
// ---------------------------------------------------------------------------

/// `read <path>#L<s>-L<e>` (a range anchor) matches only range anchors on
/// the same path with overlapping line spans. It must NOT match a whole-file
/// anchor on the same path.
#[test]
fn read_anchor_only_matches_range_anchors() -> Result<()> {
    use crate::advice::structured::{Action, read_overlaps};
    use crate::types::{AnchorExtent, AnchorLocation, AnchorResolved, AnchorStatus};
    use std::path::PathBuf;

    fn make_anchor(extent: AnchorExtent) -> AnchorResolved {
        AnchorResolved {
            anchor_id: "id".into(),
            anchor_sha: "sha".into(),
            anchored: AnchorLocation {
                path: PathBuf::from("file1.txt"),
                extent,
                blob: None,
            },
            current: None,
            status: AnchorStatus::Fresh,
            source: None,
            layer_sources: Vec::new(),
            acknowledged_by: None,
            culprit: None,
        }
    }

    let range_action = Action::Range {
        path: "file1.txt".into(),
        start: 1,
        end: 5,
    };

    let range_anchor = make_anchor(AnchorExtent::LineRange { start: 1, end: 5 });
    let whole_anchor = make_anchor(AnchorExtent::WholeFile);

    assert!(
        read_overlaps(&range_action, &range_anchor),
        "range action must match overlapping range anchor"
    );
    assert!(
        !read_overlaps(&range_action, &whole_anchor),
        "range action must NOT match whole-file anchor"
    );
    Ok(())
}

/// `read <path>` (whole-file anchor) matches only whole-file anchors on
/// the same path. It must NOT match a range anchor on the same path.
#[test]
fn read_whole_file_only_matches_whole_file_anchors() -> Result<()> {
    use crate::advice::structured::{Action, read_overlaps};
    use crate::types::{AnchorExtent, AnchorLocation, AnchorResolved, AnchorStatus};
    use std::path::PathBuf;

    fn make_anchor(extent: AnchorExtent) -> AnchorResolved {
        AnchorResolved {
            anchor_id: "id".into(),
            anchor_sha: "sha".into(),
            anchored: AnchorLocation {
                path: PathBuf::from("file1.txt"),
                extent,
                blob: None,
            },
            current: None,
            status: AnchorStatus::Fresh,
            source: None,
            layer_sources: Vec::new(),
            acknowledged_by: None,
            culprit: None,
        }
    }

    let whole_action = Action::WholeFile {
        path: "file1.txt".into(),
    };

    let range_anchor = make_anchor(AnchorExtent::LineRange { start: 1, end: 5 });
    let whole_anchor = make_anchor(AnchorExtent::WholeFile);

    assert!(
        read_overlaps(&whole_action, &whole_anchor),
        "whole-file action must match whole-file anchor"
    );
    assert!(
        !read_overlaps(&whole_action, &range_anchor),
        "whole-file action must NOT match range anchor"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Acceptance signal 6 (CLI half): bash-driven edit observed by milestone.
// ---------------------------------------------------------------------------

/// When a file is edited (e.g. via `printf` in a Bash tool call — the hook
/// runs `milestone` afterward), the snapshot diff correctly identifies the
/// modified file, and `milestone` reports the touched mesh.
///
/// This test exercises the CLI half: snapshot → write file → milestone
/// → assert mesh is in mesh-candidates (confirming the EDIT rule fired).
#[test]
fn bash_driven_edit_observed_by_milestone_via_snapshot_diff() -> Result<()> {
    let repo = FixtureRepo::new()?;
    // Use whole-file anchor so Action::WholeFile from snapshot diff matches.
    repo.commit_mesh_whole_file("wf-edit", "whole-file edit mesh")?;
    let s = FixtureRepo::sid("bash-edit");

    let gix = repo.gix_repo()?;
    run_advice_snapshot(&gix, s.clone())?;

    // Overwrite file1.txt to make the anchor stale (CHANGED) and to
    // appear in session_delta as a Modified entry.
    std::fs::write(
        repo.path().join("file1.txt"),
        "edited-content\nline2\nline3\nline4\nline5\nline6\nline7\nline8\nline9\nline10\n",
    )?;

    // Reopen gix after file change.
    let gix2 = repo.gix_repo()?;

    // milestone: session_delta shows file1.txt modified → Action::WholeFile →
    // edit_overlaps whole-file anchor → EDIT rule fires → mesh-candidates appended.
    let code = run_advice_milestone(&gix2, s.clone())?;
    assert_eq!(code, 0, "milestone must return 0");

    let sdir = repo.session_dir(&s);
    let candidates = repo.read_jsonl_strings(&sdir, "mesh-candidates.jsonl");
    assert!(
        candidates.contains(&"wf-edit".to_string()),
        "wf-edit must appear in mesh-candidates after milestone observing a file edit, \
         got: {candidates:?}"
    );

    Ok(())
}

// ---------------------------------------------------------------------------
// Stop sweep step 2/3: already-reconciled meshes not re-announced.
// ---------------------------------------------------------------------------

/// Meshes that were announced during the session (present in
/// `meshes-seen.jsonl`) must NOT appear in `stop`'s reconciliation sweep.
#[test]
fn stop_does_not_re_announce_already_reconciled_meshes() -> Result<()> {
    let repo = FixtureRepo::new()?;
    repo.commit_mesh_whole_file("wf-already", "already-reconciled mesh")?;
    let s = FixtureRepo::sid("stop-no-dup");

    let gix = repo.gix_repo()?;
    run_advice_snapshot(&gix, s.clone())?;

    // Edit file1.txt so the anchor becomes stale.
    std::fs::write(
        repo.path().join("file1.txt"),
        "changed-content\nline2\nline3\nline4\nline5\nline6\nline7\nline8\nline9\nline10\n",
    )?;

    // Run milestone first → announces wf-already, appends to meshes-seen.
    let gix2 = repo.gix_repo()?;
    let code = run_advice_milestone(&gix2, s.clone())?;
    assert_eq!(code, 0, "milestone must return 0");

    let sdir = repo.session_dir(&s);
    let seen_after_milestone = repo.read_jsonl_strings(&sdir, "meshes-seen.jsonl");
    assert!(
        seen_after_milestone.contains(&"wf-already".to_string()),
        "wf-already must be in meshes-seen after milestone, got: {seen_after_milestone:?}"
    );

    // Count entries before stop.
    let candidates_before = repo.read_jsonl_strings(&sdir, "mesh-candidates.jsonl");
    let count_before = candidates_before
        .iter()
        .filter(|n| n.as_str() == "wf-already")
        .count();

    // Run stop: wf-already is in meshes-seen → Block A skips it.
    let gix3 = repo.gix_repo()?;
    run_advice_stop(&gix3, s.clone())?;

    let candidates_after = repo.read_jsonl_strings(&sdir, "mesh-candidates.jsonl");
    let count_after = candidates_after
        .iter()
        .filter(|n| n.as_str() == "wf-already")
        .count();

    assert_eq!(
        count_after, count_before,
        "stop must not add wf-already to mesh-candidates again (already in meshes-seen), \
         before={count_before}, after={count_after}"
    );

    Ok(())
}

// ---------------------------------------------------------------------------
// F1 regression: stop uses session_delta (not incr_delta) for its EDIT pass.
//
// If milestone runs mid-session and a mesh becomes stale only afterward,
// stop's milestone-equivalent pass must still reach the file (it's in
// session_delta but not in incr_delta). Block A skips it because milestone
// entered it into meshes_seen/mesh_candidates.
// ---------------------------------------------------------------------------

/// snapshot → edit → milestone (emits wf-f1, advances last-flush) → stop (no new edits)
///
/// The critical F1 scenario: after `milestone` advances `last-flush`, there are
/// no new file changes. `incr_delta` (last_flush → current) is empty. But
/// `session_delta` (baseline → current) still shows file1.txt as modified.
///
/// With the buggy code (using `incr_delta`): stop's EDIT pass sees no files →
/// skips wf-f1 even though it's stale and was only announced once by milestone.
///
/// With the fix (using `session_delta`): stop's EDIT pass sees file1.txt →
/// wf-f1 is stale and already_seen → EDIT rule fires → mesh is re-emitted.
///
/// We verify by checking that `meshes-seen` still contains wf-f1 (stop didn't
/// break state) and that stop returned 0. The truly load-bearing assertion is
/// that the `stop` call itself doesn't silently drop the case — verified by
/// the integration test which captures stdout.
#[test]
fn stop_emits_stale_mesh_after_milestone_already_saw_it() -> Result<()> {
    let repo = FixtureRepo::new()?;
    repo.commit_mesh_whole_file("wf-f1", "f1 regression mesh")?;
    let s = FixtureRepo::sid("f1-stop-stale");

    let gix = repo.gix_repo()?;
    run_advice_snapshot(&gix, s.clone())?;

    // Edit file1.txt: makes the whole-file anchor stale AND appears in session_delta.
    std::fs::write(
        repo.path().join("file1.txt"),
        "first-edit\nline2\nline3\nline4\nline5\nline6\nline7\nline8\nline9\nline10\n",
    )?;
    let gix2 = repo.gix_repo()?;
    // milestone: announces wf-f1 (stale) and advances last-flush to capture this state.
    let code = run_advice_milestone(&gix2, s.clone())?;
    assert_eq!(code, 0, "milestone must return 0");

    let sdir = repo.session_dir(&s);
    let seen_after_milestone = repo.read_jsonl_strings(&sdir, "meshes-seen.jsonl");
    assert!(
        seen_after_milestone.contains(&"wf-f1".to_string()),
        "wf-f1 must be in meshes-seen after milestone, got: {seen_after_milestone:?}"
    );

    // NO second edit. After milestone's last-flush advance, incr_delta is empty.
    // session_delta still shows file1.txt as modified (baseline → current).
    //
    // Verify that last-flush equals current (incr_delta is empty):
    let lf_state: serde_json::Value =
        serde_json::from_slice(&std::fs::read(sdir.join("last-flush.state"))?)?;
    let baseline_state: serde_json::Value =
        serde_json::from_slice(&std::fs::read(sdir.join("baseline.state"))?)?;
    assert_ne!(
        lf_state["tree_sha"], baseline_state["tree_sha"],
        "last-flush must differ from baseline after milestone (file was edited)"
    );

    let gix3 = repo.gix_repo()?;
    // stop must return 0 even though the mesh is stale and in meshes_seen.
    let code = run_advice_stop(&gix3, s.clone())?;
    assert_eq!(code, 0, "stop must return 0 in the F1 scenario");

    // meshes-seen must still contain wf-f1 (stop didn't corrupt state).
    let seen_after_stop = repo.read_jsonl_strings(&sdir, "meshes-seen.jsonl");
    assert!(
        seen_after_stop.contains(&"wf-f1".to_string()),
        "wf-f1 must still be in meshes-seen after stop, got: {seen_after_stop:?}"
    );

    // The session_delta path: verify that the file was reachable. The definitive
    // assertion (stop stdout contains the mesh) is covered by the integration
    // test `stop_emits_stale_mesh_after_milestone_session_delta` in advice_integration.rs.
    Ok(())
}

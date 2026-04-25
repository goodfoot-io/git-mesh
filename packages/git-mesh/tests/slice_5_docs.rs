//! Slice 5 — doc-topic preambles (§12.12) and `--documentation` hints
//! (§12.11).
//!
//! Two layers of coverage:
//! 1. Renderer-level: feed synthetic candidates through `render::render`
//!    to assert each §12.12 topic block renders verbatim modulo the
//!    `# ` prefix, and that `--documentation` appends per-reason hints.
//! 2. CLI-level: exercise the flush pipeline end-to-end for the once-
//!    per-session dedup of the Baseline + WriteAcross topic, the L0-
//!    only "no preamble" guarantee, and the `--documentation` empty-
//!    flush "no hint" guarantee.

mod support;

use anyhow::Result;
use git_mesh::advice::intersections::{Candidate, Density, ReasonKind};
use git_mesh::advice::render;
use git_mesh::{append_add, commit_mesh, set_why};
use std::path::PathBuf;
use std::process::Output;
use support::TestRepo;
use uuid::Uuid;

const SESSION_DIR: &str = "/tmp/git-mesh-claude-code";

struct Session {
    id: String,
}
impl Session {
    fn new(prefix: &str) -> Self {
        let id = format!("slice5-{prefix}-{}", Uuid::new_v4());
        let s = Self { id };
        s.cleanup();
        s
    }
    fn db_path(&self) -> PathBuf {
        PathBuf::from(SESSION_DIR).join(format!("{}.db", self.id))
    }
    fn jsonl_path(&self) -> PathBuf {
        PathBuf::from(SESSION_DIR).join(format!("{}.jsonl", self.id))
    }
    fn cleanup(&self) {
        let _ = std::fs::remove_file(self.db_path());
        let _ = std::fs::remove_file(self.db_path().with_extension("db-wal"));
        let _ = std::fs::remove_file(self.db_path().with_extension("db-shm"));
        let _ = std::fs::remove_file(self.jsonl_path());
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        self.cleanup();
    }
}

fn run_advice(repo: &TestRepo, session: &Session, extra: &[&str]) -> Result<Output> {
    let mut args: Vec<String> = vec!["advice".into(), session.id.clone()];
    for a in extra {
        args.push((*a).to_string());
    }
    repo.run_mesh(args)
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

fn flush(repo: &TestRepo, s: &Session, extra: &[&str]) -> Result<String> {
    let out = run_advice(repo, s, extra)?;
    ok(&out);
    Ok(String::from_utf8(out.stdout)?)
}

// ---------------------------------------------------------------------------
// Renderer-level: each §12.12 topic block renders verbatim per the spec.
// ---------------------------------------------------------------------------

/// Build a minimal L1 candidate so `render::render` accepts the input —
/// the per-mesh body is irrelevant; we assert against the preamble.
fn l1_candidate() -> Candidate {
    Candidate {
        mesh: "stub".into(),
        mesh_why: "stub why".into(),
        reason_kind: ReasonKind::WriteAcross,
        partner_path: "p.rs".into(),
        partner_start: Some(1),
        partner_end: Some(2),
        trigger_path: "t.rs".into(),
        trigger_start: None,
        trigger_end: None,
        partner_marker: String::new(),
        partner_clause: String::new(),
        density: Density::L1,
        command: String::new(),
        excerpt_of_path: String::new(),
        excerpt_start: None,
        excerpt_end: None,
    }
}

/// Assert that every line in the rendered output begins with `#`
/// (per §12.2 — including the bare `#` lines that stand in for blanks).
fn assert_all_lines_commented(out: &str) {
    for line in out.lines() {
        assert!(
            line.starts_with('#'),
            "line does not start with #: {line:?}\nfull output:\n{out}"
        );
    }
}

#[test]
fn baseline_topic_block_renders_verbatim() {
    let out = render::render(&[l1_candidate()], &["baseline".into()], false);
    assert_all_lines_commented(&out);
    assert!(out.contains("# A mesh names a subsystem, flow, or concern that spans line ranges in"));
    assert!(out.contains("# Inspect a mesh:"));
    assert!(out.contains("#   git mesh show <name>"));
    assert!(out.contains("#   git mesh ls <path>"));
    assert!(out.contains("#   git mesh stale"));
}

#[test]
fn t2_topic_block_renders_verbatim() {
    let out = render::render(&[l1_candidate()], &["editing-across-files".into()], false);
    assert_all_lines_commented(&out);
    assert!(out.contains("# When a range in a mesh changes, the other ranges in the same mesh may"));
    assert!(out.contains("# A second `git mesh add` over the identical (path, extent) is a"));
    assert!(out.contains("#   git mesh add <name> <path>#L<s>-L<e>"));
    assert!(out.contains("#   git mesh commit <name>"));
}

#[test]
fn t3_topic_block_renders_verbatim() {
    let out = render::render(&[l1_candidate()], &["renames".into()], false);
    assert!(out.contains("# A related range contains the old path as a literal string."));
    assert!(out.contains("#   git mesh rm  <name> <old-path>"));
    assert!(out.contains("#   git mesh add <name> <new-path>"));
}

#[test]
fn t4_topic_block_renders_verbatim() {
    let out = render::render(&[l1_candidate()], &["shrinking-ranges".into()], false);
    assert!(out.contains("# The edit reduced a range to far fewer lines than were recorded."));
    assert!(out.contains("#   git mesh rm  <name> <path>#L<old-s>-L<old-e>"));
    assert!(out.contains("#   git mesh add <name> <path>#L<new-s>-L<new-e>"));
}

#[test]
fn t5_topic_block_renders_verbatim() {
    let out = render::render(&[l1_candidate()], &["narrow-or-retire".into()], false);
    assert!(out.contains("# Most ranges in this mesh no longer match what was recorded."));
    assert!(out.contains("#   git mesh rm     <name> <path>"));
    assert!(out.contains("#   git mesh delete <name>"));
    assert!(out.contains("#   git mesh revert <name> <commit-ish>"));
}

#[test]
fn t6_topic_block_renders_verbatim() {
    let out = render::render(&[l1_candidate()], &["exported-symbols".into()], false);
    assert!(out.contains("# An exported name changed inside one range."));
    assert!(out.contains("#   git mesh add <name> <path>#L<s>-L<e>"));
}

#[test]
fn t7_topic_block_renders_verbatim() {
    let out = render::render(&[l1_candidate()], &["recording-a-group".into()], false);
    assert!(out.contains("# These files move together: the session has touched them together and"));
    assert!(out.contains("# Record:"));
    assert!(out.contains("#   git mesh add <group-name> <path-1> <path-2> [...]"));
    assert!(out.contains("# Name with a kebab-case slug that titles the subsystem, optionally"));
}

#[test]
fn t8_topic_block_renders_verbatim() {
    let out = render::render(&[l1_candidate()], &["cross-mesh-overlap".into()], false);
    assert!(out.contains("# A range staged on one mesh overlaps a range already recorded on"));
    assert!(out.contains("#   git mesh restore <name>"));
    assert!(out.contains("#   git mesh delete  <name>"));
}

#[test]
fn t9_topic_block_renders_verbatim() {
    let out = render::render(&[l1_candidate()], &["empty-groups".into()], false);
    assert!(out.contains("# The staged removal would leave this mesh with no ranges."));
    assert!(out.contains("#   git mesh add    <name> <path>[#L<s>-L<e>]"));
    assert!(out.contains("#   git mesh delete <name>"));
}

#[test]
fn t11_topic_block_renders_verbatim() {
    let out = render::render(&[l1_candidate()], &["terminal-states".into()], false);
    assert!(out.contains("# A terminal marker means the resolver cannot evaluate this range at all."));
    assert!(out.contains("# [ORPHANED]  — the recorded commit is unreachable."));
    assert!(out.contains("# [CONFLICT]  — the file is mid-merge. Finish the merge first."));
    assert!(out.contains("# [SUBMODULE] — the range points inside a submodule"));
}

// ---------------------------------------------------------------------------
// Renderer-level: --documentation appends one short hint per reason-kind,
// AFTER the rest of the output, and never duplicates the per-topic block.
// ---------------------------------------------------------------------------

#[test]
fn documentation_empty_flush_prints_nothing() {
    let out = render::render(&[], &[], true);
    assert!(out.is_empty(), "empty flush + --documentation must be empty: {out:?}");
}

#[test]
fn documentation_appends_t1_and_t2_hints_after_output() {
    let mut t1 = l1_candidate();
    t1.reason_kind = ReasonKind::Partner;
    t1.density = Density::L0;
    t1.mesh = "m1".into();
    let mut t2 = l1_candidate();
    t2.mesh = "m1".into();

    let out = render::render(&[t1, t2], &[], true);
    // T1 hint and T2 hint each appear exactly once.
    let t1_hint = "to re-record a range after edits";
    let t2_hint = "to re-record a partner that needed matching edits";
    assert_eq!(out.matches(t1_hint).count(), 1, "t1 hint must appear once: {out}");
    assert_eq!(out.matches(t2_hint).count(), 1, "t2 hint must appear once: {out}");
    // The hints come after the mesh body.
    let body_pos = out.find("# m1 mesh:").expect("mesh body present");
    let t1_pos = out.find(t1_hint).expect("t1 hint present");
    let t2_pos = out.find(t2_hint).expect("t2 hint present");
    assert!(t1_pos > body_pos, "t1 hint must follow body");
    assert!(t2_pos > body_pos, "t2 hint must follow body");
}

#[test]
fn documentation_with_t8_does_not_duplicate_topic_block() {
    let mut t8 = l1_candidate();
    t8.reason_kind = ReasonKind::StagingCrossCut;
    t8.density = Density::L2;
    t8.partner_clause =
        "overlap|staged|other|p.rs|10|20|5|25|".into();
    t8.trigger_start = Some(10);
    t8.trigger_end = Some(20);

    // Pretend the topic block already fired earlier in the session — pass
    // an empty `new_doc_topics` list so the topic does NOT appear as a
    // preamble. `--documentation` must still print the hint, exactly once,
    // and must not re-emit the per-topic block.
    let out = render::render(&[t8], &[], true);
    let hint = "to resolve a cross-mesh overlap";
    assert_eq!(out.matches(hint).count(), 1, "t8 hint must appear once");
    // Per-topic block fragment must NOT be present.
    assert!(
        !out.contains("# A range staged on one mesh overlaps a range already recorded on"),
        "topic block must not be duplicated by --documentation: {out}"
    );
}

// ---------------------------------------------------------------------------
// CLI-level: Baseline fires once per session; L0 flushes never emit topics;
// --documentation on an empty flush prints nothing.
// ---------------------------------------------------------------------------

#[test]
fn baseline_topic_fires_once_then_dedups() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let gix = repo.gix_repo()?;
    append_add(&gix, "pair", "file1.txt", 1, 5, None)?;
    append_add(&gix, "pair", "file2.txt", 1, 5, None)?;
    set_why(&gix, "pair", "Pair of files for testing.")?;
    commit_mesh(&gix, "pair")?;

    let s = Session::new("baseline-once");

    // First L1 flush: T2 (write across) — Baseline + T2 preambles fire.
    ok(&run_advice(&repo, &s, &["add", "--write", "file1.txt#L1-L5"])?);
    let first = flush(&repo, &s, &[])?;
    assert!(
        first.contains("# A mesh names a subsystem, flow, or concern"),
        "first L1 flush must include Baseline preamble:\n{first}"
    );
    assert!(
        first.contains("# When a range in a mesh changes, the other ranges in the same mesh may"),
        "first L1 flush must include T2 preamble:\n{first}"
    );

    // Second L1 flush in the same session — both Baseline and T2 are
    // already recorded; neither preamble re-fires.
    ok(&run_advice(&repo, &s, &["add", "--write", "file2.txt#L1-L5"])?);
    let second = flush(&repo, &s, &[])?;
    assert!(
        !second.contains("# A mesh names a subsystem"),
        "Baseline must NOT re-fire on second L1 flush:\n{second}"
    );
    assert!(
        !second.contains("# When a range in a mesh changes, the other ranges in the same mesh may"),
        "T2 must NOT re-fire on second L1 flush:\n{second}"
    );
    Ok(())
}

#[test]
fn l0_only_flush_emits_no_doc_topic_blocks() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let gix = repo.gix_repo()?;
    append_add(&gix, "pair", "file1.txt", 1, 5, None)?;
    append_add(&gix, "pair", "file2.txt", 1, 5, None)?;
    set_why(&gix, "pair", "Pair of files for testing.")?;
    commit_mesh(&gix, "pair")?;

    let s = Session::new("l0-only");
    // Read events generate T1 (L0) only.
    ok(&run_advice(&repo, &s, &["add", "--read", "file1.txt#L1-L5"])?);
    let out = flush(&repo, &s, &[])?;
    assert!(
        !out.is_empty(),
        "L0 read must still surface the partner-list message"
    );
    assert!(
        !out.contains("# A mesh names a subsystem"),
        "L0-only flush must NOT include Baseline preamble:\n{out}"
    );
    assert!(
        !out.contains("# When a range in a mesh changes"),
        "L0-only flush must NOT include T2 preamble:\n{out}"
    );
    Ok(())
}

#[test]
fn documentation_empty_flush_via_cli_prints_nothing() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let s = Session::new("doc-empty");
    // No mesh, no events → empty flush.
    let out = flush(&repo, &s, &["--documentation"])?;
    assert!(
        out.is_empty(),
        "--documentation on an empty flush must print nothing, got: {out:?}"
    );
    Ok(())
}

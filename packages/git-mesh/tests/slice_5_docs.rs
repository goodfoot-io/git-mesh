//! Slice 5 — doc-topic preambles and `--documentation` hints.
//!
//! Renderer-level tests (each §12.12 topic block, the `--documentation`
//! hint placement, the empty-flush guarantee) are unchanged in intent;
//! only the import path moved from `intersections::` to `candidates::`.
//!
//! CLI-level tests are rewritten against the file-backed pipeline:
//! per-session topic dedup is verified via `docs-seen.jsonl` monotonicity
//! across two consecutive renders.

mod support;

use anyhow::Result;
use git_mesh::advice::candidates::{Candidate, Density, ReasonKind, candidate_to_suggestion};
use git_mesh::advice::render;
use git_mesh::advice::suggestion::Suggestion;
use git_mesh::{append_add, commit_mesh, set_why};
use std::process::Output;
use support::TestRepo;
use uuid::Uuid;

fn sid(prefix: &str) -> String {
    format!("slice5-{prefix}-{}", Uuid::new_v4())
}

fn run_advice(repo: &TestRepo, s: &str, extra: &[&str]) -> Result<Output> {
    let mut args: Vec<String> = vec!["advice".into(), s.into()];
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

fn session_dir(repo: &TestRepo, sid: &str) -> std::path::PathBuf {
    let store = git_mesh::advice::SessionStore::open(
        repo.path(),
        &repo.path().join(".git"),
        sid,
    )
    .expect("open store");
    store.baseline_objects_dir().parent().expect("parent").to_path_buf()
}

fn render_via_cli(repo: &TestRepo, s: &str, extra: &[&str]) -> Result<String> {
    let out = run_advice(repo, s, extra)?;
    ok(&out);
    Ok(String::from_utf8(out.stdout)?)
}

// ---------------------------------------------------------------------------
// Renderer-level: each §12.12 topic block renders verbatim per the spec.
// ---------------------------------------------------------------------------

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
        touched_path: String::new(),
        touched_start: None,
        touched_end: None,
        partner_marker: String::new(),
        partner_clause: String::new(),
        density: Density::L1,
        command: String::new(),
        excerpt_of_path: String::new(),
        excerpt_start: None,
        excerpt_end: None,
        old_blob: None,
        new_blob: None,
        old_path: None,
        new_path: None,
    }
}

fn l1_suggestion() -> Suggestion {
    candidate_to_suggestion(&l1_candidate())
}

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
    let out = render::render(&[l1_suggestion()], &["baseline".into()], true);
    assert_all_lines_commented(&out);
    assert!(out.contains("# A mesh is a lightweight contract for an agreement that no schema, type,"));
    assert!(out.contains("# The `why` is load-bearing identity, not commentary."));
    assert!(out.contains("# Inspect a mesh:"));
    assert!(out.contains("#   git mesh show <name>"));
    assert!(out.contains("#   git mesh ls <path>"));
    assert!(out.contains("#   git mesh stale"));
    assert!(out.contains("#   git mesh why <name>"));
}

#[test]
fn t2_topic_block_renders_verbatim() {
    let out = render::render(&[l1_suggestion()], &["editing-across-files".into()], true);
    assert_all_lines_commented(&out);
    assert!(out.contains("# When an anchor in a mesh changes, the other anchors in the same mesh"));
    assert!(out.contains("# A second `git mesh add` over the identical (path, extent) is a"));
    assert!(out.contains("#   git mesh add <name> <path>#L<s>-L<e>"));
    assert!(out.contains("#   git mesh commit <name>"));
}

#[test]
fn t3_topic_block_renders_verbatim() {
    let out = render::render(&[l1_suggestion()], &["renames".into()], true);
    assert!(out.contains("# A related anchor contains the old path as a literal string."));
    assert!(out.contains("#   git mesh rm  <name> <old-path>"));
    assert!(out.contains("#   git mesh add <name> <new-path>"));
}

#[test]
fn t4_topic_block_renders_verbatim() {
    let out = render::render(&[l1_suggestion()], &["shrinking-ranges".into()], true);
    assert!(out.contains("# The edit reduced a line-range anchor to far fewer lines than were"));
    assert!(out.contains("#   git mesh rm  <name> <path>#L<old-s>-L<old-e>"));
    assert!(out.contains("#   git mesh add <name> <path>#L<new-s>-L<new-e>"));
}

#[test]
fn t5_topic_block_renders_verbatim() {
    let out = render::render(&[l1_suggestion()], &["narrow-or-retire".into()], true);
    assert!(out.contains("# Most anchors in this mesh no longer match what was recorded."));
    assert!(out.contains("#   git mesh rm     <name> <path>"));
    assert!(out.contains("#   git mesh delete <name>"));
    assert!(out.contains("#   git mesh revert <name> <commit-ish>"));
}

#[test]
fn t6_topic_block_renders_verbatim() {
    let out = render::render(&[l1_suggestion()], &["exported-symbols".into()], true);
    assert!(out.contains("# An exported name changed inside one anchor."));
    assert!(out.contains("#   git mesh add <name> <path>#L<s>-L<e>"));
}

#[test]
fn t7_topic_block_renders_verbatim() {
    let out = render::render(&[l1_suggestion()], &["recording-a-mesh".into()], true);
    assert!(out.contains("# These files move together: the session has touched them together and"));
    assert!(out.contains("# Record:"));
    assert!(out.contains("#   git mesh add <mesh-name> <path-1> <path-2> [...]"));
    assert!(out.contains("# Name with a kebab-case slug that titles the subsystem, optionally"));
}

#[test]
fn t8_topic_block_renders_verbatim() {
    let out = render::render(&[l1_suggestion()], &["cross-mesh-overlap".into()], true);
    assert!(out.contains("# An anchor staged on one mesh overlaps an anchor already recorded on"));
    assert!(out.contains("#   git mesh restore <name>"));
    assert!(out.contains("#   git mesh delete  <name>"));
}

#[test]
fn t9_topic_block_renders_verbatim() {
    let out = render::render(&[l1_suggestion()], &["empty-meshes".into()], true);
    assert!(out.contains("# The staged removal would leave this mesh with no anchors."));
    assert!(out.contains("#   git mesh add    <name> <path>[#L<s>-L<e>]"));
    assert!(out.contains("#   git mesh delete <name>"));
}

#[test]
fn t11_topic_block_renders_verbatim() {
    let out = render::render(&[l1_suggestion()], &["terminal-states".into()], true);
    assert!(out.contains("# A terminal marker means the resolver cannot evaluate this anchor at all."));
    assert!(out.contains("# [ORPHANED]  — the recorded commit is unreachable."));
    assert!(out.contains("# [CONFLICT]  — the file is mid-merge. Finish the merge first."));
    assert!(out.contains("# [SUBMODULE] — the anchor points inside a submodule"));
}

// ---------------------------------------------------------------------------
// Renderer-level: --documentation hint placement.
// ---------------------------------------------------------------------------

#[test]
fn documentation_empty_flush_prints_nothing() {
    let out = render::render(&[], &[], true);
    assert!(out.is_empty(), "empty flush + --documentation must be empty: {out:?}");
}

#[test]
fn documentation_appends_t1_and_t2_hints_after_output() {
    let mut t1_c = l1_candidate();
    t1_c.reason_kind = ReasonKind::Partner;
    t1_c.density = Density::L0;
    t1_c.mesh = "m1".into();
    let mut t2_c = l1_candidate();
    t2_c.mesh = "m1".into();
    let t1 = candidate_to_suggestion(&t1_c);
    let t2 = candidate_to_suggestion(&t2_c);

    let out = render::render(&[t1, t2], &[], true);
    let t1_hint = "to re-record an anchor after edits";
    let t2_hint = "to re-record a partner that needed matching edits";
    assert_eq!(out.matches(t1_hint).count(), 1, "t1 hint must appear once: {out}");
    assert_eq!(out.matches(t2_hint).count(), 1, "t2 hint must appear once: {out}");
    let body_pos = out.find("# m1 mesh:").expect("mesh body present");
    let t1_pos = out.find(t1_hint).expect("t1 hint present");
    let t2_pos = out.find(t2_hint).expect("t2 hint present");
    assert!(t1_pos > body_pos, "t1 hint must follow body");
    assert!(t2_pos > body_pos, "t2 hint must follow body");
}

#[test]
fn documentation_with_t8_does_not_duplicate_topic_block() {
    let mut t8_c = l1_candidate();
    t8_c.reason_kind = ReasonKind::StagingCrossCut;
    t8_c.density = Density::L2;
    t8_c.partner_clause =
        "overlap|staged|other|p.rs|10|20|5|25|".into();
    t8_c.trigger_start = Some(10);
    t8_c.trigger_end = Some(20);
    let t8 = candidate_to_suggestion(&t8_c);

    let out = render::render(&[t8], &[], true);
    let hint = "to resolve a cross-mesh overlap";
    assert_eq!(out.matches(hint).count(), 1, "t8 hint must appear once");
    assert!(
        !out.contains("# An anchor staged on one mesh overlaps an anchor already recorded on"),
        "topic block must not be duplicated by --documentation: {out}"
    );
}

// ---------------------------------------------------------------------------
// CLI-level: docs-seen.jsonl is monotonic across two consecutive renders.
// ---------------------------------------------------------------------------

#[test]
fn docs_seen_does_not_shrink_across_renders() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let gix = repo.gix_repo()?;
    append_add(&gix, "pair", "file1.txt", 1, 5, None)?;
    append_add(&gix, "pair", "file2.txt", 1, 5, None)?;
    set_why(&gix, "pair", "Pair of files for testing.")?;
    commit_mesh(&gix, "pair")?;

    let s = sid("docs-mono");
    ok(&run_advice(&repo, &s, &["snapshot"])?);
    ok(&run_advice(&repo, &s, &["read", "file1.txt"])?);
    let _ = render_via_cli(&repo, &s, &["--documentation"])?;
    let dir = session_dir(&repo, &s);
    let first = std::fs::read(dir.join("docs-seen.jsonl"))?;

    ok(&run_advice(&repo, &s, &["read", "file2.txt"])?);
    let _ = render_via_cli(&repo, &s, &["--documentation"])?;
    let second = std::fs::read(dir.join("docs-seen.jsonl"))?;

    assert!(
        second.len() >= first.len(),
        "docs-seen.jsonl must be monotonic"
    );
    Ok(())
}

#[test]
fn documentation_empty_via_cli_prints_nothing() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let s = sid("doc-empty");
    ok(&run_advice(&repo, &s, &["snapshot"])?);
    let out = render_via_cli(&repo, &s, &["--documentation"])?;
    assert!(
        out.is_empty(),
        "--documentation with no triggers must print nothing, got: {out:?}"
    );
    Ok(())
}

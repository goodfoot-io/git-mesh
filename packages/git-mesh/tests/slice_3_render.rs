//! Slice 3 — render-output bug fixes.
//!
//! Covers: T4 false positives (4.1), T4 re-record command (4.2), per-flush
//! excerpt dedup across meshes (4.3), whole-file partner address-only
//! (4.4), `[DELETED]` marker for vanished partner (4.5), T7 phrasing with
//! per-session touch count (4.6).

mod support;

use anyhow::Result;
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
        let id = format!("slice3-{prefix}-{}", Uuid::new_v4());
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

fn flush(repo: &TestRepo, s: &Session) -> Result<String> {
    let out = run_advice(repo, s, &[])?;
    ok(&out);
    Ok(String::from_utf8(out.stdout)?)
}

// ---------------------------------------------------------------------------
// (1) T4 fail-closed without --post; (2) T4 command shape and new extent.
// ---------------------------------------------------------------------------

#[test]
fn t4_does_not_fire_without_post_content() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let gix = repo.gix_repo()?;
    append_add(&gix, "shrink", "file2.txt", 1, 16, None)?;
    append_add(&gix, "shrink", "file1.txt", 1, 5, None)?;
    set_why(&gix, "shrink", "wide range")?;
    commit_mesh(&gix, "shrink")?;

    let s = Session::new("t4-no-post");
    // Write that *would* trigger T4 if we only looked at the recorded
    // extent, but with no --post content T4 must fail-closed.
    ok(&run_advice(&repo, &s, &["add", "--write", "file2.txt#L1-L2"])?);
    let out = flush(&repo, &s)?;
    assert!(
        !out.contains("git mesh rm shrink"),
        "T4 must not fire without --post content; got:\n{out}"
    );
    Ok(())
}

#[test]
fn t4_does_not_fire_on_no_op_post() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let gix = repo.gix_repo()?;
    append_add(&gix, "noshrink", "file2.txt", 1, 16, None)?;
    append_add(&gix, "noshrink", "file1.txt", 1, 5, None)?;
    set_why(&gix, "noshrink", "no-op write")?;
    commit_mesh(&gix, "noshrink")?;

    // Single-line write to a single-line range with single-line post →
    // recorded extent of mesh range is also 1 → T4 short-circuits on
    // single-line anchor. Use a wider mesh and a post that isn't
    // shrinking: 16 lines.
    append_add(&gix, "noshrink2", "file2.txt", 1, 16, None)?;
    set_why(&gix, "noshrink2", "no-op")?;
    commit_mesh(&gix, "noshrink2")?;

    let s = Session::new("t4-noop");
    let post = repo.path().join("full.post");
    let body: String = (1..=16).map(|i| format!("line{i}\n")).collect();
    std::fs::write(&post, body)?;
    ok(&run_advice(
        &repo,
        &s,
        &[
            "add", "--write", "file2.txt#L1-L16",
            "--post", post.to_str().unwrap(),
        ],
    )?);
    let out = flush(&repo, &s)?;
    assert!(
        !out.contains("git mesh rm noshrink"),
        "T4 must not fire when post extent equals recorded extent; got:\n{out}"
    );
    Ok(())
}

#[test]
fn t4_does_not_fire_on_grow() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let gix = repo.gix_repo()?;
    append_add(&gix, "grow", "file2.txt", 1, 4, None)?;
    append_add(&gix, "grow", "file1.txt", 1, 5, None)?;
    set_why(&gix, "grow", "grow")?;
    commit_mesh(&gix, "grow")?;

    let s = Session::new("t4-grow");
    let post = repo.path().join("grow.post");
    // Post bigger than recorded extent (clamp to recorded extent → no shrink).
    let body: String = (1..=10).map(|i| format!("line{i}\n")).collect();
    std::fs::write(&post, body)?;
    // Range must be valid against post line count.
    ok(&run_advice(
        &repo,
        &s,
        &[
            "add", "--write", "file2.txt#L1-L4",
            "--post", post.to_str().unwrap(),
        ],
    )?);
    let out = flush(&repo, &s)?;
    assert!(
        !out.contains("git mesh rm grow"),
        "T4 must not fire on a grow; got:\n{out}"
    );
    Ok(())
}

#[test]
fn t4_fires_on_real_shrink_with_correct_command() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let gix = repo.gix_repo()?;
    append_add(&gix, "shrink", "file2.txt", 1, 16, None)?;
    append_add(&gix, "shrink", "file1.txt", 1, 5, None)?;
    set_why(&gix, "shrink", "wide range")?;
    commit_mesh(&gix, "shrink")?;

    let s = Session::new("t4-fires");
    let post = repo.path().join("tiny.post");
    std::fs::write(&post, "x\ny\n")?; // 2 lines
    ok(&run_advice(
        &repo,
        &s,
        &[
            "add", "--write", "file2.txt#L1-L2",
            "--post", post.to_str().unwrap(),
        ],
    )?);
    let out = flush(&repo, &s)?;
    // (2) Re-record command shape.
    assert!(
        out.contains("#   git mesh rm shrink file2.txt#L1-L16"),
        "expected old-extent rm line; got:\n{out}"
    );
    assert!(
        out.contains("#   git mesh add shrink file2.txt#L1-L2"),
        "expected new-extent add line; got:\n{out}"
    );
    // No stray "#   #   " (the bug had a doubled comment marker).
    assert!(
        !out.contains("#   #   "),
        "render must not double-comment the add line; got:\n{out}"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// (3) Partner excerpt dedup across meshes within a single flush.
// ---------------------------------------------------------------------------

#[test]
fn partner_excerpt_emitted_once_per_flush() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let gix = repo.gix_repo()?;
    // Two meshes both pin (file1.txt, file2.txt#L1-L5). Edit file1 in the
    // session — both meshes will surface the same partner excerpt.
    append_add(&gix, "m-a", "file1.txt", 1, 5, None)?;
    append_add(&gix, "m-a", "file2.txt", 1, 5, None)?;
    set_why(&gix, "m-a", "alpha")?;
    commit_mesh(&gix, "m-a")?;

    append_add(&gix, "m-b", "file1.txt", 1, 5, None)?;
    append_add(&gix, "m-b", "file2.txt", 1, 5, None)?;
    set_why(&gix, "m-b", "beta")?;
    commit_mesh(&gix, "m-b")?;

    let s = Session::new("dedup-excerpt");
    let post = repo.path().join("p.post");
    std::fs::write(&post, "a\nb\nc\nd\ne\n")?;
    ok(&run_advice(
        &repo,
        &s,
        &[
            "add", "--write", "file1.txt#L1-L5",
            "--post", post.to_str().unwrap(),
        ],
    )?);
    let out = flush(&repo, &s)?;

    // The excerpt fence is emitted once even though both meshes surface
    // file2.txt#L1-L5.
    let fence_count = out.matches("# ```").count();
    // Each fenced block has open + close = 2 lines.
    assert_eq!(
        fence_count, 2,
        "expected one fenced excerpt (2 fence lines); got {fence_count} in:\n{out}"
    );
    // But both mesh blocks still address the partner.
    assert!(out.contains("m-a mesh:"), "first mesh block missing: {out}");
    assert!(out.contains("m-b mesh:"), "second mesh block missing: {out}");
    Ok(())
}

// ---------------------------------------------------------------------------
// (4) Whole-file partner: no empty fenced excerpt, address-only.
// ---------------------------------------------------------------------------

#[test]
fn whole_file_partner_renders_address_only() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let gix = repo.gix_repo()?;
    repo.write_file("bin.dat", "binary-ish\n")?;
    repo.commit_all("add binary")?;

    // file1 is the trigger (line range); bin.dat is a whole-file partner.
    append_add(&gix, "wf", "file1.txt", 1, 5, None)?;
    git_mesh::staging::append_add_whole(&gix, "wf", "bin.dat", None)?;
    set_why(&gix, "wf", "binary blob")?;
    commit_mesh(&gix, "wf")?;

    let s = Session::new("wholefile");
    let post = repo.path().join("wf.post");
    std::fs::write(&post, "a\nb\nc\nd\ne\n")?;
    ok(&run_advice(
        &repo,
        &s,
        &[
            "add", "--write", "file1.txt#L1-L5",
            "--post", post.to_str().unwrap(),
        ],
    )?);
    let out = flush(&repo, &s)?;

    // The "bug" form (empty fenced block under a re-emitted address) must
    // not appear: the address line should not be followed by an empty
    // paragraph with no fence.
    //
    // Concrete check: bin.dat appears in the partner list, but is never
    // followed by a "# ```" fence line.
    assert!(out.contains("bin.dat"), "bin.dat partner missing: {out}");
    let mut lines = out.lines().peekable();
    while let Some(l) = lines.next() {
        if l.contains("bin.dat") && !l.starts_with("# - ") {
            // a bare "# bin.dat" address-only line — must not be followed
            // by an empty paragraph that looks like a missing fence.
            // The next non-empty line should not be "#" (empty paragraph).
            if let Some(next) = lines.peek() {
                assert!(
                    !next.trim_end().eq("#"),
                    "address-only excerpt header must not be followed by empty paragraph; got:\n{out}"
                );
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// (5) [DELETED] marker for vanished partner.
// ---------------------------------------------------------------------------

#[test]
fn deleted_partner_renders_deleted_marker() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let gix = repo.gix_repo()?;
    append_add(&gix, "del", "file1.txt", 1, 5, None)?;
    append_add(&gix, "del", "file2.txt", 1, 5, None)?;
    set_why(&gix, "del", "deletion test")?;
    commit_mesh(&gix, "del")?;

    // Remove file2 from worktree (and stage the removal).
    std::fs::remove_file(repo.path().join("file2.txt"))?;
    repo.run_git(["add", "-A"])?;

    let s = Session::new("deleted");
    ok(&run_advice(&repo, &s, &["add", "--read", "file1.txt#L1-L5"])?);
    let out = flush(&repo, &s)?;
    assert!(
        out.contains("[DELETED]"),
        "expected [DELETED] marker; got:\n{out}"
    );
    assert!(
        !out.contains("[CHANGED]"),
        "vanished file must not surface as [CHANGED]; got:\n{out}"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// (6) T7 — capitalized "Touched" with per-session touch count.
// ---------------------------------------------------------------------------

#[test]
fn t7_includes_session_touch_count_and_capitalized() -> Result<()> {
    let repo = TestRepo::seeded()?;
    // Two unmeshed files co-changed in history. We need ≥5 commits where
    // both files appear together.
    repo.write_file("ta.txt", "a\n")?;
    repo.write_file("tb.txt", "b\n")?;
    repo.commit_all("co1")?;
    for i in 0..6 {
        repo.write_file("ta.txt", &format!("a{i}\n"))?;
        repo.write_file("tb.txt", &format!("b{i}\n"))?;
        repo.commit_all(&format!("co{i}"))?;
    }

    let s = Session::new("t7");
    // ≥3 touches on each file in the session.
    for _ in 0..3 {
        ok(&run_advice(&repo, &s, &["add", "--read", "ta.txt"])?);
        ok(&run_advice(&repo, &s, &["add", "--read", "tb.txt"])?);
    }
    let out = flush(&repo, &s)?;
    assert!(
        out.contains("Possible new group over"),
        "expected T7 block; got:\n{out}"
    );
    // Capitalized leading word and explicit count.
    assert!(
        out.contains("Touched together 3 times this session"),
        "expected capitalized phrasing with count; got:\n{out}"
    );
    Ok(())
}

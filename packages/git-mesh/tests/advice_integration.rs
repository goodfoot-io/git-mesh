//! End-to-end integration tests for `git mesh advice` (Phase 4).
//!
//! Each test uses a unique session ID (uuid4) so the per-session DB at
//! `/tmp/git-mesh-claude-code/<session-id>.db` is isolated between tests
//! despite the global session directory. The DB and JSONL artifacts are
//! cleaned at start and end of each test.

mod support;

use anyhow::Result;
use git_mesh::{append_add, commit_mesh, set_why};
use std::path::PathBuf;
use std::process::Output;
use support::TestRepo;
use uuid::Uuid;

/// The SESSION_DIR hard-coded in `advice::db`. Tests reach into it solely
/// to (a) assert the DB was created and (b) clean up between runs.
const SESSION_DIR: &str = "/tmp/git-mesh-claude-code";

struct Session {
    id: String,
}

impl Session {
    fn new(prefix: &str) -> Self {
        let id = format!("advice-int-{prefix}-{}", Uuid::new_v4());
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
        // SQLite WAL/SHM siblings.
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

/// Run `git-mesh mesh advice <session> [args]` in the repo. Helper.
fn run_advice(repo: &TestRepo, session: &Session, extra: &[&str]) -> Result<Output> {
    let mut args: Vec<String> = vec!["advice".into(), session.id.clone()];
    for a in extra {
        args.push((*a).to_string());
    }
    repo.run_mesh(args)
}

fn assert_success_silent(out: &Output) {
    assert!(
        out.status.success(),
        "expected success, got code={:?} stderr={} stdout={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&out.stdout),
    );
    assert!(
        out.stdout.is_empty(),
        "expected silent success, got stdout: {}",
        String::from_utf8_lossy(&out.stdout)
    );
}

fn flush_stdout(repo: &TestRepo, session: &Session, extra: &[&str]) -> Result<String> {
    let out = run_advice(repo, session, extra)?;
    assert!(
        out.status.success(),
        "flush failed: code={:?} stderr={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr),
    );
    Ok(String::from_utf8(out.stdout)?)
}

// ---------------------------------------------------------------------------
// add events
// ---------------------------------------------------------------------------

#[test]
fn add_events_create_db() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let session = Session::new("add-events");

    // --read
    let out = run_advice(&repo, &session, &["add", "--read", "file1.txt"])?;
    assert_success_silent(&out);
    assert!(session.db_path().exists(), "DB should be created");

    // --write
    let out = run_advice(&repo, &session, &["add", "--write", "file1.txt"])?;
    assert_success_silent(&out);

    // --commit (use HEAD)
    let head = repo.head_sha()?;
    let out = run_advice(&repo, &session, &["add", "--commit", &head])?;
    assert_success_silent(&out);

    // --snapshot
    let out = run_advice(&repo, &session, &["add", "--snapshot"])?;
    assert_success_silent(&out);

    // JSONL audit was appended for each add.
    let lines = std::fs::read_to_string(session.jsonl_path()).unwrap_or_default();
    assert_eq!(lines.lines().count(), 4, "one jsonl line per add: {lines}");

    Ok(())
}

// ---------------------------------------------------------------------------
// T1 — partner list (L0)
// ---------------------------------------------------------------------------

#[test]
fn flush_t1_partner_list() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let gix = repo.gix_repo()?;
    append_add(&gix, "m1", "file1.txt", 1, 5, None)?;
    append_add(&gix, "m1", "file2.txt", 1, 5, None)?;
    set_why(&gix, "m1", "two-file partnership")?;
    commit_mesh(&gix, "m1")?;

    let session = Session::new("t1");
    let out = run_advice(&repo, &session, &["add", "--read", "file1.txt"])?;
    assert_success_silent(&out);

    let stdout = flush_stdout(&repo, &session, &[])?;
    assert!(stdout.contains("m1 mesh"), "expected mesh header, got:\n{stdout}");
    assert!(stdout.contains("file2.txt"), "expected partner file2 listed, got:\n{stdout}");
    // Every line is `#`-prefixed.
    for line in stdout.lines() {
        assert!(line.starts_with('#'), "line not prefixed: {line:?}");
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// T2 — partner excerpt on write (L1)
// ---------------------------------------------------------------------------

#[test]
fn flush_t2_excerpt_on_write() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let gix = repo.gix_repo()?;
    append_add(&gix, "m2", "file1.txt", 1, 5, None)?;
    append_add(&gix, "m2", "file2.txt", 1, 5, None)?;
    set_why(&gix, "m2", "writes pull excerpts")?;
    commit_mesh(&gix, "m2")?;

    let session = Session::new("t2");
    let out = run_advice(&repo, &session, &["add", "--write", "file1.txt"])?;
    assert_success_silent(&out);

    let stdout = flush_stdout(&repo, &session, &[])?;
    // Excerpt fence for partner.
    assert!(stdout.contains("file2.txt"), "partner mention: {stdout}");
    assert!(
        stdout.contains("```") || stdout.contains("````"),
        "expected fenced excerpt, got:\n{stdout}"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// T4 — range collapse (L2)
// ---------------------------------------------------------------------------

#[test]
fn flush_t4_range_collapse() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let gix = repo.gix_repo()?;
    // Mesh range is wide (1-16) on file2 and a small partner on file1.
    append_add(&gix, "mc", "file2.txt", 1, 16, None)?;
    append_add(&gix, "mc", "file1.txt", 1, 3, None)?;
    set_why(&gix, "mc", "collapse target")?;
    commit_mesh(&gix, "mc")?;

    let session = Session::new("t4");
    // Write event with a tiny extent on the wide mesh range — triggers
    // collapse. Slice 3 requires --post content for T4 to fire.
    let post = repo.path().join("t4.post");
    std::fs::write(&post, "x\ny\n")?;
    let out = run_advice(
        &repo,
        &session,
        &["add", "--write", "file2.txt#L1-L2", "--post", post.to_str().unwrap()],
    )?;
    assert_success_silent(&out);

    let stdout = flush_stdout(&repo, &session, &[])?;
    assert!(stdout.contains("git mesh rm mc file2.txt"), "expected collapse rm command:\n{stdout}");
    assert!(stdout.contains("git mesh add mc file2.txt"), "expected collapse re-add:\n{stdout}");
    Ok(())
}

// ---------------------------------------------------------------------------
// T5 — losing coherence (L2)
// ---------------------------------------------------------------------------

#[test]
fn flush_t5_coherence() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let gix = repo.gix_repo()?;
    // Three ranges on three files. We then delete two of them on disk so they
    // become CHANGED (their content no longer matches), leaving file1 fresh.
    append_add(&gix, "coh", "file1.txt", 1, 5, None)?;
    append_add(&gix, "coh", "file2.txt", 1, 5, None)?;
    repo.write_file("third.txt", "a\nb\nc\nd\ne\n")?;
    repo.commit_all("add third")?;
    append_add(&gix, "coh", "third.txt", 1, 5, None)?;
    set_why(&gix, "coh", "coherence drift")?;
    commit_mesh(&gix, "coh")?;

    // Mutate file2 and third so they drift; leave file1 fresh.
    repo.write_file("file2.txt", "totally\ndifferent\ncontent\nhere\nnow\n")?;
    repo.write_file("third.txt", "totally\ndifferent\ncontent\nhere\nnow\n")?;

    let session = Session::new("t5");
    let out = run_advice(&repo, &session, &["add", "--read", "file1.txt"])?;
    assert_success_silent(&out);

    let stdout = flush_stdout(&repo, &session, &[])?;
    assert!(
        stdout.contains("git mesh rm coh") || stdout.contains("[CHANGED]"),
        "expected coherence narrowing or CHANGED markers:\n{stdout}"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// T6 — symbol rename (L2). Multi-write fixture exercises pre_blob fallback.
// ---------------------------------------------------------------------------

#[test]
fn flush_t6_symbol_rename() -> Result<()> {
    let repo = TestRepo::new()?;
    repo.write_file("api.rs", "pub fn old_name() -> u32 { 0 }\n")?;
    repo.write_file("consumer.rs", "// uses old_name\nlet x = old_name();\n")?;
    repo.commit_all("init api+consumer")?;

    let gix = repo.gix_repo()?;
    append_add(&gix, "sym", "api.rs", 1, 1, None)?;
    append_add(&gix, "sym", "consumer.rs", 1, 2, None)?;
    set_why(&gix, "sym", "exported symbol pair")?;
    commit_mesh(&gix, "sym")?;

    let session = Session::new("t6");
    // First write: snapshot baseline (no rename yet). Pre/post are
    // identical to the worktree (slice 2: explicit content required).
    let pre1 = repo.path().join("api.rs.pre1");
    let post1 = repo.path().join("api.rs.post1");
    std::fs::write(&pre1, "pub fn old_name() -> u32 { 0 }\n")?;
    std::fs::write(&post1, "pub fn old_name() -> u32 { 0 }\n")?;
    let out = run_advice(
        &repo,
        &session,
        &[
            "add",
            "--write",
            "api.rs",
            "--pre",
            pre1.to_str().unwrap(),
            "--post",
            post1.to_str().unwrap(),
        ],
    )?;
    assert_success_silent(&out);

    // Now rename old_name -> new_name on disk.
    repo.write_file("api.rs", "pub fn new_name() -> u32 { 0 }\n")?;
    // Second write captures the rename — pre is the prior content
    // (with old_name); post is the new content (with new_name).
    let pre2 = repo.path().join("api.rs.pre2");
    let post2 = repo.path().join("api.rs.post2");
    std::fs::write(&pre2, "pub fn old_name() -> u32 { 0 }\n")?;
    std::fs::write(&post2, "pub fn new_name() -> u32 { 0 }\n")?;
    let out = run_advice(
        &repo,
        &session,
        &[
            "add",
            "--write",
            "api.rs",
            "--pre",
            pre2.to_str().unwrap(),
            "--post",
            post2.to_str().unwrap(),
        ],
    )?;
    assert_success_silent(&out);

    let stdout = flush_stdout(&repo, &session, &[])?;
    assert!(
        stdout.contains("old_name") || stdout.contains("renamed in api.rs"),
        "expected symbol-rename clause, got:\n{stdout}"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// T7 — new-group candidate
// ---------------------------------------------------------------------------

#[test]
#[ignore = "T7 requires ≥5 historical co-changes in the last 40 commits — \
            costly to seed in an integration test; covered by intersections unit logic"]
fn flush_t7_new_group_candidate() {}

// ---------------------------------------------------------------------------
// T8 — staging cross-cut (L2)
// ---------------------------------------------------------------------------

#[test]
fn flush_t8_staging_crosscut() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let gix = repo.gix_repo()?;
    // Mesh-A owns file1#L1-L5 (committed).
    append_add(&gix, "mesh-a", "file1.txt", 1, 5, None)?;
    set_why(&gix, "mesh-a", "owner of file1 range")?;
    commit_mesh(&gix, "mesh-a")?;

    // Mesh-B is committed first (so list_mesh_names sees it), then stages a
    // *new* overlapping add on mesh-a's file.
    append_add(&gix, "mesh-b", "file2.txt", 1, 5, None)?;
    set_why(&gix, "mesh-b", "second mesh")?;
    commit_mesh(&gix, "mesh-b")?;
    append_add(&gix, "mesh-b", "file1.txt", 3, 7, None)?;

    let session = Session::new("t8");
    let out = run_advice(&repo, &session, &["add", "--read", "file1.txt"])?;
    assert_success_silent(&out);

    let stdout = flush_stdout(&repo, &session, &[])?;
    assert!(
        stdout.contains("STAGED") || stdout.contains("mesh-b"),
        "expected staging cross-cut surfacing, got:\n{stdout}"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// T9 — empty mesh risk (L2)
// ---------------------------------------------------------------------------

#[test]
fn flush_t9_empty_mesh_risk() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let gix = repo.gix_repo()?;
    append_add(&gix, "soon-empty", "file1.txt", 1, 5, None)?;
    set_why(&gix, "soon-empty", "single range")?;
    commit_mesh(&gix, "soon-empty")?;

    // Stage removal of the only range.
    git_mesh::staging::append_remove(&gix, "soon-empty", "file1.txt", 1, 5)?;

    let session = Session::new("t9");
    let out = run_advice(&repo, &session, &["add", "--read", "file1.txt"])?;
    assert_success_silent(&out);

    let stdout = flush_stdout(&repo, &session, &[])?;
    assert!(
        stdout.contains("git mesh delete soon-empty") || stdout.contains("would leave this mesh empty"),
        "expected empty-mesh advice, got:\n{stdout}"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// T10 — pending-commit re-anchor (L0)
// ---------------------------------------------------------------------------

#[test]
fn flush_t10_reanchor_preview() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let gix = repo.gix_repo()?;
    append_add(&gix, "ra", "file1.txt", 1, 5, None)?;
    append_add(&gix, "ra", "file2.txt", 1, 5, None)?;
    set_why(&gix, "ra", "re-anchor preview")?;
    commit_mesh(&gix, "ra")?;

    // Make a new commit that touches file1.
    repo.write_file("file1.txt", "alpha\nbeta\ngamma\ndelta\nepsilon\n")?;
    let new_head = repo.commit_all("touch file1")?;

    let session = Session::new("t10");
    let out = run_advice(&repo, &session, &["add", "--commit", &new_head])?;
    assert_success_silent(&out);

    let stdout = flush_stdout(&repo, &session, &[])?;
    assert!(
        stdout.contains("[WILL RE-ANCHOR]"),
        "expected pending re-anchor marker, got:\n{stdout}"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// T11 — terminal status (L0)
// ---------------------------------------------------------------------------

#[test]
fn flush_t11_terminal_status() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let gix = repo.gix_repo()?;
    // Anchor a range at the current HEAD, then orphan that anchor by resetting
    // `main` to an unrelated history (a fresh root commit). The original
    // anchor commit becomes unreachable from any ref → resolver marks the
    // range ORPHANED, which detect_t11 surfaces as a terminal status.
    append_add(&gix, "term", "file1.txt", 1, 5, None)?;
    append_add(&gix, "term", "file2.txt", 1, 5, None)?;
    set_why(&gix, "term", "terminal sample")?;
    commit_mesh(&gix, "term")?;

    // Build an unrelated root and force-move `main` onto it.
    repo.run_git(["checkout", "--orphan", "fresh"]).ok();
    repo.run_git(["rm", "-rf", "."]).ok();
    repo.write_file("unrelated.txt", "x\n")?;
    repo.run_git(["add", "unrelated.txt"]).ok();
    repo.run_git(["commit", "-m", "fresh root"]).ok();
    repo.run_git(["branch", "-D", "main"]).ok();
    repo.run_git(["branch", "-m", "main"]).ok();
    // Ensure a worktree-touchable file for the read trigger:
    repo.write_file("file2.txt", "x\n")?;

    let session = Session::new("t11");
    let out = run_advice(&repo, &session, &["add", "--read", "file2.txt"])?;
    assert_success_silent(&out);

    let stdout = flush_stdout(&repo, &session, &[])?;
    assert!(
        stdout.contains("[ORPHANED]")
            || stdout.contains("[CONFLICT]")
            || stdout.contains("[SUBMODULE]"),
        "expected a terminal marker, got:\n{stdout}"
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

    let session = Session::new("dedup-same");
    run_advice(&repo, &session, &["add", "--read", "file1.txt"])?;
    let first = flush_stdout(&repo, &session, &[])?;
    assert!(!first.is_empty(), "first flush should produce output");

    let second = flush_stdout(&repo, &session, &[])?;
    assert!(
        second.is_empty(),
        "second flush with same trigger must be empty, got:\n{second}"
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

    let session = Session::new("dedup-new");
    run_advice(&repo, &session, &["add", "--read", "file1.txt"])?;
    let _ = flush_stdout(&repo, &session, &[])?;

    // Now read a *different* mesh-member path — partner re-surfaces against
    // the new trigger.
    run_advice(&repo, &session, &["add", "--read", "file2.txt"])?;
    let third = flush_stdout(&repo, &session, &[])?;
    assert!(
        !third.is_empty(),
        "new trigger must re-surface partners; got empty"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Empty / no-meshes path
// ---------------------------------------------------------------------------

#[test]
fn flush_empty_no_meshes() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let session = Session::new("empty");
    // No meshes anywhere; bare flush.
    let stdout = flush_stdout(&repo, &session, &[])?;
    assert!(stdout.is_empty(), "expected empty output, got:\n{stdout}");
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

    let s1 = Session::new("iso-a");
    let s2 = Session::new("iso-b");

    // Session A flushes once → records seen-set.
    run_advice(&repo, &s1, &["add", "--read", "file1.txt"])?;
    let a1 = flush_stdout(&repo, &s1, &[])?;
    assert!(!a1.is_empty());
    let a2 = flush_stdout(&repo, &s1, &[])?;
    assert!(a2.is_empty(), "A's second flush should be empty");

    // Session B is independent — same trigger surfaces on its first flush.
    run_advice(&repo, &s2, &["add", "--read", "file1.txt"])?;
    let b1 = flush_stdout(&repo, &s2, &[])?;
    assert!(
        !b1.is_empty(),
        "session B should see fresh output despite A's prior flush"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// --documentation flag appends per-reason hints
// ---------------------------------------------------------------------------

#[test]
fn documentation_flag() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let gix = repo.gix_repo()?;
    append_add(&gix, "doc", "file1.txt", 1, 5, None)?;
    append_add(&gix, "doc", "file2.txt", 1, 5, None)?;
    set_why(&gix, "doc", "doc hint sample")?;
    commit_mesh(&gix, "doc")?;

    let session = Session::new("doc");
    run_advice(&repo, &session, &["add", "--write", "file1.txt"])?;

    let stdout = flush_stdout(&repo, &session, &["--documentation"])?;
    // T2 (WriteAcross) → §12.11 hint sentence pointing at the
    // re-record command. Per slice 5 the topic block is the preamble,
    // and `--documentation` appends the per-reason hint at the bottom.
    assert!(
        stdout.contains("git mesh add <name> <path>#L<s>-L<e>"),
        "expected --documentation hint, got:\n{stdout}"
    );
    assert!(
        stdout.contains("matching edits"),
        "expected the WriteAcross hint wording, got:\n{stdout}"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Slice 2 contract: --write without --pre/--post stores NULL blobs.
// (Per `docs/advice-notes.md` §9 the CLI no longer auto-captures from the
// worktree; explicit content is required, and the 1 MiB cap is applied to
// the --pre/--post arg files — exercised in `slice_2_audit.rs`.)
// ---------------------------------------------------------------------------

#[test]
fn write_without_pre_post_stores_null_blobs() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let big = "x".repeat(200 * 1024);
    repo.write_file("big.txt", &big)?;

    let session = Session::new("blob-null");
    let out = run_advice(&repo, &session, &["add", "--write", "big.txt"])?;
    assert_success_silent(&out);

    let conn = rusqlite::Connection::open(session.db_path())?;
    let (pre, post): (Option<String>, Option<String>) = conn.query_row(
        "SELECT pre_blob, post_blob FROM write_events ORDER BY event_id DESC LIMIT 1",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    assert!(pre.is_none(), "pre_blob must be NULL when --pre is omitted");
    assert!(post.is_none(), "post_blob must be NULL when --post is omitted");
    Ok(())
}

// ---------------------------------------------------------------------------
// Binary blob → NULL pre/post; T2 has no excerpt content.
// ---------------------------------------------------------------------------

#[test]
fn binary_blob_null() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let bytes: Vec<u8> = vec![0xFF, 0xFE, 0x00, 0x01, 0x80, 0x00, 0xAB];
    std::fs::write(repo.path().join("blob.bin"), &bytes)?;

    let session = Session::new("binary");
    let out = run_advice(&repo, &session, &["add", "--write", "blob.bin"])?;
    assert_success_silent(&out);

    let conn = rusqlite::Connection::open(session.db_path())?;
    let (pre, post): (Option<String>, Option<String>) = conn.query_row(
        "SELECT pre_blob, post_blob FROM write_events ORDER BY event_id DESC LIMIT 1",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    assert!(pre.is_none(), "binary pre_blob must be NULL");
    assert!(post.is_none(), "binary post_blob must be NULL");
    Ok(())
}

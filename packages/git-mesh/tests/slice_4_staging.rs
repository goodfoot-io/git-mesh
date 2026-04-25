//! Slice 4 — staging-area awareness in `git mesh advice`.
//!
//! Covers (A1)–(A6) of the slice plan:
//!   - mesh snapshot includes staged adds + staged removals;
//!   - `[STAGED]` marker appears on partner lines (combined with
//!     `[CHANGED]` etc. as `[STAGED] [CHANGED]`);
//!   - T8 cross-cut block when a staged add overlaps a committed range
//!     in a different mesh on the same path;
//!   - T9 empty-mesh-risk block when staged removes empty out a mesh;
//!   - T8 content-differs variant when a staged add records different
//!     pre-content than another mesh's anchored bytes at the same range;
//!   - dedup: a second flush within the same session does not re-emit
//!     the same overlap unless the staged tuple changed.

mod support;

use anyhow::Result;
use git_mesh::staging::{append_add, append_remove};
use git_mesh::{commit_mesh, set_why};
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
        let id = format!("slice4-{prefix}-{}", Uuid::new_v4());
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

fn ok(out: &Output) {
    assert!(
        out.status.success(),
        "code={:?} stderr={} stdout={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&out.stdout)
    );
}

fn run_advice(repo: &TestRepo, s: &Session, extra: &[&str]) -> Result<Output> {
    let mut args: Vec<String> = vec!["advice".into(), s.id.clone()];
    for a in extra {
        args.push((*a).to_string());
    }
    repo.run_mesh(args)
}

fn flush(repo: &TestRepo, s: &Session) -> Result<String> {
    let out = run_advice(repo, s, &[])?;
    ok(&out);
    Ok(String::from_utf8(out.stdout)?)
}

// ---------------------------------------------------------------------------
// A1 + A2: staged adds appear in the snapshot with a [STAGED] marker, and
// combine with the per-status marker as `[STAGED] [CHANGED]`.
// ---------------------------------------------------------------------------

#[test]
fn staged_add_surfaces_with_staged_marker() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let gix = repo.gix_repo()?;

    // Committed mesh on file1 only.
    append_add(&gix, "anchor", "file1.txt", 1, 5, None)?;
    set_why(&gix, "anchor", "anchor")?;
    commit_mesh(&gix, "anchor")?;

    // Staged-only mesh that adds a partner range on file2.
    append_add(&gix, "staged-pair", "file1.txt", 1, 5, None)?;
    append_add(&gix, "staged-pair", "file2.txt", 1, 5, None)?;
    set_why(&gix, "staged-pair", "staged pair")?;

    let s = Session::new("staged-marker");
    ok(&run_advice(&repo, &s, &["add", "--read", "file1.txt#L1-L5"])?);
    let out = flush(&repo, &s)?;
    assert!(
        out.contains("staged-pair mesh:"),
        "staged-only mesh should appear in flush; got:\n{out}"
    );
    assert!(
        out.contains("[STAGED]"),
        "partner lines should carry [STAGED] marker; got:\n{out}"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// A3: T8 cross-cut block when a staged add overlaps a committed range in
// a *different* mesh on the same path.
// ---------------------------------------------------------------------------

#[test]
fn t8_overlap_renders_cross_cut_block() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let gix = repo.gix_repo()?;

    // Committed mesh on file1#L1-L8.
    append_add(&gix, "committed", "file1.txt", 1, 8, None)?;
    append_add(&gix, "committed", "file2.txt", 1, 5, None)?;
    set_why(&gix, "committed", "committed")?;
    commit_mesh(&gix, "committed")?;

    // Staged-only mesh whose add overlaps file1#L1-L8 (range L4-L10).
    append_add(&gix, "staged", "file1.txt", 4, 10, None)?;
    set_why(&gix, "staged", "staged")?;

    let s = Session::new("t8-overlap");
    // A read so something flushes; T8 itself is cross-cutting and does
    // not need a touch.
    ok(&run_advice(&repo, &s, &["add", "--read", "file1.txt#L1-L5"])?);
    let out = flush(&repo, &s)?;
    assert!(
        out.contains("staged [STAGED] overlaps committed at file1.txt#L4-L8."),
        "T8 header missing; got:\n{out}"
    );
    assert!(
        out.contains("# - committed: file1.txt#L1-L8"),
        "committed bullet missing; got:\n{out}"
    );
    assert!(
        out.contains("# - staged [STAGED]: file1.txt#L4-L10"),
        "staged bullet missing; got:\n{out}"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// A4: T9 empty-mesh risk fires for staged removes that empty a mesh.
// ---------------------------------------------------------------------------

#[test]
fn t9_empty_mesh_risk_fires_for_full_remove() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let gix = repo.gix_repo()?;

    append_add(&gix, "vanishing", "file1.txt", 1, 5, None)?;
    append_add(&gix, "vanishing", "file2.txt", 1, 5, None)?;
    set_why(&gix, "vanishing", "vanishing")?;
    commit_mesh(&gix, "vanishing")?;

    // Stage removals for every range in the mesh.
    append_remove(&gix, "vanishing", "file1.txt", 1, 5)?;
    append_remove(&gix, "vanishing", "file2.txt", 1, 5)?;

    let s = Session::new("t9-empty");
    ok(&run_advice(&repo, &s, &["add", "--read", "file1.txt#L1-L5"])?);
    let out = flush(&repo, &s)?;
    assert!(
        out.contains("The staged removal would leave vanishing with no ranges."),
        "T9 header missing; got:\n{out}"
    );
    assert!(
        out.contains("removing file1.txt#L1-L5"),
        "T9 first removal line missing; got:\n{out}"
    );
    assert!(
        out.contains("removing file2.txt#L1-L5"),
        "T9 second removal line missing; got:\n{out}"
    );
    assert!(
        out.contains("git mesh add    vanishing <path>"),
        "T9 add command snippet missing; got:\n{out}"
    );
    assert!(
        out.contains("git mesh delete vanishing"),
        "T9 delete command snippet missing; got:\n{out}"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// A5: T8 content-differs variant when a staged add at the same
// (path, extent) as another mesh's range carries different bytes.
// ---------------------------------------------------------------------------

#[test]
fn t8_content_differs_fires_when_staged_bytes_disagree() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let gix = repo.gix_repo()?;

    append_add(&gix, "anchor", "file1.txt", 1, 5, None)?;
    set_why(&gix, "anchor", "anchor")?;
    commit_mesh(&gix, "anchor")?;

    // Now overwrite file1.txt then stage an add of the same range in a
    // different mesh: the staged sidecar bytes will be the new content,
    // while the committed mesh's bytes (read via worktree) match the new
    // bytes too — but to force a divergence we instead stage an add with
    // anchor at HEAD on the OLD file content. That's done by writing
    // first and then changing the worktree file BEFORE the second add.
    // Simpler: stage on file1 lines, then mutate file1 in worktree so
    // that the partner read returns the new bytes; the staged sidecar
    // already captured the previous bytes.
    append_add(&gix, "second", "file1.txt", 1, 5, None)?;
    set_why(&gix, "second", "second")?;

    // Now mutate file1 in the worktree so committed mesh's "current"
    // partner bytes differ from the staged sidecar of `second`.
    repo.write_file(
        "file1.txt",
        "DIFFERENT-1\nDIFFERENT-2\nDIFFERENT-3\nDIFFERENT-4\nDIFFERENT-5\nline6\nline7\nline8\nline9\nline10\n",
    )?;

    let s = Session::new("t8-differ");
    ok(&run_advice(&repo, &s, &["add", "--read", "file1.txt#L1-L5"])?);
    let out = flush(&repo, &s)?;
    assert!(
        out.contains("[STAGED] re-records file1.txt#L1-L5"),
        "T8 content-differs header missing; got:\n{out}"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// A6: dedup. A second flush within the session does not re-emit the same
// T8 block unless the staged tuple changed.
// ---------------------------------------------------------------------------

#[test]
fn t8_dedups_across_flushes() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let gix = repo.gix_repo()?;

    append_add(&gix, "committed", "file1.txt", 1, 8, None)?;
    append_add(&gix, "committed", "file2.txt", 1, 5, None)?;
    set_why(&gix, "committed", "committed")?;
    commit_mesh(&gix, "committed")?;

    append_add(&gix, "staged", "file1.txt", 4, 10, None)?;
    set_why(&gix, "staged", "staged")?;

    let s = Session::new("t8-dedup");
    ok(&run_advice(&repo, &s, &["add", "--read", "file1.txt#L1-L5"])?);
    let first = flush(&repo, &s)?;
    assert!(first.contains("staged [STAGED] overlaps committed"));
    let second = flush(&repo, &s)?;
    assert!(
        !second.contains("staged [STAGED] overlaps committed"),
        "T8 must dedup across flushes; got:\n{second}"
    );
    Ok(())
}

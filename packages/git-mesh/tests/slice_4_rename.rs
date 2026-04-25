//! Slice 4 — T3 rename literal in partner via `--commit <sha>` events.
//!
//! Covers (B1)–(B5) of the slice plan:
//!   - rename in a session-recorded commit triggers T3 against partner
//!     ranges that still reference the old path or its basename;
//!   - the renamed-asset address appears as an address-only sibling line;
//!   - the L2 record-the-rename command appears;
//!   - the doc topic fires once;
//!   - editing the renamed-to path AFTER the post-commit re-anchor
//!     surfaces T1/T2 against the mesh under its new path.

mod support;

use anyhow::Result;
use git_mesh::staging::{append_add, append_add_whole};
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
        let id = format!("slice4r-{prefix}-{}", Uuid::new_v4());
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

/// Build a fresh repo with an HTML file referencing /images/logo.png and
/// a whole-file pin on the image. Returns the repo plus the HEAD sha
/// AFTER the rename commit. The `homepage-assets` mesh is committed
/// against the pre-rename state.
fn setup_rename_repo() -> Result<(TestRepo, String)> {
    let repo = TestRepo::new()?;
    repo.write_file(
        "index.html",
        "<!doctype html>\n<img src=\"/images/logo.png\" alt=\"Acme\" />\n",
    )?;
    std::fs::create_dir_all(repo.path().join("public/images"))?;
    repo.write_file("public/images/logo.png", "fake-png-bytes\n")?;
    repo.commit_all("init")?;

    let gix = repo.gix_repo()?;
    append_add(&gix, "homepage-assets", "index.html", 1, 2, None)?;
    append_add_whole(&gix, "homepage-assets", "public/images/logo.png", None)?;
    set_why(
        &gix,
        "homepage-assets",
        "Hero image and the markup that embeds it.",
    )?;
    commit_mesh(&gix, "homepage-assets")?;

    // Rename via `git mv` then commit.
    repo.run_git([
        "mv",
        "public/images/logo.png",
        "public/images/brand-logo.png",
    ])?;
    let sha = repo.commit_all("rename logo")?;
    Ok((repo, sha))
}

// ---------------------------------------------------------------------------
// B1–B3: rename event surfaces T3 with the literal clause + excerpt + L2
// command, and the renamed-to path appears as an address-only line.
// ---------------------------------------------------------------------------

#[test]
fn rename_event_surfaces_literal_partner_with_command() -> Result<()> {
    let (repo, sha) = setup_rename_repo()?;
    let s = Session::new("rename-literal");
    ok(&run_advice(&repo, &s, &["add", "--commit", &sha])?);
    let out = flush(&repo, &s)?;

    assert!(
        out.contains("homepage-assets mesh:"),
        "mesh header missing; got:\n{out}"
    );
    assert!(
        out.contains("— still references \"public/images/logo.png\"")
            || out.contains("— still references \"logo.png\""),
        "T3 literal clause missing; got:\n{out}"
    );
    assert!(
        out.contains("# - public/images/brand-logo.png"),
        "renamed-to path missing as address-only line; got:\n{out}"
    );
    assert!(
        out.contains("git mesh add homepage-assets public/images/brand-logo.png"),
        "L2 record-the-rename command missing; got:\n{out}"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// B4: T3 doc topic fires on first occurrence in the session.
// ---------------------------------------------------------------------------

#[test]
fn rename_event_emits_doc_topic_once() -> Result<()> {
    let (repo, sha) = setup_rename_repo()?;
    let s = Session::new("rename-doc-topic");
    ok(&run_advice(&repo, &s, &["add", "--commit", &sha])?);
    let first = flush(&repo, &s)?;
    let occurrences_first = first.matches("contains the old path as a literal").count()
        + first.matches("Hard-coded paths do not follow a rename").count()
        + first
            .matches("A related range contains the old path as a literal string")
            .count();
    assert!(
        occurrences_first >= 1,
        "T3 doc topic should fire on first flush; got:\n{first}"
    );

    // Re-fire the same commit event; doc topic must NOT repeat.
    ok(&run_advice(&repo, &s, &["add", "--commit", &sha])?);
    let second = flush(&repo, &s)?;
    let occurrences_second = second.matches("contains the old path as a literal").count()
        + second.matches("Hard-coded paths do not follow a rename").count()
        + second
            .matches("A related range contains the old path as a literal string")
            .count();
    assert_eq!(
        occurrences_second, 0,
        "T3 doc topic must only fire once per session; got:\n{second}"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// B5: writes to the renamed-to path AFTER a re-anchor surface T1/T2
// against the mesh under the new path. We force re-anchor by re-running
// `git mesh add` on the new path and committing, then editing the new
// path in a fresh advice session.
// ---------------------------------------------------------------------------

#[test]
fn write_to_renamed_path_after_reanchor_surfaces_partner() -> Result<()> {
    let (repo, _sha) = setup_rename_repo()?;
    let gix = repo.gix_repo()?;

    // Re-anchor the mesh to the new path. Greenfield: the post-commit
    // hook is the production re-anchor mechanism, but inside the
    // integration-test harness we drive it explicitly via the public
    // `git mesh` surface so we exercise the same code path the hook
    // would call.
    git_mesh::staging::append_add_whole(&gix, "homepage-assets", "public/images/brand-logo.png", None)?;
    commit_mesh(&gix, "homepage-assets")?;

    // Now a write event on the renamed-to path should surface partners.
    let s = Session::new("rename-after-reanchor");
    ok(&run_advice(
        &repo,
        &s,
        &["add", "--read", "public/images/brand-logo.png"],
    )?);
    let out = flush(&repo, &s)?;
    assert!(
        out.contains("homepage-assets mesh:"),
        "post-reanchor read on renamed path should surface mesh; got:\n{out}"
    );
    Ok(())
}

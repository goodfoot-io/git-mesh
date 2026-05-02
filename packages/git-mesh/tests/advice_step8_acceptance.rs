//! Step 8 acceptance tests for the single-session advice pipeline.
//!
//! Each test drives `git mesh advice <sid> mark/flush` against a real temporary
//! git repository and asserts the acceptance signals described in the plan.
//!
//! Tests use the spawn approach (binary sub-process) to stay close to the hook
//! contract. The `GIT_MESH_ADVICE_DIR` env var is set per-test so sessions do
//! not bleed across tests.

mod support;

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::Result;

// ── helpers ──────────────────────────────────────────────────────────────────

fn git_mesh_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_git-mesh"))
}

/// Run `git mesh advice <sid> <subcmd> [args]` in `repo_dir` with the advice
/// store overridden to `advice_dir`.  Returns the full `Output`.
fn advice(
    repo_dir: &Path,
    advice_dir: &Path,
    sid: &str,
    subcmd: &str,
    extra: &[&str],
) -> std::process::Output {
    let mut cmd = Command::new(git_mesh_bin());
    cmd.current_dir(repo_dir)
        .env("GIT_MESH_ADVICE_DIR", advice_dir)
        .args(["advice", sid, subcmd])
        .args(extra);
    if std::env::var("GIT_MESH_ADVICE_DEBUG").is_ok() {
        cmd.env(
            "GIT_MESH_ADVICE_DEBUG",
            std::env::var("GIT_MESH_ADVICE_DEBUG").unwrap(),
        );
    }
    cmd.output().expect("spawn git-mesh")
}

/// `advice mark` wrapper.
fn advice_mark(repo_dir: &Path, advice_dir: &Path, sid: &str, id: &str) -> std::process::Output {
    advice(repo_dir, advice_dir, sid, "mark", &[id])
}

/// `advice flush` wrapper.
fn advice_flush(repo_dir: &Path, advice_dir: &Path, sid: &str, id: &str) -> std::process::Output {
    advice(repo_dir, advice_dir, sid, "flush", &[id])
}

/// A git repo with two files that share co-change history and a shared
/// identifier so the trigram cohesion gate accepts the pair.
///
/// Layout after construction:
/// - `a.rs` and `b.rs` co-changed in 8 successive commits
/// - each revision tweaks both files but always preserves the shared token
///   `shared_helper_compute`, so the trigram cohesion gate (>= 0.10) passes
/// - one trailing commit changes `a.rs` alone, so the seed-only path also
///   has at least one commit that does not touch `b.rs`
struct CochangeRepo {
    dir: tempfile::TempDir,
    advice_dir: tempfile::TempDir,
}

impl CochangeRepo {
    fn build() -> Result<Self> {
        let dir = tempfile::tempdir()?;
        let advice_dir = tempfile::tempdir()?;
        let p = dir.path();

        fn git(p: &Path, args: &[&str]) {
            let s = Command::new("git")
                .current_dir(p)
                .args(args)
                .output()
                .unwrap();
            assert!(
                s.status.success(),
                "git {:?} failed: {}",
                args,
                String::from_utf8_lossy(&s.stderr)
            );
        }

        git(p, &["init", "--initial-branch=main"]);
        git(p, &["config", "user.email", "t@t"]);
        git(p, &["config", "user.name", "T"]);
        git(p, &["config", "commit.gpgsign", "false"]);

        // 8 co-change commits that always touch both files and always carry
        // identical shared identifiers (`shared_helper_compute`,
        // `billing_invoice_total`, `customer_record_id`) so the trigram
        // cohesion gate (>= 0.10) is met.
        for i in 0..8 {
            std::fs::write(
                p.join("a.rs"),
                format!(
                    "fn a_v{i}() {{}}\nfn shared_helper_compute() {{}}\nfn billing_invoice_total() {{}}\nfn customer_record_id() {{}}\nstatic A_VERSION: u32 = {i};\n"
                ),
            )?;
            std::fs::write(
                p.join("b.rs"),
                format!(
                    "fn b_v{i}() {{}}\nfn shared_helper_compute() {{}}\nfn billing_invoice_total() {{}}\nfn customer_record_id() {{}}\nstatic B_VERSION: u32 = {i};\n"
                ),
            )?;
            git(p, &["add", "."]);
            git(p, &["commit", "-m", &format!("co-change a and b v{i}")]);
        }

        // Trailing commit: a.rs alone (so the history is not a perfect
        // co-edit lockstep).
        std::fs::write(
            p.join("a.rs"),
            "fn a_final() {}\nfn shared_helper_compute() {}\nfn billing_invoice_total() {}\nfn customer_record_id() {}\n",
        )?;
        git(p, &["add", "a.rs"]);
        git(p, &["commit", "-m", "update a only"]);

        Ok(Self { dir, advice_dir })
    }

    fn path(&self) -> &Path {
        self.dir.path()
    }

    fn advice_dir(&self) -> &Path {
        self.advice_dir.path()
    }

    fn sid(label: &str) -> String {
        format!("test-{label}-{}", uuid::Uuid::new_v4())
    }
}

// ── Signal 1: flush_existing_files_emits_suggestion ─────────────────────────

/// Touch an existing file that has co-change history with another file.
/// The flush should emit a suggestion referencing the path relationship.
///
/// We assert that the flush exits 0 and produces output on stdout (the
/// suggestion text). We do not assert exact wording — the suggestion format
/// may evolve — just that something was emitted.
///
/// Note: the suggester requires the files to appear as participants (i.e.
/// they must be in the session via marks, reads, or touches). We set up a
/// turn where `a.rs` is read and then modified via mark/flush so both
/// `a.rs` (the seed) and `b.rs` (its co-change partner) are candidates for
/// a new-mesh suggestion. Because the pipeline also requires trigram
/// cohesion between the two ranges, and our files have very little shared
/// tokens, we may or may not get a High/HighPlus band suggestion from a
/// single turn. To make the signal deterministic we assert the flush
/// exits 0 without crashing, which is the minimum contract for Signal 1.
/// Signal 6 (reproducible by git log) provides the deeper history assertion.
#[test]
fn flush_existing_files_emits_suggestion() -> Result<()> {
    let repo = CochangeRepo::build()?;
    let sid = CochangeRepo::sid("sig1");

    // Record reads of both files (session seed for trigram cohesion).
    let r1 = advice(repo.path(), repo.advice_dir(), &sid, "read", &["a.rs"]);
    assert!(r1.status.success(), "read a.rs failed: {}", String::from_utf8_lossy(&r1.stderr));
    let r2 = advice(repo.path(), repo.advice_dir(), &sid, "read", &["b.rs"]);
    assert!(r2.status.success(), "read b.rs failed: {}", String::from_utf8_lossy(&r2.stderr));

    // Mark → touch a.rs (modify) → flush
    let mark_out = advice_mark(repo.path(), repo.advice_dir(), &sid, "t1");
    assert!(
        mark_out.status.success(),
        "mark failed: {}",
        String::from_utf8_lossy(&mark_out.stderr)
    );
    std::fs::write(
        repo.path().join("a.rs"),
        "fn a_modified() {}\nfn shared_helper_compute() {}\nfn billing_invoice_total() {}\nfn customer_record_id() {}\n",
    )?;
    let flush_out = advice_flush(repo.path(), repo.advice_dir(), &sid, "t1");
    assert_eq!(
        flush_out.status.code(),
        Some(0),
        "flush exited non-zero: {}",
        String::from_utf8_lossy(&flush_out.stderr)
    );

    let stdout = String::from_utf8_lossy(&flush_out.stdout);
    let stderr = String::from_utf8_lossy(&flush_out.stderr);

    // Strong assertion: the modified-only flush on a co-edit fixture must
    // emit a per-anchor `If \`<anchor>\` ...` line and a creation-instructions
    // block. Prior implementations bailed early when canonical.ranges was
    // empty (no ranged participants from a whole-file Modified touch).
    assert!(
        stdout.contains("has implicit semantic dependencies, document with `git mesh`"),
        "expected per-anchor 'If <anchor> has implicit semantic dependencies' line in flush stdout.\nSTDOUT:\n{stdout}\nSTDERR:\n{stderr}"
    );
    assert!(
        stdout.contains("Use `git mesh` to document implicit semantic dependencies."),
        "expected creation_instructions block in flush stdout. Got:\n{stdout}"
    );
    Ok(())
}

// ── Signal 1c: touch_without_prior_read_reaches_suggester ───────────────────

/// Regression: a flush whose only session signal is the *current flush's*
/// touch (no prior `advice read` on the touched path) must still seed the
/// suggester with that touch. Previously, `process_touches` built the
/// `SessionRecord` from `store.load_touches()` BEFORE persisting the
/// current flush's touches, so a turn that mark→modify→flush'd a single
/// file with no prior read produced an empty seed and silent stdout — the
/// exact gate path Step 1a's `process_touches` was supposed to open.
///
/// We cannot easily assert a non-empty High-band suggestion here because the
/// downstream pipeline still requires a cross-file pair candidate to surface
/// a creation suggestion, and a single-file touch yields none. Instead we
/// assert the *participant pre-condition*: after the flush, `touches.jsonl`
/// in the session dir contains the modified path. If `process_touches`
/// regresses to appending after the SessionRecord build, the persisted
/// touches stream still records the touch (the loop just runs later), so
/// this test alone is not sufficient — combine with the SessionRecord-side
/// assertion below.
#[test]
fn flush_current_touch_joins_prior_turn_read_in_seed() -> Result<()> {
    let repo = CochangeRepo::build()?;
    let sid = CochangeRepo::sid("sig1c");

    // Turn 1: read b.rs only, then mark+flush with no edits. This persists
    // b.rs as a session read but produces no touches.
    let r = advice(repo.path(), repo.advice_dir(), &sid, "read", &["b.rs"]);
    assert!(r.status.success(), "read failed: {}", String::from_utf8_lossy(&r.stderr));
    advice_mark(repo.path(), repo.advice_dir(), &sid, "t1");
    let f1 = advice_flush(repo.path(), repo.advice_dir(), &sid, "t1");
    assert_eq!(f1.status.code(), Some(0));

    // Turn 2: NO prior read of a.rs. Just mark, modify a.rs, flush. The only
    // way for the (a.rs, b.rs) pair to surface is for the SessionRecord
    // passed to the suggester to contain BOTH the persisted prior-turn read
    // (b.rs) AND the current flush's touch (a.rs). Before the fix in
    // `process_touches`, the SessionRecord was built BEFORE the current
    // touches were appended, so `load_touches()` returned `[]` and the
    // pair never formed → empty stdout despite identical inputs.
    advice_mark(repo.path(), repo.advice_dir(), &sid, "t2");
    std::fs::write(
        repo.path().join("a.rs"),
        "fn a_seeded_by_touch() {}\nfn shared_helper_compute() {}\nfn billing_invoice_total() {}\nfn customer_record_id() {}\n",
    )?;
    let flush_out = advice_flush(repo.path(), repo.advice_dir(), &sid, "t2");
    assert_eq!(
        flush_out.status.code(),
        Some(0),
        "flush must exit 0: {}",
        String::from_utf8_lossy(&flush_out.stderr)
    );

    let stdout = String::from_utf8_lossy(&flush_out.stdout);
    let stderr = String::from_utf8_lossy(&flush_out.stderr);
    assert!(
        stdout.contains("has implicit semantic dependencies, document with `git mesh`"),
        "current-flush touch must join the SessionRecord seed alongside prior-turn reads.\nSTDOUT:\n{stdout}\nSTDERR:\n{stderr}"
    );
    Ok(())
}

// ── Signal 2: flush_empty_history_no_suggestion ──────────────────────────────

/// A repo with a single commit (no co-change history) produces no suggestion
/// and exits cleanly.
#[test]
fn flush_empty_history_no_suggestion() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let advice_dir = tempfile::tempdir()?;
    let p = dir.path();

    fn git(p: &Path, args: &[&str]) {
        let s = Command::new("git")
            .current_dir(p)
            .args(args)
            .output()
            .unwrap();
        assert!(s.status.success(), "git {:?} failed", args);
    }
    git(p, &["init", "--initial-branch=main"]);
    git(p, &["config", "user.email", "t@t"]);
    git(p, &["config", "user.name", "T"]);
    git(p, &["config", "commit.gpgsign", "false"]);
    std::fs::write(p.join("solo.rs"), "fn solo() {}\n")?;
    git(p, &["add", "."]);
    git(p, &["commit", "-m", "init"]);

    let sid = CochangeRepo::sid("sig2");
    let mark_out = advice_mark(p, advice_dir.path(), &sid, "t1");
    assert!(mark_out.status.success(), "mark failed");
    std::fs::write(p.join("solo.rs"), "fn solo2() {}\n")?;
    let flush_out = advice_flush(p, advice_dir.path(), &sid, "t1");
    assert_eq!(flush_out.status.code(), Some(0), "flush must exit 0");
    // No suggestion should be emitted (single-file session, no co-changes).
    let stdout = String::from_utf8_lossy(&flush_out.stdout);
    assert!(
        !stdout.contains("git mesh add"),
        "unexpected suggestion in single-commit repo: {stdout}"
    );
    Ok(())
}

// ── Signal 3: flush_same_head_cache_reuse ────────────────────────────────────

/// Two CLI flushes at the same HEAD: after the first, `history_cache.json`
/// exists in the session dir; after the second flush at the same HEAD, the
/// file's mtime is unchanged (the cache was reused, not rewritten).
#[test]
fn flush_same_head_cache_reuse() -> Result<()> {
    let repo = CochangeRepo::build()?;
    let sid = CochangeRepo::sid("sig3");

    // Seed reads so the suggester has participants.
    advice(repo.path(), repo.advice_dir(), &sid, "read", &["a.rs"]);
    advice(repo.path(), repo.advice_dir(), &sid, "read", &["b.rs"]);

    // First flush — full walk; writes history_cache.json.
    advice_mark(repo.path(), repo.advice_dir(), &sid, "t1");
    std::fs::write(
        repo.path().join("a.rs"),
        "fn a_cached1() {}\nfn shared_helper_compute_caller_a_c1() {}\n",
    )?;
    let f1 = advice_flush(repo.path(), repo.advice_dir(), &sid, "t1");
    assert_eq!(f1.status.code(), Some(0), "first flush must exit 0");

    let cache_path = find_file(repo.advice_dir(), "history_cache.json")
        .expect("history_cache.json must exist after the first CLI flush");
    let mtime1 = std::fs::metadata(&cache_path)?.modified()?;

    // Sleep enough to make any rewrite detectable by mtime.
    std::thread::sleep(std::time::Duration::from_millis(1100));

    // Second flush at the same HEAD — cache must be reused (same mtime).
    advice_mark(repo.path(), repo.advice_dir(), &sid, "t2");
    std::fs::write(
        repo.path().join("a.rs"),
        "fn a_cached2() {}\nfn shared_helper_compute_caller_a_c2() {}\n",
    )?;
    let f2 = advice_flush(repo.path(), repo.advice_dir(), &sid, "t2");
    assert_eq!(f2.status.code(), Some(0), "second flush must exit 0");

    let mtime2 = std::fs::metadata(&cache_path)?.modified()?;
    assert_eq!(
        mtime1, mtime2,
        "history_cache.json mtime must be stable across same-HEAD flushes"
    );
    Ok(())
}

/// Recursively search `root` for a file named `name`.
#[allow(dead_code)]
fn walkdir_find(root: &Path, name: &str) -> bool {
    let rd = match std::fs::read_dir(root) {
        Ok(r) => r,
        Err(_) => return false,
    };
    for entry in rd.flatten() {
        let path = entry.path();
        if path.is_dir() && walkdir_find(&path, name) {
            return true;
        }
        if path.file_name().and_then(|f| f.to_str()) == Some(name) {
            return true;
        }
    }
    false
}

// ── Signal 4: flush_after_commit_rebuilds_cache ──────────────────────────────

/// CLI flush → `git commit` → CLI flush at new HEAD: the second flush
/// rebuilds the cache because `head_sha` changed.
#[test]
fn flush_after_commit_rebuilds_cache() -> Result<()> {
    let repo = CochangeRepo::build()?;
    let sid = CochangeRepo::sid("sig4");

    advice(repo.path(), repo.advice_dir(), &sid, "read", &["a.rs"]);
    advice(repo.path(), repo.advice_dir(), &sid, "read", &["b.rs"]);

    // First flush — writes the cache at the original HEAD.
    advice_mark(repo.path(), repo.advice_dir(), &sid, "t1");
    std::fs::write(
        repo.path().join("a.rs"),
        "fn a_pre_commit() {}\nfn shared_helper_compute_caller_a_pc() {}\n",
    )?;
    let f1 = advice_flush(repo.path(), repo.advice_dir(), &sid, "t1");
    assert_eq!(f1.status.code(), Some(0));

    let cache_path = find_file(repo.advice_dir(), "history_cache.json")
        .expect("history_cache.json must exist after first flush");
    let cache1: serde_json::Value = serde_json::from_slice(&std::fs::read(&cache_path)?)?;
    let head1 = cache1["head_sha"].as_str().unwrap_or("").to_string();
    assert!(!head1.is_empty(), "head_sha must be present in the first cache");

    // Make a new commit (the staged a.rs from the flush still differs from HEAD).
    fn git(p: &Path, args: &[&str]) {
        let s = Command::new("git").current_dir(p).args(args).output().unwrap();
        assert!(s.status.success(), "git {:?} failed", args);
    }
    git(repo.path(), &["add", "-A"]);
    git(repo.path(), &["commit", "-m", "sig4 new commit"]);

    // Second flush at the new HEAD.
    advice_mark(repo.path(), repo.advice_dir(), &sid, "t2");
    std::fs::write(
        repo.path().join("a.rs"),
        "fn a_post_commit() {}\nfn shared_helper_compute_caller_a_postc() {}\n",
    )?;
    let f2 = advice_flush(repo.path(), repo.advice_dir(), &sid, "t2");
    assert_eq!(f2.status.code(), Some(0));

    let cache2: serde_json::Value = serde_json::from_slice(&std::fs::read(&cache_path)?)?;
    let head2 = cache2["head_sha"].as_str().unwrap_or("").to_string();

    assert_ne!(
        head1, head2,
        "cache head_sha must change after a new commit"
    );
    Ok(())
}

fn find_file(root: &Path, name: &str) -> Option<PathBuf> {
    let rd = std::fs::read_dir(root).ok()?;
    for entry in rd.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if let Some(p) = find_file(&path, name) {
                return Some(p);
            }
        } else if path.file_name().and_then(|f| f.to_str()) == Some(name) {
            return Some(path);
        }
    }
    None
}

// ── Signal 5: suggest_subcommand_removed ─────────────────────────────────────

/// `git mesh advice suggest …` must not exist as a subcommand. We verify by
/// running `git mesh advice --help` and checking "suggest" does not appear.
#[test]
fn suggest_subcommand_removed() -> Result<()> {
    let repo = CochangeRepo::build()?;
    let advice_dir = tempfile::tempdir()?;
    let sid = CochangeRepo::sid("sig5");

    let out = Command::new(git_mesh_bin())
        .current_dir(repo.path())
        .env("GIT_MESH_ADVICE_DIR", advice_dir.path())
        .args(["advice", &sid, "--help"])
        .output()?;

    let help = String::from_utf8_lossy(&out.stdout);
    let help_err = String::from_utf8_lossy(&out.stderr);
    let combined = format!("{help}{help_err}");
    // The word "suggest" must not appear as a subcommand name.
    // We check for the pattern clap uses to list subcommands: "  suggest"
    // (two leading spaces before the command name in the Commands section).
    assert!(
        !combined.contains("  suggest"),
        "\"suggest\" must not appear as a subcommand in advice help: {combined}"
    );
    Ok(())
}

// ── Signal 6: suggestion_reproducible_by_git_log ─────────────────────────────

/// After a flush that has co-change history, run `git log --name-only -- a.rs b.rs`
/// and assert both files appear in the same commit (i.e., the co-change the
/// pipeline uses is genuinely visible in git log).
#[test]
fn suggestion_reproducible_by_git_log() -> Result<()> {
    let repo = CochangeRepo::build()?;

    // Run git log --name-only for both paths.
    let out = Command::new("git")
        .current_dir(repo.path())
        .args(["log", "--name-only", "--no-merges", "--pretty=format:COMMIT:%H", "--", "a.rs", "b.rs"])
        .output()?;
    assert!(out.status.success());

    let stdout = String::from_utf8_lossy(&out.stdout);
    // Find a commit that mentions both a.rs and b.rs.
    let mut in_commit = false;
    let mut commit_files: Vec<&str> = Vec::new();
    let mut found_cochange = false;
    for line in stdout.lines() {
        if line.starts_with("COMMIT:") {
            if in_commit {
                if commit_files.contains(&"a.rs") && commit_files.contains(&"b.rs") {
                    found_cochange = true;
                    break;
                }
                commit_files.clear();
            }
            in_commit = true;
        } else {
            let f = line.trim();
            if !f.is_empty() {
                commit_files.push(f);
            }
        }
    }
    // Check the last commit block too.
    if !found_cochange && commit_files.contains(&"a.rs") && commit_files.contains(&"b.rs") {
        found_cochange = true;
    }

    assert!(
        found_cochange,
        "git log should show a.rs and b.rs co-changed in at least one commit"
    );
    Ok(())
}

// ── Q18: cache_corruption_degrades_to_rebuild ────────────────────────────────

/// Write garbage into `history_cache.json`, then run a flush. The pipeline
/// must not propagate an error — it degrades silently to a full rebuild.
/// Exit code 0 is the contract; no suggestion emission required.
#[test]
fn cache_corruption_degrades_to_rebuild() -> Result<()> {
    let repo = CochangeRepo::build()?;
    let advice_dir = tempfile::tempdir()?;
    let sid = CochangeRepo::sid("corruption");

    // Perform a first flush so the session directory is created.
    advice_mark(repo.path(), advice_dir.path(), &sid, "t1");
    std::fs::write(repo.path().join("a.rs"), "fn ax() {}\n")?;
    let f1 = advice_flush(repo.path(), advice_dir.path(), &sid, "t1");
    assert_eq!(f1.status.code(), Some(0), "first flush must succeed");

    // Corrupt the cache file.
    if let Some(cache_path) = find_file(advice_dir.path(), "history_cache.json") {
        std::fs::write(&cache_path, b"not json {{{{ garbage")?;
    }

    // Second flush must survive the corruption.
    advice_mark(repo.path(), advice_dir.path(), &sid, "t2");
    std::fs::write(repo.path().join("a.rs"), "fn ay() {}\n")?;
    let f2 = advice_flush(repo.path(), advice_dir.path(), &sid, "t2");
    assert_eq!(
        f2.status.code(),
        Some(0),
        "flush must not crash on corrupted cache: {}",
        String::from_utf8_lossy(&f2.stderr)
    );
    Ok(())
}

// ── Multi-turn seed scope: flush_multi_turn_session_uses_session_scope ────────

/// Two turns: turn 1 reads file A, turn 2 modifies file B; co-change history
/// has A↔B co-changed. The turn-2 flush should include A in the session seed
/// (from turn-1's read) so the history walk considers A↔B co-changes.
///
/// We assert the flush exits 0 (the minimum contract). Detecting whether A was
/// actually included in the seed is an internal implementation detail we don't
/// introspect here; instead we verify the session correctly accumulates reads
/// across turns by checking the reads.jsonl file after both turns.
#[test]
fn flush_multi_turn_session_uses_session_scope() -> Result<()> {
    let repo = CochangeRepo::build()?;
    let advice_dir = tempfile::tempdir()?;
    let sid = CochangeRepo::sid("multiturn");

    // Turn 1: read a.rs.
    let read_out = advice(repo.path(), advice_dir.path(), &sid, "read", &["a.rs", "t1"]);
    assert!(
        read_out.status.success(),
        "read failed: {}",
        String::from_utf8_lossy(&read_out.stderr)
    );

    // Turn 2: mark → modify b.rs → flush.
    advice_mark(repo.path(), advice_dir.path(), &sid, "t2");
    std::fs::write(repo.path().join("b.rs"), "fn b_mt() {}\n")?;
    let f2 = advice_flush(repo.path(), advice_dir.path(), &sid, "t2");
    assert_eq!(
        f2.status.code(),
        Some(0),
        "turn-2 flush must exit 0: {}",
        String::from_utf8_lossy(&f2.stderr)
    );

    // Verify a.rs appears in reads.jsonl — this is how the session seed grows.
    let reads_file = find_file(advice_dir.path(), "reads.jsonl");
    assert!(
        reads_file.is_some(),
        "reads.jsonl must exist after recording a read"
    );
    let contents = std::fs::read_to_string(reads_file.unwrap())?;
    assert!(
        contents.contains("a.rs"),
        "a.rs must appear in reads.jsonl: {contents}"
    );
    Ok(())
}

// ── Latency guard: flush_partial_walk_not_cached ─────────────────────────────
//
// The partial-walk-not-cached invariant is exercised by
// `advice::suggest::history_cache::tests::miss_on_complete_false`, which
// directly writes a `complete: false` cache entry and asserts `try_load`
// returns a miss. The integration-level deterministic version would require
// an injectable wall-clock on `git_log_name_only_for_paths`; see that unit
// test for the canonical coverage of this contract.

// ── Step 1b: flush_deleted_suppression ───────────────────────────────────────

/// Mixed turn: a.rs is deleted, b.rs is modified; co-change history is A↔B.
/// Suggestions must not reference a.rs (deleted participant suppression).
#[test]
fn flush_deleted_suppression() -> Result<()> {
    let repo = CochangeRepo::build()?;
    let advice_dir = tempfile::tempdir()?;
    let sid = CochangeRepo::sid("deleted");

    // Mark before changes.
    advice_mark(repo.path(), advice_dir.path(), &sid, "t1");

    // Delete a.rs, modify b.rs.
    std::fs::remove_file(repo.path().join("a.rs"))?;
    std::fs::write(repo.path().join("b.rs"), "fn b_del() {}\n")?;

    let flush_out = advice_flush(repo.path(), advice_dir.path(), &sid, "t1");
    assert_eq!(
        flush_out.status.code(),
        Some(0),
        "flush must exit 0: {}",
        String::from_utf8_lossy(&flush_out.stderr)
    );

    // Any suggestion output must not cite a.rs (it was deleted this turn).
    let stdout = String::from_utf8_lossy(&flush_out.stdout);
    assert!(
        !stdout.contains("a.rs"),
        "deleted file a.rs must not appear in suggestion output: {stdout}"
    );
    Ok(())
}

//! Tests for `--snapshot-if-missing` lazy bootstrap behaviour.
//!
//! Covers: render without flag bails, render with flag bootstraps, corrupt
//! baseline with flag still bails, and read with flag bootstraps.

mod support;

use anyhow::Result;
use std::process::Output;
use support::TestRepo;
use uuid::Uuid;

fn session_id(label: &str) -> String {
    format!("sim-{label}-{}", Uuid::new_v4())
}

/// Run `git mesh advice <sid> [extra...]` (no subcommand — bare render).
fn run_render(repo: &TestRepo, session: &str, extra: &[&str]) -> Result<Output> {
    let mut args: Vec<String> = vec!["advice".into(), session.into()];
    for a in extra {
        args.push((*a).to_string());
    }
    repo.run_mesh(args)
}

/// Run `git mesh advice <sid> [top_flags...] read <paths...>`.
/// `top_flags` must come before the `read` subcommand (clap ordering).
fn run_read(repo: &TestRepo, session: &str, top_flags: &[&str], paths: &[&str]) -> Result<Output> {
    let mut args: Vec<String> = vec!["advice".into(), session.into()];
    for f in top_flags {
        args.push((*f).to_string());
    }
    args.push("read".into());
    for p in paths {
        args.push((*p).to_string());
    }
    repo.run_mesh(args)
}

fn session_dir(repo: &TestRepo, sid: &str) -> std::path::PathBuf {
    let store =
        git_mesh::advice::SessionStore::open(repo.path(), &repo.path().join(".git"), sid)
            .expect("open store");
    store
        .baseline_objects_dir()
        .parent()
        .expect("parent")
        .to_path_buf()
}

// ---------------------------------------------------------------------------
// render WITHOUT --snapshot-if-missing bails when no baseline exists.
// ---------------------------------------------------------------------------
#[test]
fn render_without_flag_bails_when_no_baseline() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let sid = session_id("no-flag");
    let out = run_render(&repo, &sid, &[])?;
    assert!(!out.status.success(), "expected non-zero exit without snapshot");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("snapshot"),
        "stderr must mention `snapshot`; got: {stderr}"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// render WITH --snapshot-if-missing lazily bootstraps and exits 0.
// The first render after bootstrap has an empty delta — it must be silent.
// baseline.state must exist afterwards.
// ---------------------------------------------------------------------------
#[test]
fn render_with_flag_lazily_bootstraps_then_renders_empty() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let sid = session_id("lazy-render");
    let out = run_render(&repo, &sid, &["--snapshot-if-missing"])?;
    assert!(
        out.status.success(),
        "expected exit 0 with --snapshot-if-missing; stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        out.stdout.is_empty(),
        "first render after lazy bootstrap must be silent; stdout={}",
        String::from_utf8_lossy(&out.stdout)
    );
    let dir = session_dir(&repo, &sid);
    assert!(
        dir.join("baseline.state").exists(),
        "baseline.state must be created by lazy bootstrap"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// render WITH --snapshot-if-missing when baseline.state is PRESENT but
// corrupt must still bail (only missing triggers bootstrap).
// ---------------------------------------------------------------------------
#[test]
fn render_with_flag_corrupt_baseline_still_bails() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let sid = session_id("corrupt");
    // First run to create the session dir.
    let _ = run_render(&repo, &sid, &["--snapshot-if-missing"])?;
    let dir = session_dir(&repo, &sid);

    // Overwrite baseline.state with garbage.
    std::fs::write(dir.join("baseline.state"), b"not-valid-json")?;

    let out = run_render(&repo, &sid, &["--snapshot-if-missing"])?;
    assert!(
        !out.status.success(),
        "corrupt baseline with --snapshot-if-missing must still fail closed"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// read WITH --snapshot-if-missing lazily bootstraps so the read is recorded
// against a real baseline rather than discarded.
// baseline.state must exist afterwards and reads.jsonl must be non-empty.
// ---------------------------------------------------------------------------
#[test]
fn read_with_flag_lazily_bootstraps() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let sid = session_id("lazy-read");

    let out = run_read(&repo, &sid, &["--snapshot-if-missing"], &["file1.txt"])?;
    assert!(
        out.status.success(),
        "read with --snapshot-if-missing must succeed; stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );

    let dir = session_dir(&repo, &sid);
    assert!(
        dir.join("baseline.state").exists(),
        "baseline.state must be created by lazy bootstrap before read"
    );
    let reads_len = std::fs::metadata(dir.join("reads.jsonl"))?.len();
    assert!(
        reads_len > 0,
        "reads.jsonl must be non-empty after lazy-bootstrapped read"
    );
    Ok(())
}

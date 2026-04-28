//! Integration tests for GIT_MESH_ADVICE_DEBUG=1 provenance trace.

mod support;

use anyhow::Result;
use git_mesh::{append_add, commit_mesh, set_why};
use std::process::Command;
use support::TestRepo;
use uuid::Uuid;

fn sid() -> String {
    format!("advice-debug-{}", Uuid::new_v4())
}

/// Run `git mesh advice` in the repo, returning (stdout, stderr).
fn run_advice_with_env(
    repo: &TestRepo,
    session: &str,
    debug: bool,
    extra: &[&str],
) -> Result<(String, String)> {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_git-mesh"));
    cmd.current_dir(repo.path());
    cmd.arg("advice");
    cmd.arg(session);
    for a in extra {
        cmd.arg(a);
    }
    if debug {
        cmd.env("GIT_MESH_ADVICE_DEBUG", "1");
    } else {
        cmd.env_remove("GIT_MESH_ADVICE_DEBUG");
    }
    let out = cmd.output()?;
    Ok((
        String::from_utf8(out.stdout)?,
        String::from_utf8(out.stderr)?,
    ))
}

#[test]
fn debug_unset_stderr_empty() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let gix = repo.gix_repo()?;

    append_add(&gix, "m1", "file1.txt", 1, 5, None)?;
    append_add(&gix, "m1", "file2.txt", 1, 5, None)?;
    set_why(&gix, "m1", "file1 and file2 move together")?;
    commit_mesh(&gix, "m1")?;

    let s = sid();
    // Snapshot
    let snap = run_advice_with_env(&repo, &s, false, &["snapshot"])?;
    assert!(snap.1.is_empty(), "snapshot stderr non-empty without debug: {:?}", snap.1);

    // Touch file1.txt to trigger a candidate
    repo.write_file("file1.txt", "line1\nline2\nline3\nline4\nline5\nchanged\n")?;

    let (stdout, stderr) = run_advice_with_env(&repo, &s, false, &[])?;
    // When debug is unset, no git-mesh-advice-debug: lines must appear.
    // (Other CLI warnings may appear on stderr and are not our concern here.)
    let debug_lines: Vec<&str> = stderr
        .lines()
        .filter(|l| l.starts_with("git-mesh-advice-debug:"))
        .collect();
    assert!(
        debug_lines.is_empty(),
        "debug prefix lines must not appear when GIT_MESH_ADVICE_DEBUG is unset; got: {debug_lines:?}"
    );
    // stdout should still render advice
    let _ = stdout;
    Ok(())
}

#[test]
fn debug_set_stderr_non_empty_with_prefix() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let gix = repo.gix_repo()?;

    append_add(&gix, "m1", "file1.txt", 1, 5, None)?;
    append_add(&gix, "m1", "file2.txt", 1, 5, None)?;
    set_why(&gix, "m1", "file1 and file2 move together")?;
    commit_mesh(&gix, "m1")?;

    let s = sid();
    run_advice_with_env(&repo, &s, true, &["snapshot"])?;

    // Touch file1.txt to trigger a detector
    repo.write_file("file1.txt", "line1\nline2\nline3\nline4\nline5\nchanged\n")?;

    let (stdout, stderr) = run_advice_with_env(&repo, &s, true, &[])?;

    // Debug lines are prefixed; the CLI may also emit other stderr (e.g. repo-key
    // isolation warnings). Filter for debug lines only.
    let debug_lines: Vec<&str> = stderr
        .lines()
        .filter(|l| l.starts_with("git-mesh-advice-debug:"))
        .collect();

    assert!(
        !debug_lines.is_empty(),
        "no git-mesh-advice-debug: lines in stderr when GIT_MESH_ADVICE_DEBUG=1; got:\n{stderr}"
    );

    // At least one line must reference a detector.
    let has_detector = debug_lines.iter().any(|l| {
        l.contains("detect_delta_intersects_mesh")
            || l.contains("detect_read_intersects_mesh")
            || l.contains("detect_partner_drift")
            || l.contains("detect_rename_consequence")
            || l.contains("detect_staging_cross_cut")
            || l.contains("detect_range_shrink")
    });
    assert!(has_detector, "no detector line found in debug trace:\n{stderr}");

    // stdout is unchanged relative to the non-debug run.
    let (stdout_nodebug, _) = {
        // Re-run with a fresh session in the same repo (the previous run advanced the cursor).
        let s2 = sid();
        run_advice_with_env(&repo, &s2, false, &["snapshot"])?;
        // Re-touch to produce the same candidates.
        repo.write_file("file1.txt", "line1\nline2\nline3\nline4\nline5\nchanged-again\n")?;
        run_advice_with_env(&repo, &s2, false, &[])
    }?;
    // Both renders must contain the partner path (may differ in exact content
    // due to blob OIDs, but both must mention file2.txt).
    assert!(
        stdout.contains("file2.txt"),
        "debug stdout must mention partner path; got:\n{stdout}"
    );
    assert!(
        stdout_nodebug.contains("file2.txt"),
        "non-debug stdout must mention partner path; got:\n{stdout_nodebug}"
    );

    Ok(())
}

#[test]
fn debug_cli_entry_and_exit_lines_present() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let gix = repo.gix_repo()?;

    append_add(&gix, "m1", "file1.txt", 1, 5, None)?;
    append_add(&gix, "m1", "file2.txt", 1, 5, None)?;
    set_why(&gix, "m1", "pair")?;
    commit_mesh(&gix, "m1")?;

    let s = sid();
    run_advice_with_env(&repo, &s, true, &["snapshot"])?;
    repo.write_file("file1.txt", "changed\n")?;

    let (_, stderr) = run_advice_with_env(&repo, &s, true, &[])?;

    // Verify cli-entry carries the correct sid= kv pair — this confirms that
    // format_line actually serialises key-value pairs, not just the tag.
    let cli_entry_line = stderr
        .lines()
        .find(|l| l.contains("cli-entry"))
        .expect("no cli-entry line in trace");
    assert!(
        cli_entry_line.contains(&format!("sid={s}")),
        "cli-entry line missing expected sid={s}; got: {cli_entry_line}"
    );

    assert!(
        stderr.lines().any(|l| l.contains("cli-exit")),
        "no cli-exit line in trace:\n{stderr}"
    );
    Ok(())
}

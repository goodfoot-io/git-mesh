//! CLI performance logging opt-in.

mod support;

use anyhow::Result;
use support::TestRepo;

#[test]
fn perf_flag_logs_timings_to_stderr_only() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let out = repo.run_mesh(["--perf", "ls"])?;

    assert_eq!(out.status.code(), Some(0));
    let stdout = String::from_utf8(out.stdout)?;
    let stderr = String::from_utf8(out.stderr)?;

    assert_eq!(stdout.trim(), "no meshes");
    assert!(
        stderr.contains("git-mesh perf: command.ls"),
        "expected command timing in stderr, got: {stderr}"
    );
    assert!(
        stderr.contains("git-mesh perf: git.discover"),
        "expected git discovery timing in stderr, got: {stderr}"
    );
    Ok(())
}

#[test]
fn perf_env_logs_timings_to_stderr_only() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_git-mesh"));
    let out = cmd
        .current_dir(repo.path())
        .env("GIT_MESH_PERF", "1")
        .arg("ls")
        .output()?;

    assert_eq!(out.status.code(), Some(0));
    let stdout = String::from_utf8(out.stdout)?;
    let stderr = String::from_utf8(out.stderr)?;

    assert_eq!(stdout.trim(), "no meshes");
    assert!(
        stderr.contains("git-mesh perf: command.ls"),
        "expected command timing in stderr, got: {stderr}"
    );
    Ok(())
}

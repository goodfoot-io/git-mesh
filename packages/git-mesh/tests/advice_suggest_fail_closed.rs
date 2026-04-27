//! Fail-closed tests for `git mesh advice suggest`.
//!
//! Verifies that the binary exits non-zero with a useful error message when
//! `GIT_MESH_ADVICE_DIR` is unset or points at an empty directory. Also covers
//! the fixture-mode stderr notice emitted when `GIT_MESH_SUGGEST_FIXTURE=1`.

use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git-mesh");

/// Run `git mesh advice suggest` with the given environment, stripping the
/// inherited `GIT_MESH_ADVICE_DIR` from the parent process.
fn run_suggest_with_env(extra: &[(&str, &str)]) -> std::process::Output {
    let mut cmd = Command::new(BIN);
    // Remove any GIT_MESH_ADVICE_DIR the test runner might have inherited.
    cmd.env_remove("GIT_MESH_ADVICE_DIR")
        .env("HOME", "/tmp")
        .args(["advice", "suggest"]);
    for (k, v) in extra {
        cmd.env(k, v);
    }
    cmd.output().expect("spawn git-mesh")
}

#[test]
fn suggest_fails_when_advice_dir_unset() {
    let out = run_suggest_with_env(&[]);
    assert!(
        !out.status.success(),
        "expected non-zero exit when GIT_MESH_ADVICE_DIR is unset, got success"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("GIT_MESH_ADVICE_DIR"),
        "stderr must mention GIT_MESH_ADVICE_DIR; got: {stderr:?}"
    );
}

#[test]
fn suggest_fails_when_advice_dir_points_at_missing_path() {
    let out = run_suggest_with_env(&[("GIT_MESH_ADVICE_DIR", "/tmp/nonexistent-git-mesh-test-dir")]);
    assert!(
        !out.status.success(),
        "expected non-zero exit when GIT_MESH_ADVICE_DIR points at missing dir"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("nonexistent-git-mesh-test-dir") || stderr.contains("does not exist"),
        "stderr must name the missing path; got: {stderr:?}"
    );
}

#[test]
fn suggest_fails_when_advice_dir_is_empty() {
    let dir = tempfile::tempdir().expect("tempdir");
    let out = run_suggest_with_env(&[("GIT_MESH_ADVICE_DIR", dir.path().to_str().unwrap())]);
    assert!(
        !out.status.success(),
        "expected non-zero exit when GIT_MESH_ADVICE_DIR has no sessions"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("no sessions") || stderr.contains("reads.jsonl") || stderr.contains("touches.jsonl"),
        "stderr must mention missing sessions; got: {stderr:?}"
    );
}

#[test]
fn suggest_stderr_contains_fixture_mode_notice_when_fixture_env_set() {
    // Create a directory with at least one session so the command does not
    // fail on "no sessions found" before reaching the fixture-mode code path.
    let dir = tempfile::tempdir().expect("tempdir");
    let session_dir = dir.path().join("session-001");
    std::fs::create_dir_all(&session_dir).expect("mkdir");
    std::fs::write(
        session_dir.join("reads.jsonl"),
        "{\"path\":\"a.rs\",\"ts\":\"2024-01-01T00:00:00Z\"}\n",
    )
    .expect("write reads.jsonl");

    let out = run_suggest_with_env(&[
        ("GIT_MESH_ADVICE_DIR", dir.path().to_str().unwrap()),
        ("GIT_MESH_SUGGEST_FIXTURE", "1"),
    ]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("fixture mode") && stderr.contains("GIT_MESH_SUGGEST_FIXTURE"),
        "stderr must contain fixture-mode notice when GIT_MESH_SUGGEST_FIXTURE=1; got: {stderr:?}"
    );
}

#[test]
fn suggest_stderr_has_no_fixture_mode_notice_in_normal_mode() {
    // Point at a missing dir (will fail closed) but NOT in fixture mode.
    let out = run_suggest_with_env(&[("GIT_MESH_ADVICE_DIR", "/tmp/nonexistent-git-mesh-test-dir")]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("fixture mode"),
        "stderr must NOT contain fixture-mode notice in normal mode; got: {stderr:?}"
    );
}

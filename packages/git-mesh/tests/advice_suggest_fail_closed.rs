//! Fail-closed tests for `git mesh advice suggest`.
//!
//! Verifies that the binary exits non-zero with a useful error message when
//! `GIT_MESH_ADVICE_DIR` is unset or points at an empty directory. (Finding #3.)

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

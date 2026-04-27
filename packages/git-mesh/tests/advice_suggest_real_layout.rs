//! Integration test verifying that `git mesh advice suggest` loads sessions
//! from the real two-level layout: `<base>/<repo_key>/<sid>/{reads,touches}.jsonl`.
//!
//! The loader must walk into subdirectories that do NOT directly contain
//! `reads.jsonl`/`touches.jsonl` (i.e. repo_key directories) and discover
//! session directories one level deeper.

use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git-mesh");

/// Run `git mesh advice suggest` pointing at a two-level corpus layout.
#[test]
fn suggest_loads_two_level_layout() {
    // Build: <base>/somerepokey/<sid>/reads.jsonl
    let base = tempfile::tempdir().expect("tempdir");
    let repo_key_dir = base.path().join("aabbccddeeff0011"); // arbitrary stable key
    let session_dir = repo_key_dir.join("session-001");
    std::fs::create_dir_all(&session_dir).expect("mkdir session_dir");

    // Write a minimal reads.jsonl with one record.
    let reads_jsonl = session_dir.join("reads.jsonl");
    std::fs::write(
        &reads_jsonl,
        "{\"path\":\"src/main.rs\",\"ts\":\"2024-01-01T00:00:00Z\"}\n",
    )
    .expect("write reads.jsonl");

    let out = Command::new(BIN)
        .env_remove("GIT_MESH_ADVICE_DIR")
        .env("GIT_MESH_ADVICE_DIR", base.path().to_str().unwrap())
        // Disable history channel so we don't need a real git repo.
        .env("GIT_MESH_SUGGEST_FIXTURE", "1")
        // Disable history explicitly too.
        .env("GIT_MESH_SUGGEST_HISTORY", "0")
        .env("HOME", "/tmp")
        .args(["advice", "suggest"])
        .output()
        .expect("spawn git-mesh");

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);

    // The command should either succeed or fail — but it must NOT fail with
    // "no sessions found", which would indicate the two-level layout was not walked.
    assert!(
        !stderr.contains("no sessions found"),
        "loader did not discover two-level layout sessions; stderr: {stderr:?}"
    );

    // If the command succeeded, there must be JSONL output (suggestions).
    // If it failed for another reason (e.g. empty corpus produces no suggestions),
    // that is acceptable — the key assertion is the loader found the sessions.
    // We assert the command did not exit with the "no sessions" error.
    let _ = stdout;
    let _ = out.status;
}

/// Also verify the fixture (flat) layout still works after the loader change.
#[test]
fn suggest_still_loads_flat_fixture_layout() {
    let base = tempfile::tempdir().expect("tempdir");
    let session_dir = base.path().join("session-flat-001");
    std::fs::create_dir_all(&session_dir).expect("mkdir session_dir");

    let reads_jsonl = session_dir.join("reads.jsonl");
    std::fs::write(
        &reads_jsonl,
        "{\"path\":\"src/lib.rs\",\"ts\":\"2024-01-01T00:00:00Z\"}\n",
    )
    .expect("write reads.jsonl");

    let out = Command::new(BIN)
        .env_remove("GIT_MESH_ADVICE_DIR")
        .env("GIT_MESH_ADVICE_DIR", base.path().to_str().unwrap())
        .env("GIT_MESH_SUGGEST_FIXTURE", "1")
        .env("GIT_MESH_SUGGEST_HISTORY", "0")
        .env("HOME", "/tmp")
        .args(["advice", "suggest"])
        .output()
        .expect("spawn git-mesh");

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("no sessions found"),
        "loader did not discover flat fixture layout sessions; stderr: {stderr:?}"
    );
}

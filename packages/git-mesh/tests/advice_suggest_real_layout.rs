//! Integration test verifying that `git mesh advice suggest` loads sessions
//! from the real two-level layout: `<base>/<repo_key>/<sid>/{reads,touches}.jsonl`.
//!
//! The loader must walk into subdirectories that do NOT directly contain
//! `reads.jsonl`/`touches.jsonl` (i.e. repo_key directories) and discover
//! session directories one level deeper.
//!
//! Also covers finding 1 (cross-repo isolation) and finding 2 (ambiguous flat-vs-nested
//! classification).

use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_git-mesh");

/// Helper: write a minimal reads.jsonl with one record naming `path`.
fn write_reads_jsonl(session_dir: &std::path::Path, path: &str) {
    std::fs::create_dir_all(session_dir).expect("mkdir session_dir");
    std::fs::write(
        session_dir.join("reads.jsonl"),
        format!("{{\"path\":\"{path}\",\"ts\":\"2024-01-01T00:00:00Z\"}}\n"),
    )
    .expect("write reads.jsonl");
}

/// Helper: write a minimal touches.jsonl with one record naming `path`.
fn write_touches_jsonl(session_dir: &std::path::Path, path: &str) {
    std::fs::create_dir_all(session_dir).expect("mkdir session_dir");
    std::fs::write(
        session_dir.join("touches.jsonl"),
        format!(
            "{{\"path\":\"{path}\",\"start_line\":1,\"end_line\":5,\
             \"ts\":\"2024-01-01T00:00:00Z\"}}\n"
        ),
    )
    .expect("write touches.jsonl");
}

/// Run `git mesh advice suggest` pointing at a two-level corpus layout.
#[test]
fn suggest_loads_two_level_layout() {
    // Build: <base>/somerepokey/<sid>/reads.jsonl
    let base = tempfile::tempdir().expect("tempdir");
    let repo_key_dir = base.path().join("aabbccddeeff0011"); // arbitrary stable key
    let session_dir = repo_key_dir.join("session-001");
    write_reads_jsonl(&session_dir, "src/main.rs");

    let out = Command::new(BIN)
        .env_remove("GIT_MESH_ADVICE_DIR")
        .env("GIT_MESH_ADVICE_DIR", base.path().to_str().unwrap())
        // Fixture mode: cross-corpus (no repo-key isolation), no history.
        .env("GIT_MESH_SUGGEST_FIXTURE", "1")
        .env("GIT_MESH_SUGGEST_HISTORY", "0")
        .env("HOME", "/tmp")
        .args(["advice", "suggest"])
        .output()
        .expect("spawn git-mesh");

    let stderr = String::from_utf8_lossy(&out.stderr);

    // Must NOT fail with "no sessions found" — two-level layout must be discovered.
    assert!(
        !stderr.contains("no sessions found"),
        "loader did not discover two-level layout sessions; stderr: {stderr:?}"
    );
}

/// Also verify the fixture (flat) layout still works after the loader change.
#[test]
fn suggest_still_loads_flat_fixture_layout() {
    let base = tempfile::tempdir().expect("tempdir");
    let session_dir = base.path().join("session-flat-001");
    write_reads_jsonl(&session_dir, "src/lib.rs");

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

// ---------------------------------------------------------------------------
// Finding 1: cross-repo session isolation
// ---------------------------------------------------------------------------

/// When inside repo-B, sessions from key_A must be excluded even when key_B has sessions.
///
/// This test uses fixture mode (GIT_MESH_SUGGEST_FIXTURE=1) as a proxy for
/// "all sessions eligible" to verify the corpus is NOT polluted by key_A sessions
/// in strict mode. The strict-mode half is verified by
/// `suggest_strict_mode_excludes_foreign_key_sessions`.
#[test]
fn suggest_strict_mode_excludes_foreign_key_sessions() {
    // Build two-level layout with two distinct repo keys.
    let base = tempfile::tempdir().expect("tempdir");

    // key_a: sessions with paths from repo-A
    let key_a = "aaaaaaaaaaaaaaaa";
    write_reads_jsonl(&base.path().join(key_a).join("sa"), "repo_a/src/main.rs");

    // key_b: sessions with paths from repo-B — we want ONLY these.
    let key_b = "bbbbbbbbbbbbbbbb";
    write_reads_jsonl(&base.path().join(key_b).join("sb"), "repo_b/src/lib.rs");

    // Init a real git repo so gix::discover(".") succeeds and yields a preferred_key.
    // We compute that key and rename key_b to match it so the loader picks key_b.
    let repo_tmp = tempfile::tempdir().expect("repo tempdir");
    Command::new("git")
        .args(["init", "--initial-branch=main"])
        .current_dir(repo_tmp.path())
        .output()
        .expect("git init");
    let repo_key = {
        let root = std::fs::canonicalize(repo_tmp.path()).expect("canonicalize repo");
        let git_dir = root.join(".git");
        compute_repo_key(&root, &git_dir)
    };

    // Rename the key_b dir to the real repo key so preferred_key matches.
    let real_key_dir = base.path().join(&repo_key);
    std::fs::rename(base.path().join(key_b), &real_key_dir).expect("rename key_b dir");
    write_reads_jsonl(&real_key_dir.join("sb"), "repo_b/src/lib.rs");

    // Run from within the real repo dir — strict mode (no FIXTURE env).
    let out = Command::new(BIN)
        .current_dir(repo_tmp.path())
        .env_remove("GIT_MESH_SUGGEST_FIXTURE")
        .env_remove("GIT_MESH_ADVICE_DIR")
        .env("GIT_MESH_ADVICE_DIR", base.path().to_str().unwrap())
        .env("GIT_MESH_SUGGEST_HISTORY", "0")
        .env("HOME", "/tmp")
        .args(["advice", "suggest"])
        .output()
        .expect("spawn git-mesh");

    let stderr = String::from_utf8_lossy(&out.stderr);
    let stdout = String::from_utf8_lossy(&out.stdout);

    // Must find the preferred-key sessions (not "no sessions found").
    assert!(
        !stderr.contains("no sessions found"),
        "strict mode: preferred-key sessions not found; stderr: {stderr:?}"
    );
    // Must NOT reference repo_a paths in suggestions.
    assert!(
        !stdout.contains("repo_a"),
        "strict mode: foreign repo_a path leaked into suggestions; stdout: {stdout:?}"
    );
}

/// When inside a repo but the preferred key has NO sessions, return empty (fail-closed).
#[test]
fn suggest_strict_mode_returns_empty_when_no_preferred_key_sessions() {
    let base = tempfile::tempdir().expect("tempdir");

    // Only key_a has sessions — no sessions for our current repo's key.
    write_reads_jsonl(&base.path().join("aaaaaaaaaaaaaaaa").join("sa"), "repo_a/src/main.rs");

    let repo_tmp = tempfile::tempdir().expect("repo tempdir");
    Command::new("git")
        .args(["init", "--initial-branch=main"])
        .current_dir(repo_tmp.path())
        .output()
        .expect("git init");

    let out = Command::new(BIN)
        .current_dir(repo_tmp.path())
        .env_remove("GIT_MESH_SUGGEST_FIXTURE")
        .env_remove("GIT_MESH_ADVICE_DIR")
        .env("GIT_MESH_ADVICE_DIR", base.path().to_str().unwrap())
        .env("GIT_MESH_SUGGEST_HISTORY", "0")
        .env("HOME", "/tmp")
        .args(["advice", "suggest"])
        .output()
        .expect("spawn git-mesh");

    let stderr = String::from_utf8_lossy(&out.stderr);

    // Must fail with "no sessions found" naming the preferred key.
    assert!(
        stderr.contains("no sessions found"),
        "expected fail-closed 'no sessions found' error; stderr: {stderr:?}"
    );
    // The error message should name the preferred key path.
    assert!(
        !out.status.success(),
        "expected non-zero exit code when preferred key has no sessions"
    );
}

/// In fixture mode (GIT_MESH_SUGGEST_FIXTURE=1), all keys are eligible regardless of repo.
#[test]
fn suggest_fixture_mode_loads_all_keys() {
    let base = tempfile::tempdir().expect("tempdir");
    write_reads_jsonl(&base.path().join("aaaaaaaaaaaaaaaa").join("sa"), "repo_a/src/main.rs");
    write_reads_jsonl(&base.path().join("bbbbbbbbbbbbbbbb").join("sb"), "repo_b/src/lib.rs");

    let repo_tmp = tempfile::tempdir().expect("repo tempdir");
    Command::new("git")
        .args(["init", "--initial-branch=main"])
        .current_dir(repo_tmp.path())
        .output()
        .expect("git init");

    let out = Command::new(BIN)
        .current_dir(repo_tmp.path())
        .env("GIT_MESH_SUGGEST_FIXTURE", "1")
        .env_remove("GIT_MESH_ADVICE_DIR")
        .env("GIT_MESH_ADVICE_DIR", base.path().to_str().unwrap())
        .env("GIT_MESH_SUGGEST_HISTORY", "0")
        .env("HOME", "/tmp")
        .args(["advice", "suggest"])
        .output()
        .expect("spawn git-mesh");

    let stderr = String::from_utf8_lossy(&out.stderr);

    // Fixture mode: must find sessions across both keys.
    assert!(
        !stderr.contains("no sessions found"),
        "fixture mode did not find cross-key sessions; stderr: {stderr:?}"
    );
}

// ---------------------------------------------------------------------------
// Finding 2: ambiguous flat-vs-nested classification
// ---------------------------------------------------------------------------

/// A stray reads.jsonl at the key level must not cause real nested sessions to be dropped.
/// A warning must be emitted to stderr, and the nested sessions must be loaded.
#[test]
fn suggest_ambiguous_key_dir_prefers_nested_and_warns() {
    let base = tempfile::tempdir().expect("tempdir");

    let key_dir = base.path().join("myrepokey");
    // Stray file at the key level (finding 2).
    std::fs::create_dir_all(&key_dir).expect("mkdir key_dir");
    std::fs::write(
        key_dir.join("reads.jsonl"),
        "{\"path\":\"stray/path.rs\",\"ts\":\"2024-01-01T00:00:00Z\"}\n",
    )
    .expect("write stray reads.jsonl");

    // Real session under the key (what the user actually cares about).
    write_reads_jsonl(&key_dir.join("sa"), "real/src/lib.rs");
    write_touches_jsonl(&key_dir.join("sa"), "real/src/lib.rs");

    let out = Command::new(BIN)
        .env_remove("GIT_MESH_ADVICE_DIR")
        .env("GIT_MESH_ADVICE_DIR", base.path().to_str().unwrap())
        // Fixture mode so repo-key isolation doesn't hide the keyed sessions.
        .env("GIT_MESH_SUGGEST_FIXTURE", "1")
        .env("GIT_MESH_SUGGEST_HISTORY", "0")
        .env("HOME", "/tmp")
        .args(["advice", "suggest"])
        .output()
        .expect("spawn git-mesh");

    let stderr = String::from_utf8_lossy(&out.stderr);

    // A warning about the ambiguous directory must be emitted.
    assert!(
        stderr.contains("contains both session files") || stderr.contains("ambiguous"),
        "expected ambiguity warning in stderr; stderr: {stderr:?}"
    );

    // The nested sessions must have been loaded (no "no sessions found" failure).
    assert!(
        !stderr.contains("no sessions found"),
        "ambiguous key dir caused real nested sessions to be dropped; stderr: {stderr:?}"
    );
}

/// A pure nested key dir (no stray files) must be classified correctly with no warning.
#[test]
fn suggest_pure_nested_key_dir_no_warning() {
    let base = tempfile::tempdir().expect("tempdir");
    let key_dir = base.path().join("cleankey");
    write_reads_jsonl(&key_dir.join("s1"), "clean/src/main.rs");

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

    // No ambiguity warning for a clean nested layout.
    assert!(
        !stderr.contains("contains both session files"),
        "unexpected ambiguity warning for clean nested dir; stderr: {stderr:?}"
    );
    assert!(
        !stderr.contains("no sessions found"),
        "pure nested key dir not discovered; stderr: {stderr:?}"
    );
}

// ---------------------------------------------------------------------------
// Helper: compute repo_key the same way as store::repo_key
// ---------------------------------------------------------------------------

fn compute_repo_key(repo_root: &std::path::Path, git_dir: &std::path::Path) -> String {
    let r = std::fs::canonicalize(repo_root).unwrap_or_else(|_| repo_root.to_path_buf());
    let g = std::fs::canonicalize(git_dir).unwrap_or_else(|_| git_dir.to_path_buf());
    let mut s = String::new();
    s.push_str(&r.to_string_lossy());
    s.push('\n');
    s.push_str(&g.to_string_lossy());
    fnv64_hex(s.as_bytes())
}

fn fnv64_hex(data: &[u8]) -> String {
    const OFFSET: u64 = 14695981039346656037;
    const PRIME: u64 = 1099511628211;
    let mut h = OFFSET;
    for &b in data {
        h ^= b as u64;
        h = h.wrapping_mul(PRIME);
    }
    format!("{h:016x}")
}

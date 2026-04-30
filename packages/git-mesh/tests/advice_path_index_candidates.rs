//! Integration tests: `git mesh advice <sid> flush` and `... read` use the
//! path-index to scope resolution to candidate meshes for the touched/read
//! paths, instead of the all-mesh `resolver::all_meshes` scan.
//!
//! Both tests seed multiple meshes against different files, exercise the
//! advice surface against a path tied to exactly one of them, and assert
//! that:
//!
//! 1. The `advice.{flush,read}.resolve-candidates` perf span fires.
//! 2. `resolver.resolve-meshes` (the all-mesh span emitted by
//!    `resolver::all_meshes`) does NOT fire — confirming the flush/read
//!    path no longer enumerates every mesh.
//! 3. Only the candidate mesh ends up in the session's
//!    `mesh-candidates.jsonl`.

mod support;

use anyhow::Result;
use git_mesh::{append_add, commit_mesh, set_why};
use std::process::Command;
use support::TestRepo;
use uuid::Uuid;

fn sid(prefix: &str) -> String {
    format!("advice-pi-{prefix}-{}", Uuid::new_v4())
}

fn seed_two_meshes(repo: &TestRepo) -> Result<()> {
    let gix = repo.gix_repo()?;
    append_add(&gix, "m-file1", "file1.txt", 1, 5, None)?;
    set_why(&gix, "m-file1", "scoped to file1")?;
    commit_mesh(&gix, "m-file1")?;

    append_add(&gix, "m-file2", "file2.txt", 1, 5, None)?;
    set_why(&gix, "m-file2", "scoped to file2")?;
    commit_mesh(&gix, "m-file2")?;
    Ok(())
}

fn mesh_candidates_for(repo: &TestRepo, session_id: &str) -> Vec<String> {
    let gix = match repo.gix_repo() {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };
    let session_dir =
        git_mesh::advice::session::store::session_dir(repo.path(), gix.git_dir(), session_id);
    let path = session_dir.join("mesh-candidates.jsonl");
    let Ok(contents) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    contents
        .lines()
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect()
}

#[test]
fn flush_uses_path_index_candidates_not_all_meshes() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed_two_meshes(&repo)?;

    let s = sid("flush");

    // Mark a tool-use, edit only file1.txt, then flush with perf on.
    let mark = repo.run_mesh(["advice", &s, "mark", "tool-1"])?;
    assert_eq!(mark.status.code(), Some(0));

    std::fs::write(
        repo.path().join("file1.txt"),
        "edited-line-1\nline2\nline3\nline4\nline5\nline6\nline7\nline8\nline9\nline10\n",
    )?;

    let out = Command::new(env!("CARGO_BIN_EXE_git-mesh"))
        .current_dir(repo.path())
        .env("GIT_MESH_PERF", "1")
        .args(["advice", &s, "flush", "tool-1"])
        .output()?;
    assert_eq!(out.status.code(), Some(0));
    let stderr = String::from_utf8(out.stderr)?;

    assert!(
        stderr.contains("git-mesh perf: advice.flush.resolve-candidates"),
        "expected advice.flush.resolve-candidates perf span; got: {stderr}"
    );
    assert!(
        !stderr.contains("git-mesh perf: resolver.resolve-meshes "),
        "flush must not run the all-mesh resolver span; got: {stderr}"
    );

    let candidates = mesh_candidates_for(&repo, &s);
    assert!(
        candidates
            .iter()
            .any(|c| c.contains("\"m-file1\"") || c == "m-file1"),
        "expected m-file1 in mesh-candidates.jsonl; got: {candidates:?}"
    );
    assert!(
        !candidates.iter().any(|c| c.contains("m-file2")),
        "m-file2 must not appear; got: {candidates:?}"
    );

    Ok(())
}

#[test]
fn read_uses_path_index_candidates_not_all_meshes() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed_two_meshes(&repo)?;

    let s = sid("read");

    let out = Command::new(env!("CARGO_BIN_EXE_git-mesh"))
        .current_dir(repo.path())
        .env("GIT_MESH_PERF", "1")
        .args(["advice", &s, "read", "file1.txt#L1-L5"])
        .output()?;
    assert_eq!(out.status.code(), Some(0));
    let stderr = String::from_utf8(out.stderr)?;

    assert!(
        stderr.contains("git-mesh perf: advice.read.resolve-candidates"),
        "expected advice.read.resolve-candidates perf span; got: {stderr}"
    );
    assert!(
        !stderr.contains("git-mesh perf: resolver.resolve-meshes "),
        "read must not run the all-mesh resolver span; got: {stderr}"
    );

    let candidates = mesh_candidates_for(&repo, &s);
    assert!(
        candidates
            .iter()
            .any(|c| c.contains("m-file1") || c == "m-file1"),
        "expected m-file1 in mesh-candidates.jsonl; got: {candidates:?}"
    );
    assert!(
        !candidates.iter().any(|c| c.contains("m-file2")),
        "m-file2 must not appear; got: {candidates:?}"
    );

    Ok(())
}

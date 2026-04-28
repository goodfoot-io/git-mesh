//! Integration tests for `scripts/migrate-ranges-to-anchors.mjs`.
//!
//! Each test builds a synthetic pre-rename repo by writing blobs and refs
//! directly via `git hash-object` / `git update-ref` / `git mktree` /
//! `git commit-tree`, then drives the migration script via `node`.

mod support;

use anyhow::Result;
use std::path::PathBuf;
use std::process::Command;
use support::TestRepo;

/// Absolute path to the migration script.
fn script_path() -> PathBuf {
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest.join("scripts/migrate-ranges-to-anchors.mjs")
}

/// Locate `node` on PATH at runtime.
fn node_bin() -> String {
    let out = Command::new("which")
        .arg("node")
        .output()
        .expect("which node failed");
    assert!(out.status.success(), "node not found on PATH");
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

/// Write a mesh commit with a "ranges" tree entry listing the given uuids.
fn plant_old_mesh(repo: &TestRepo, mesh_name: &str, uuids: &[&str]) -> Result<()> {
    let anchors_content = uuids.join("\n") + "\n";

    // Write the blob for the "ranges" file (content = uuid list).
    let ranges_blob = {
        let mut child = Command::new("git")
            .args(["-C", repo.path().to_str().unwrap()])
            .args(["hash-object", "-w", "--stdin"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()?;
        use std::io::Write;
        child
            .stdin
            .as_mut()
            .unwrap()
            .write_all(anchors_content.as_bytes())?;
        let output = child.wait_with_output()?;
        anyhow::ensure!(output.status.success(), "hash-object for ranges blob failed");
        String::from_utf8(output.stdout)?.trim().to_string()
    };

    // Write a config blob (minimal valid JSON).
    let config_blob = {
        let mut child = Command::new("git")
            .args(["-C", repo.path().to_str().unwrap()])
            .args(["hash-object", "-w", "--stdin"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()?;
        use std::io::Write;
        child.stdin.as_mut().unwrap().write_all(b"{}\n")?;
        let output = child.wait_with_output()?;
        anyhow::ensure!(output.status.success(), "hash-object for config blob failed");
        String::from_utf8(output.stdout)?.trim().to_string()
    };

    // Build a tree with "ranges" and "config" entries.
    let tree_input = format!(
        "100644 blob {ranges_blob}\tranges\n100644 blob {config_blob}\tconfig\n"
    );
    let tree_sha = {
        let mut child = Command::new("git")
            .args(["-C", repo.path().to_str().unwrap()])
            .arg("mktree")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()?;
        use std::io::Write;
        child
            .stdin
            .as_mut()
            .unwrap()
            .write_all(tree_input.as_bytes())?;
        let output = child.wait_with_output()?;
        anyhow::ensure!(output.status.success(), "mktree failed");
        String::from_utf8(output.stdout)?.trim().to_string()
    };

    // Build a commit with no parents.
    let commit_sha = repo.git_stdout([
        "commit-tree",
        &tree_sha,
        "-m",
        &format!("mesh: {mesh_name}"),
    ])?;

    repo.run_git([
        "update-ref",
        &format!("refs/meshes/v1/{mesh_name}"),
        &commit_sha,
    ])?;

    Ok(())
}

/// Run the migration script against the repo. Returns (stdout, stderr, exit code).
fn run_script(repo: &TestRepo, extra_args: &[&str]) -> Result<(String, String, i32)> {
    let node = node_bin();
    let script = script_path();
    let mut cmd = Command::new(&node);
    cmd.arg(script.to_str().unwrap());
    cmd.arg("--repo");
    cmd.arg(repo.path().to_str().unwrap());
    for a in extra_args {
        cmd.arg(a);
    }
    let out = cmd.output()?;
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    let code = out.status.code().unwrap_or(-1);
    Ok((stdout, stderr, code))
}

// ── Tests ─────────────────────────────────────────────────────────────────

/// Happy path: two old-format anchor refs + one mesh; verify post-conditions.
#[test]
fn migrates_refs_and_mesh_tree() -> Result<()> {
    let repo = TestRepo::new()?;
    // Need at least one commit for a realistic commit SHA in the blob.
    repo.write_file("file.txt", "line1\nline2\nline3\n")?;
    let commit_sha = repo.commit_all("initial")?;

    let uuid1 = "aaaaaaaa-0000-0000-0000-000000000001";
    let uuid2 = "aaaaaaaa-0000-0000-0000-000000000002";

    plant_old_anchor_v2(&repo, uuid1, &commit_sha, "file.txt")?;
    plant_old_anchor_v2(&repo, uuid2, &commit_sha, "file.txt")?;
    plant_old_mesh(&repo, "my-mesh", &[uuid1, uuid2])?;

    // Pre-conditions.
    assert!(repo.ref_exists(&format!("refs/ranges/v1/{uuid1}")));
    assert!(repo.ref_exists(&format!("refs/ranges/v1/{uuid2}")));
    assert!(!repo.ref_exists(&format!("refs/anchors/v1/{uuid1}")));

    let (stdout, stderr, code) = run_script(&repo, &[])?;
    assert_eq!(
        code, 0,
        "script exited non-zero\nstdout={stdout}\nstderr={stderr}"
    );

    // refs/ranges/v1/* must be gone.
    let ranges = repo.list_refs("refs/ranges/v1")?;
    assert!(ranges.is_empty(), "old refs still present: {ranges:?}");

    // refs/anchors/v1/* must exist for both uuids.
    assert!(
        repo.ref_exists(&format!("refs/anchors/v1/{uuid1}")),
        "new ref missing for {uuid1}"
    );
    assert!(
        repo.ref_exists(&format!("refs/anchors/v1/{uuid2}")),
        "new ref missing for {uuid2}"
    );

    // Each new blob must have "commit" and "extent" keywords, not the old ones.
    for uuid in &[uuid1, uuid2] {
        let blob_sha = repo.git_stdout(["rev-parse", &format!("refs/anchors/v1/{uuid}")])?;
        let blob_text = repo.git_stdout(["cat-file", "blob", &blob_sha])?;
        assert!(
            blob_text.contains("commit "),
            "new blob missing 'commit' for {uuid}: {blob_text}"
        );
        assert!(
            blob_text.contains("extent "),
            "new blob missing 'extent' for {uuid}: {blob_text}"
        );
        assert!(
            !blob_text.contains("anchor "),
            "new blob still has old 'anchor' for {uuid}: {blob_text}"
        );
        assert!(
            !blob_text.starts_with("range ") && !blob_text.contains("\nrange "),
            "new blob still has old 'range' for {uuid}: {blob_text}"
        );
    }

    // Mesh tree must have "anchors" file and no "ranges" file.
    let mesh_commit = repo.git_stdout(["rev-parse", "refs/meshes/v1/my-mesh"])?;
    let tree_entries = repo.git_stdout(["ls-tree", &mesh_commit])?;
    assert!(
        tree_entries.contains("\tanchors"),
        "mesh tree missing 'anchors' entry: {tree_entries}"
    );
    assert!(
        !tree_entries.contains("\tranges"),
        "mesh tree still has 'ranges' entry: {tree_entries}"
    );

    // The uuid list in the "anchors" blob must match what we planted.
    let anchors_content =
        repo.git_stdout(["show", &format!("{mesh_commit}:anchors")])?;
    assert!(
        anchors_content.contains(uuid1),
        "anchors blob missing {uuid1}"
    );
    assert!(
        anchors_content.contains(uuid2),
        "anchors blob missing {uuid2}"
    );

    // The Rust binary must be able to parse each migrated anchor.
    for uuid in &[uuid1, uuid2] {
        let out = repo.run_mesh(["show", "my-mesh"])?;
        assert!(
            out.status.success(),
            "git mesh show failed after migration\nstderr={}",
            String::from_utf8_lossy(&out.stderr)
        );
        let _ = uuid; // uuid is validated indirectly via show
    }

    // Summary line should mention the count.
    assert!(
        stdout.contains("migrated 2"),
        "unexpected summary: {stdout}"
    );

    Ok(())
}

/// Idempotency: re-running on an already-migrated repo exits 0, no writes.
#[test]
fn idempotent_on_already_migrated_repo() -> Result<()> {
    let repo = TestRepo::new()?;
    repo.write_file("file.txt", "line1\nline2\nline3\n")?;
    let commit_sha = repo.commit_all("initial")?;

    let uuid1 = "bbbbbbbb-0000-0000-0000-000000000001";
    plant_old_anchor_v2(&repo, uuid1, &commit_sha, "file.txt")?;
    plant_old_mesh(&repo, "m", &[uuid1])?;

    // First run — migrate.
    let (_, _, code) = run_script(&repo, &[])?;
    assert_eq!(code, 0);

    // Record state after first run.
    let anchor_sha_after_first =
        repo.git_stdout(["rev-parse", &format!("refs/anchors/v1/{uuid1}")])?;

    // Second run — must be idempotent.
    let (stdout, stderr, code2) = run_script(&repo, &[])?;
    assert_eq!(
        code2, 0,
        "second run exited non-zero\nstdout={stdout}\nstderr={stderr}"
    );
    assert!(
        stdout.contains("already migrated"),
        "expected idempotent message, got: {stdout}"
    );

    // No new writes: anchor sha must be unchanged.
    let anchor_sha_after_second =
        repo.git_stdout(["rev-parse", &format!("refs/anchors/v1/{uuid1}")])?;
    assert_eq!(
        anchor_sha_after_first, anchor_sha_after_second,
        "second run changed anchor sha"
    );

    Ok(())
}

/// --dry-run prints planned operations but makes no mutations.
#[test]
fn dry_run_makes_no_writes() -> Result<()> {
    let repo = TestRepo::new()?;
    repo.write_file("file.txt", "line1\nline2\nline3\n")?;
    let commit_sha = repo.commit_all("initial")?;

    let uuid1 = "cccccccc-0000-0000-0000-000000000001";
    plant_old_anchor_v2(&repo, uuid1, &commit_sha, "file.txt")?;

    let (stdout, _stderr, code) = run_script(&repo, &["--dry-run"])?;
    assert_eq!(code, 0, "dry-run exited non-zero\nstdout={stdout}");
    assert!(
        stdout.contains("[dry-run]"),
        "expected dry-run output, got: {stdout}"
    );

    // Old ref must still exist.
    assert!(
        repo.ref_exists(&format!("refs/ranges/v1/{uuid1}")),
        "dry-run deleted the old ref"
    );
    // New ref must NOT exist.
    assert!(
        !repo.ref_exists(&format!("refs/anchors/v1/{uuid1}")),
        "dry-run created the new ref"
    );

    Ok(())
}

/// Malformed blob (missing required `anchor` keyword) → non-zero exit before
/// any deletion.
// Note: this also covers the fail-closed contract: old refs are left intact
// on any parse error.
#[test]
fn fails_closed_on_malformed_blob() -> Result<()> {
    let repo = TestRepo::new()?;
    repo.write_file("file.txt", "line1\nline2\nline3\n")?;
    let _commit_sha = repo.commit_all("initial")?;

    let uuid = "dddddddd-0000-0000-0000-000000000001";

    // Write a blob that is missing the "anchor" header.
    let bad_blob = "created 2025-01-01T00:00:00Z\nrange 1 3 aabbccddaabbccddaabbccddaabbccddaabbccdd\tfile.txt\n";
    let blob_sha = {
        let mut child = Command::new("git")
            .args(["-C", repo.path().to_str().unwrap()])
            .args(["hash-object", "-w", "--stdin"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()?;
        use std::io::Write;
        child.stdin.as_mut().unwrap().write_all(bad_blob.as_bytes())?;
        let output = child.wait_with_output()?;
        anyhow::ensure!(output.status.success(), "hash-object failed");
        String::from_utf8(output.stdout)?.trim().to_string()
    };
    repo.run_git(["update-ref", &format!("refs/ranges/v1/{uuid}"), &blob_sha])?;

    let (stdout, stderr, code) = run_script(&repo, &[])?;
    assert_ne!(
        code, 0,
        "expected non-zero exit for malformed blob\nstdout={stdout}\nstderr={stderr}"
    );

    // Old ref must still be intact (fail-closed).
    assert!(
        repo.ref_exists(&format!("refs/ranges/v1/{uuid}")),
        "malformed-blob run deleted the old ref"
    );

    Ok(())
}

// ── Helper (deduped) ──────────────────────────────────────────────────────

/// Write an old-format anchor blob and create refs/ranges/v1/<uuid>.
/// Avoids the borrow/spawn tangle in the first draft of plant_old_anchor.
fn plant_old_anchor_v2(
    repo: &TestRepo,
    uuid: &str,
    commit_sha: &str,
    path: &str,
) -> Result<()> {
    let old_blob = format!(
        "anchor {commit_sha}\ncreated 2025-01-01T00:00:00Z\nrange 1 3 aabbccddaabbccddaabbccddaabbccddaabbccdd\t{path}\n"
    );
    let blob_sha = {
        let mut child = Command::new("git")
            .args(["-C", repo.path().to_str().unwrap()])
            .args(["hash-object", "-w", "--stdin"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()?;
        use std::io::Write;
        child
            .stdin
            .as_mut()
            .unwrap()
            .write_all(old_blob.as_bytes())?;
        let output = child.wait_with_output()?;
        anyhow::ensure!(output.status.success(), "hash-object failed");
        String::from_utf8(output.stdout)?.trim().to_string()
    };
    repo.run_git(["update-ref", &format!("refs/ranges/v1/{uuid}"), &blob_sha])?;
    Ok(())
}

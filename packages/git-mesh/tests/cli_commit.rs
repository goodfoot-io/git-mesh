//! CLI: add, rm, message, commit, config.

mod support;

use anyhow::Result;
use support::TestRepo;

/// Concatenate every file in `.git/mesh/staging/` (ops, sidecars, msg)
/// for assertion purposes — replaces the deleted `git mesh status`
/// view used by these tests to check what was staged.
fn staging_dump(repo: &TestRepo, mesh: &str) -> String {
    let dir = repo.path().join(".git").join("mesh").join("staging");
    let mut out = String::new();
    if !dir.exists() {
        return out;
    }
    let mut entries: Vec<_> = std::fs::read_dir(&dir)
        .map(|i| i.flatten().collect())
        .unwrap_or_default();
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let fname = entry.file_name().to_string_lossy().into_owned();
        // Match `<mesh>` ops file, `<mesh>.<N>` sidecars, `<mesh>.why`,
        // and the metadata sidecars `<mesh>.<N>.meta`.
        if (fname == mesh || fname.starts_with(&format!("{mesh}.")))
            && let Ok(s) = std::fs::read_to_string(entry.path())
        {
            out.push_str(&s);
            out.push('\n');
        }
    }
    out
}

#[test]

fn cli_add_stages_range() -> Result<()> {
    let repo = TestRepo::seeded()?;
    repo.mesh_stdout(["add", "m", "file1.txt#L1-L5"])?;
    let status = staging_dump(&repo, "m");
    assert!(status.contains("file1.txt"));
    assert!(status.contains("1-5") || status.contains("L1-L5"));
    Ok(())
}

#[test]

fn cli_add_accepts_at_anchor() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let head = repo.head_sha()?;
    repo.mesh_stdout(["add", "m", "file1.txt#L1-L5", "--at", &head])?;
    Ok(())
}

#[test]

fn cli_add_rejects_bad_address() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let out = repo.run_mesh(["add", "m", "oops-no-fragment"])?;
    assert!(!out.status.success());
    Ok(())
}

#[test]
fn cli_add_rejects_missing_path() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let out = repo.run_mesh(["add", "m", "no/such.txt#L1-L2"])?;
    assert!(!out.status.success());
    Ok(())
}

#[test]
fn cli_add_rejects_end_past_eof() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let out = repo.run_mesh(["add", "m", "file1.txt#L1-L99"])?;
    assert!(!out.status.success());
    Ok(())
}

#[test]
fn cli_add_is_atomic_when_any_range_is_invalid() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let out = repo.run_mesh(["add", "m", "file1.txt#L1-L5", "file2.txt#L99-L100"])?;
    assert!(!out.status.success());
    let status = staging_dump(&repo, "m");
    assert!(!status.contains("file1.txt#L1-L5"), "status={status}");
    Ok(())
}

#[test]
fn cli_add_is_atomic_when_any_path_is_missing() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let out = repo.run_mesh(["add", "m", "file1.txt#L1-L5", "no/such.txt#L1-L2"])?;
    assert!(!out.status.success());
    let status = staging_dump(&repo, "m");
    assert!(!status.contains("file1.txt#L1-L5"), "status={status}");
    Ok(())
}

#[test]
fn cli_commit_accepts_paths_with_spaces() -> Result<()> {
    let repo = TestRepo::seeded()?;
    repo.commit_file(
        "dir with spaces/file 3.txt",
        "line1\nline2\nline3\n",
        "add spaced file",
    )?;
    repo.mesh_stdout(["add", "spaced", "dir with spaces/file 3.txt#L1-L2"])?;
    repo.mesh_stdout(["why", "spaced", "-m", "Initial"])?;
    repo.mesh_stdout(["commit", "spaced"])?;
    let out = repo.mesh_stdout(["show", "spaced", "--oneline"])?;
    assert!(
        out.contains("dir with spaces/file 3.txt#L1-L2"),
        "show={out}"
    );
    Ok(())
}

#[test]
fn cli_add_accepts_paths_with_spaces_alongside_other_ranges() -> Result<()> {
    let repo = TestRepo::seeded()?;
    repo.commit_file(
        "dir with spaces/file 3.txt",
        "line1\nline2\nline3\n",
        "add spaced file",
    )?;
    repo.mesh_stdout([
        "add",
        "spaced",
        "dir with spaces/file 3.txt#L1-L2",
        "file1.txt#L1-L3",
    ])?;
    let status = staging_dump(&repo, "spaced");
    assert!(
        status.contains("dir with spaces/file 3.txt#L1-L2"),
        "status={status}"
    );
    assert!(status.contains("file1.txt#L1-L3"), "status={status}");
    Ok(())
}

#[test]

fn cli_rm_stages_remove() -> Result<()> {
    let repo = TestRepo::seeded()?;
    repo.mesh_stdout(["add", "m", "file1.txt#L1-L5"])?;
    repo.mesh_stdout(["why", "m", "-m", "seed"])?;
    repo.mesh_stdout(["commit", "m"])?;
    repo.mesh_stdout(["rm", "m", "file1.txt#L1-L5"])?;
    let status = staging_dump(&repo, "m");
    assert!(status.contains("remove") || status.contains("rm"));
    Ok(())
}

#[test]

fn cli_message_inline() -> Result<()> {
    let repo = TestRepo::seeded()?;
    repo.mesh_stdout(["why", "m", "-m", "Hello"])?;
    let status = staging_dump(&repo, "m");
    assert!(status.contains("Hello"));
    Ok(())
}

#[test]

fn cli_message_from_file() -> Result<()> {
    let repo = TestRepo::seeded()?;
    repo.write_file("msg.txt", "Subject\n\nBody\n")?;
    repo.mesh_stdout(["why", "m", "-F", "msg.txt"])?;
    Ok(())
}

#[test]

fn cli_commit_writes_ref() -> Result<()> {
    let repo = TestRepo::seeded()?;
    repo.mesh_stdout(["add", "m", "file1.txt#L1-L5"])?;
    repo.mesh_stdout(["why", "m", "-m", "Initial"])?;
    repo.mesh_stdout(["commit", "m"])?;
    assert!(repo.ref_exists("refs/meshes/v1/m"));
    Ok(())
}

#[test]

fn cli_commit_empty_is_error() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let out = repo.run_mesh(["commit", "empty"])?;
    assert!(!out.status.success());
    Ok(())
}

#[test]

fn cli_config_read_lists_defaults() -> Result<()> {
    let repo = TestRepo::seeded()?;
    repo.mesh_stdout(["add", "m", "file1.txt#L1-L5"])?;
    repo.mesh_stdout(["why", "m", "-m", "seed"])?;
    repo.mesh_stdout(["commit", "m"])?;
    let out = repo.mesh_stdout(["config", "m"])?;
    assert!(out.contains("copy-detection"));
    assert!(out.contains("ignore-whitespace"));
    Ok(())
}

#[test]

fn cli_config_stage_override_shows_starred_line() -> Result<()> {
    let repo = TestRepo::seeded()?;
    repo.mesh_stdout(["add", "m", "file1.txt#L1-L5"])?;
    repo.mesh_stdout(["why", "m", "-m", "seed"])?;
    repo.mesh_stdout(["commit", "m"])?;
    repo.mesh_stdout(["config", "m", "copy-detection", "off"])?;
    let out = repo.mesh_stdout(["config", "m"])?;
    assert!(out.contains("* copy-detection"));
    assert!(out.contains("(staged)"));
    Ok(())
}

#[test]

fn cli_config_unknown_key_errors() -> Result<()> {
    let repo = TestRepo::seeded()?;
    repo.mesh_stdout(["add", "m", "file1.txt#L1-L5"])?;
    repo.mesh_stdout(["why", "m", "-m", "seed"])?;
    repo.mesh_stdout(["commit", "m"])?;
    let out = repo.run_mesh(["config", "m", "no-such-key"])?;
    assert!(!out.status.success());
    Ok(())
}

#[test]

fn cli_commit_reserved_name_rejected() -> Result<()> {
    let repo = TestRepo::seeded()?;
    // `stale` is on the reserved list — clap may treat it as a subcommand.
    let out = repo.run_mesh(["add", "stale", "file1.txt#L1-L5"])?;
    assert!(!out.status.success());
    Ok(())
}

#[test]
fn cli_config_unset_stages_default() -> Result<()> {
    let repo = TestRepo::seeded()?;
    repo.mesh_stdout(["add", "m", "file1.txt#L1-L5"])?;
    repo.mesh_stdout(["why", "m", "-m", "seed"])?;
    repo.mesh_stdout(["commit", "m"])?;
    // Stage a non-default, then --unset resets to default.
    repo.mesh_stdout(["config", "m", "ignore-whitespace", "true"])?;
    repo.mesh_stdout(["config", "m", "--unset", "ignore-whitespace"])?;
    let out = repo.mesh_stdout(["config", "m"])?;
    // Final resolved staged value is `false` (default); the committed
    // value is also `false`, so the displayed line is the un-starred
    // default.
    assert!(out.contains("ignore-whitespace false"));
    Ok(())
}

/// Build a POSIX-shell editor script that replaces the EDITMSG file
/// with `content`. Returns the path to the script.
fn make_editor_script(repo: &TestRepo, content: &str) -> Result<std::path::PathBuf> {
    let p = repo.path().join("fake-editor.sh");
    let body = format!("#!/bin/sh\ncat >\"$1\" <<'__MESH_EOF__'\n{content}\n__MESH_EOF__\n");
    std::fs::write(&p, body)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perm = std::fs::metadata(&p)?.permissions();
        perm.set_mode(0o755);
        std::fs::set_permissions(&p, perm)?;
    }
    Ok(p)
}

fn run_mesh_with_editor(
    repo: &TestRepo,
    editor: &std::path::Path,
    args: &[&str],
) -> Result<std::process::Output> {
    let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_git-mesh"));
    cmd.current_dir(repo.path());
    cmd.env("EDITOR", editor);
    cmd.env_remove("VISUAL");
    cmd.env_remove("GIT_EDITOR");
    for a in args {
        cmd.arg(a);
    }
    Ok(cmd.output()?)
}

#[test]
fn cli_message_edit_blank_template_new_mesh() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let editor = make_editor_script(&repo, "Hello from editor")?;
    let out = run_mesh_with_editor(&repo, &editor, &["why", "m", "--edit"])?;
    assert!(
        out.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let status = staging_dump(&repo, "m");
    assert!(status.contains("Hello from editor"), "status={status}");
    Ok(())
}

#[test]
fn cli_message_edit_prepopulated_from_existing() -> Result<()> {
    let repo = TestRepo::seeded()?;
    repo.mesh_stdout(["why", "m", "-m", "Pre-existing text"])?;
    // Editor appends a suffix to whatever the template was.
    let editor_path = repo.path().join("fake-editor.sh");
    let body = "#!/bin/sh\nprintf '%s\\n-edited' \"$(cat \"$1\")\" >\"$1\"\n";
    std::fs::write(&editor_path, body)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perm = std::fs::metadata(&editor_path)?.permissions();
        perm.set_mode(0o755);
        std::fs::set_permissions(&editor_path, perm)?;
    }
    let out = run_mesh_with_editor(&repo, &editor_path, &["why", "m", "--edit"])?;
    assert!(out.status.success());
    let status = staging_dump(&repo, "m");
    assert!(status.contains("Pre-existing text"), "status={status}");
    assert!(status.contains("-edited"), "status={status}");
    Ok(())
}

#[test]
fn cli_message_edit_inherits_from_parent_commit() -> Result<()> {
    let repo = TestRepo::seeded()?;
    repo.mesh_stdout(["add", "m", "file1.txt#L1-L5"])?;
    repo.mesh_stdout(["why", "m", "-m", "Parent commit message"])?;
    repo.mesh_stdout(["commit", "m"])?;
    // No staged .why exists; editor should see the parent's message.
    let editor_path = repo.path().join("fake-editor.sh");
    let body = "#!/bin/sh\ncp \"$1\" \"$1.seen\"\ncat >\"$1\" <<'__EOF__'\nNew body\n__EOF__\n";
    std::fs::write(&editor_path, body)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perm = std::fs::metadata(&editor_path)?.permissions();
        perm.set_mode(0o755);
        std::fs::set_permissions(&editor_path, perm)?;
    }
    let out = run_mesh_with_editor(&repo, &editor_path, &["why", "m", "--edit"])?;
    assert!(out.status.success());
    // Collect the snapshot of what the editor saw.
    let seen_path = repo
        .path()
        .join(".git")
        .join("mesh")
        .join("staging")
        .join("m.why.EDITMSG.seen");
    let seen = std::fs::read_to_string(&seen_path)?;
    assert!(
        seen.contains("Parent commit message"),
        "editor template was: {seen}"
    );
    Ok(())
}

#[test]
fn cli_message_edit_empty_buffer_aborts() -> Result<()> {
    let repo = TestRepo::seeded()?;
    // Editor produces only comment lines — stripped => empty => abort.
    let editor = make_editor_script(&repo, "# only a comment\n# another")?;
    let out = run_mesh_with_editor(&repo, &editor, &["why", "m", "--edit"])?;
    assert!(!out.status.success(), "abort should fail");
    let msg_path = repo
        .path()
        .join(".git")
        .join("mesh")
        .join("staging")
        .join("m.why");
    assert!(!msg_path.exists(), "empty-abort must not write .why");
    Ok(())
}

#[test]
fn cli_why_edit_flag_triggers_editor() -> Result<()> {
    // Per `docs/why-plan.md` §B1, the editor flow is opted into with
    // `--edit`. Bare `git mesh why <name>` is the reader form.
    let repo = TestRepo::seeded()?;
    let editor = make_editor_script(&repo, "edit flow worked")?;
    let out = run_mesh_with_editor(&repo, &editor, &["why", "m", "--edit"])?;
    assert!(
        out.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let status = staging_dump(&repo, "m");
    assert!(status.contains("edit flow worked"), "status={status}");
    Ok(())
}

#[test]
fn cli_config_unset_unknown_key_errors() -> Result<()> {
    let repo = TestRepo::seeded()?;
    repo.mesh_stdout(["add", "m", "file1.txt#L1-L5"])?;
    repo.mesh_stdout(["why", "m", "-m", "seed"])?;
    repo.mesh_stdout(["commit", "m"])?;
    let out = repo.run_mesh(["config", "m", "--unset", "nope"])?;
    assert!(!out.status.success());
    Ok(())
}

#[test]
fn cli_add_coalesces_duplicate_ranges_within_args() -> Result<()> {
    // Slice 3: silent coalesce — last occurrence wins, exit 0.
    let repo = TestRepo::seeded()?;
    let out = repo.run_mesh(["add", "m", "file1.txt#L1-L2", "file1.txt#L1-L2"])?;
    assert!(
        out.status.success(),
        "expected success; stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let status = staging_dump(&repo, "m");
    let occurrences = status.matches("file1.txt#L1-L2").count();
    assert_eq!(occurrences, 1, "expected one staged op; status={status}");
    Ok(())
}

#[test]
fn cli_add_supersedes_already_staged_duplicate() -> Result<()> {
    // Slice 3: re-adding the same `(path, extent)` after an edit
    // succeeds and supersedes the earlier add (sidecar reflects the
    // newer bytes).
    let repo = TestRepo::seeded()?;
    repo.mesh_stdout(["add", "m", "file1.txt#L1-L2"])?;
    repo.write_file(
        "file1.txt",
        "lineONE\nlineTWO\nline3\nline4\nline5\nline6\nline7\nline8\nline9\nline10\n",
    )?;
    let out = repo.run_mesh(["add", "m", "file1.txt#L1-L2"])?;
    assert!(
        out.status.success(),
        "expected success; stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let status = staging_dump(&repo, "m");
    let occurrences = status.matches("add file1.txt#L1-L2").count();
    assert_eq!(occurrences, 1, "expected one staged add; status={status}");
    // Sidecar reflects the post-edit bytes.
    let sidecar = std::fs::read_to_string(
        repo.path()
            .join(".git")
            .join("mesh")
            .join("staging")
            .join("m.1"),
    )?;
    assert!(sidecar.starts_with("lineONE\n"), "sidecar={sidecar}");
    Ok(())
}

#[test]
fn cli_rm_of_range_not_in_mesh_errors() -> Result<()> {
    let repo = TestRepo::seeded()?;
    repo.mesh_stdout(["add", "m", "file1.txt#L1-L5"])?;
    repo.mesh_stdout(["why", "m", "-m", "seed"])?;
    repo.mesh_stdout(["commit", "m"])?;
    let out = repo.run_mesh(["rm", "m", "file1.txt#L7-L9"])?;
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("anchor not in mesh"), "stderr={stderr}");
    Ok(())
}

#[test]
fn cli_rm_of_staged_add_succeeds() -> Result<()> {
    // Removing a range that exists only as a staged add should be allowed
    // (it "undoes" the add in the same staging pass).
    let repo = TestRepo::seeded()?;
    repo.mesh_stdout(["add", "m", "file1.txt#L1-L5"])?;
    let out = repo.run_mesh(["rm", "m", "file1.txt#L1-L5"])?;
    assert!(
        out.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    Ok(())
}

#[test]
fn cli_commit_no_name_commits_all_staged() -> Result<()> {
    let repo = TestRepo::seeded()?;
    repo.mesh_stdout(["add", "a", "file1.txt#L1-L2"])?;
    repo.mesh_stdout(["why", "a", "-m", "mesh a"])?;
    repo.mesh_stdout(["add", "b", "file2.txt#L1-L2"])?;
    repo.mesh_stdout(["why", "b", "-m", "mesh b"])?;
    let stdout = repo.mesh_stdout(["commit"])?;
    assert!(stdout.contains("refs/meshes/v1/a"), "stdout={stdout}");
    assert!(stdout.contains("refs/meshes/v1/b"), "stdout={stdout}");
    assert!(repo.ref_exists("refs/meshes/v1/a"));
    assert!(repo.ref_exists("refs/meshes/v1/b"));
    Ok(())
}

#[test]
fn cli_commit_no_name_nothing_staged_exits_zero() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let out = repo.run_mesh(["commit"])?;
    assert!(
        out.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("nothing staged"), "stdout={stdout}");
    Ok(())
}

#[test]
fn cli_why_reader_prints_current_and_historical_text() -> Result<()> {
    // `docs/why-plan.md` §B2: bare `git mesh why <name>` prints the
    // current why; `--at <commit>` prints the why at a historical
    // mesh commit.
    let repo = TestRepo::seeded()?;
    repo.mesh_stdout(["add", "h", "file1.txt#L1-L5"])?;
    repo.mesh_stdout(["why", "h", "-m", "v1 why"])?;
    repo.mesh_stdout(["commit", "h"])?;
    repo.mesh_stdout(["why", "h", "-m", "v2 why"])?;
    repo.mesh_stdout(["commit", "h"])?;

    let current = repo.mesh_stdout(["why", "h"])?;
    assert!(current.contains("v2 why"), "current={current}");

    let historical = repo.mesh_stdout(["why", "h", "--at", "HEAD~1"])?;
    assert!(historical.contains("v1 why"), "historical={historical}");
    assert!(!historical.contains("v2 why"), "historical={historical}");
    Ok(())
}

#[test]
fn cli_commit_first_mesh_without_why_errors_with_guidance() -> Result<()> {
    // `docs/why-plan.md` §B4: a new mesh has no parent to inherit from,
    // so committing without a staged why must hard-fail. The error must
    // map to `Error::WhyRequired` and the rendered message must point
    // the operator at `git mesh why <name> -m`.
    let repo = TestRepo::seeded()?;
    repo.mesh_stdout(["add", "fresh", "file1.txt#L1-L5"])?;
    let out = repo.run_mesh(["commit", "fresh"])?;
    assert!(!out.status.success(), "expected non-zero exit");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("git mesh why fresh -m"),
        "expected guidance pointing at writer form; stderr={stderr}"
    );
    Ok(())
}

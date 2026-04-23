//! CLI: add, rm, message, commit, status, config.

mod support;

use anyhow::Result;
use support::TestRepo;

#[test]

fn cli_add_stages_range() -> Result<()> {
    let repo = TestRepo::seeded()?;
    repo.mesh_stdout(["add", "m", "file1.txt#L1-L5"])?;
    let status = repo.mesh_stdout(["status", "m"])?;
    assert!(status.contains("file1.txt"));
    assert!(status.contains("L1-L5"));
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

fn cli_rm_stages_remove() -> Result<()> {
    let repo = TestRepo::seeded()?;
    repo.mesh_stdout(["add", "m", "file1.txt#L1-L5"])?;
    repo.mesh_stdout(["message", "m", "-m", "seed"])?;
    repo.mesh_stdout(["commit", "m"])?;
    repo.mesh_stdout(["rm", "m", "file1.txt#L1-L5"])?;
    let status = repo.mesh_stdout(["status", "m"])?;
    assert!(status.contains("remove") || status.contains("rm"));
    Ok(())
}

#[test]

fn cli_message_inline() -> Result<()> {
    let repo = TestRepo::seeded()?;
    repo.mesh_stdout(["message", "m", "-m", "Hello"])?;
    let status = repo.mesh_stdout(["status", "m"])?;
    assert!(status.contains("Hello"));
    Ok(())
}

#[test]

fn cli_message_from_file() -> Result<()> {
    let repo = TestRepo::seeded()?;
    repo.write_file("msg.txt", "Subject\n\nBody\n")?;
    repo.mesh_stdout(["message", "m", "-F", "msg.txt"])?;
    Ok(())
}

#[test]

fn cli_commit_writes_ref() -> Result<()> {
    let repo = TestRepo::seeded()?;
    repo.mesh_stdout(["add", "m", "file1.txt#L1-L5"])?;
    repo.mesh_stdout(["message", "m", "-m", "Initial"])?;
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

fn cli_status_check_exit_code_clean() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let out = repo.run_mesh(["status", "--check"])?;
    assert_eq!(out.status.code(), Some(0));
    Ok(())
}

#[test]

fn cli_status_check_exit_code_drifty() -> Result<()> {
    let repo = TestRepo::seeded()?;
    repo.mesh_stdout(["add", "m", "file1.txt#L1-L5"])?;
    // drift the worktree
    repo.write_file(
        "file1.txt",
        "DRIFT\nline2\nline3\nline4\nline5\nline6\nline7\nline8\nline9\nline10\n",
    )?;
    let out = repo.run_mesh(["status", "--check"])?;
    assert_ne!(out.status.code(), Some(0));
    Ok(())
}

#[test]

fn cli_config_read_lists_defaults() -> Result<()> {
    let repo = TestRepo::seeded()?;
    repo.mesh_stdout(["add", "m", "file1.txt#L1-L5"])?;
    repo.mesh_stdout(["message", "m", "-m", "seed"])?;
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
    repo.mesh_stdout(["message", "m", "-m", "seed"])?;
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
    repo.mesh_stdout(["message", "m", "-m", "seed"])?;
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
fn cli_status_header_matches_git_show_shape() -> Result<()> {
    let repo = TestRepo::seeded()?;
    repo.mesh_stdout(["add", "m", "file1.txt#L1-L5"])?;
    repo.mesh_stdout(["message", "m", "-m", "Initial subject"])?;
    repo.mesh_stdout(["commit", "m"])?;
    // Stage a second add without committing.
    repo.mesh_stdout(["add", "m", "file2.txt#L1-L3"])?;
    let out = repo.mesh_stdout(["status", "m"])?;
    assert!(out.starts_with("mesh m\n"), "header: {out}");
    assert!(out.contains("commit "));
    assert!(out.contains("Author:"));
    assert!(out.contains("Date:"));
    assert!(out.contains("    Initial subject"));
    assert!(out.contains("Staged changes:"));
    assert!(out.contains("  add     file2.txt#L1-L3"));
    Ok(())
}

#[test]
fn cli_status_shows_staged_message_and_config() -> Result<()> {
    let repo = TestRepo::seeded()?;
    repo.mesh_stdout(["add", "m", "file1.txt#L1-L5"])?;
    repo.mesh_stdout(["message", "m", "-m", "seed"])?;
    repo.mesh_stdout(["commit", "m"])?;
    // Stage a fresh message + config on top of the committed mesh.
    repo.mesh_stdout(["message", "m", "-m", "my staged subject"])?;
    repo.mesh_stdout(["config", "m", "copy-detection", "off"])?;
    let out = repo.mesh_stdout(["status", "m"])?;
    assert!(out.contains("Staged message:"));
    assert!(out.contains("  my staged subject"));
    assert!(out.contains("  config  copy-detection off"));
    Ok(())
}

#[test]
fn cli_status_check_prints_drift_diff() -> Result<()> {
    let repo = TestRepo::seeded()?;
    repo.mesh_stdout(["add", "m", "file1.txt#L1-L5"])?;
    repo.write_file(
        "file1.txt",
        "DRIFT\nline2\nline3\nline4\nline5\nline6\nline7\nline8\nline9\nline10\n",
    )?;
    let out = repo.run_mesh(["status", "--check"])?;
    assert_ne!(out.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("Working tree drift:"));
    assert!(stdout.contains("file1.txt#L1-L5"));
    assert!(stdout.contains("--- file1.txt#L1-L5 (staged)"));
    assert!(stdout.contains("+++ file1.txt#L1-L5 (working tree)"));
    assert!(stdout.contains("@@ "));
    Ok(())
}

#[test]
fn cli_config_unset_stages_default() -> Result<()> {
    let repo = TestRepo::seeded()?;
    repo.mesh_stdout(["add", "m", "file1.txt#L1-L5"])?;
    repo.mesh_stdout(["message", "m", "-m", "seed"])?;
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
    let out = run_mesh_with_editor(&repo, &editor, &["message", "m", "--edit"])?;
    assert!(
        out.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let status = repo.mesh_stdout(["status", "m"])?;
    assert!(status.contains("Hello from editor"), "status={status}");
    Ok(())
}

#[test]
fn cli_message_edit_prepopulated_from_existing() -> Result<()> {
    let repo = TestRepo::seeded()?;
    repo.mesh_stdout(["message", "m", "-m", "Pre-existing text"])?;
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
    let out = run_mesh_with_editor(&repo, &editor_path, &["message", "m", "--edit"])?;
    assert!(out.status.success());
    let status = repo.mesh_stdout(["status", "m"])?;
    assert!(status.contains("Pre-existing text"), "status={status}");
    assert!(status.contains("-edited"), "status={status}");
    Ok(())
}

#[test]
fn cli_message_edit_inherits_from_parent_commit() -> Result<()> {
    let repo = TestRepo::seeded()?;
    repo.mesh_stdout(["add", "m", "file1.txt#L1-L5"])?;
    repo.mesh_stdout(["message", "m", "-m", "Parent commit message"])?;
    repo.mesh_stdout(["commit", "m"])?;
    // No staged .msg exists; editor should see the parent's message.
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
    let out = run_mesh_with_editor(&repo, &editor_path, &["message", "m", "--edit"])?;
    assert!(out.status.success());
    // Collect the snapshot of what the editor saw.
    let seen_path = repo
        .path()
        .join(".git")
        .join("mesh")
        .join("staging")
        .join("m.msg.EDITMSG.seen");
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
    let out = run_mesh_with_editor(&repo, &editor, &["message", "m", "--edit"])?;
    assert!(!out.status.success(), "abort should fail");
    let msg_path = repo
        .path()
        .join(".git")
        .join("mesh")
        .join("staging")
        .join("m.msg");
    assert!(!msg_path.exists(), "empty-abort must not write .msg");
    Ok(())
}

#[test]
fn cli_message_bare_triggers_editor() -> Result<()> {
    // §10.2: `git mesh message <name>` with no flags = --edit.
    let repo = TestRepo::seeded()?;
    let editor = make_editor_script(&repo, "bare edit worked")?;
    let out = run_mesh_with_editor(&repo, &editor, &["message", "m"])?;
    assert!(
        out.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let status = repo.mesh_stdout(["status", "m"])?;
    assert!(status.contains("bare edit worked"), "status={status}");
    Ok(())
}

#[test]
fn cli_config_unset_unknown_key_errors() -> Result<()> {
    let repo = TestRepo::seeded()?;
    repo.mesh_stdout(["add", "m", "file1.txt#L1-L5"])?;
    repo.mesh_stdout(["message", "m", "-m", "seed"])?;
    repo.mesh_stdout(["commit", "m"])?;
    let out = repo.run_mesh(["config", "m", "--unset", "nope"])?;
    assert!(!out.status.success());
    Ok(())
}

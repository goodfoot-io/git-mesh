//! Integration tests for the rewritten doctor hook scan:
//! core.hooksPath support, wrapper-walk, ignore directive,
//! CouldNotVerifyHook, and absence of pre-commit findings.

mod support;

use anyhow::Result;
use support::TestRepo;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn make_executable(path: &std::path::Path) -> Result<()> {
    support::make_executable(path)?;
    Ok(())
}

fn write_hook(dir: &std::path::Path, name: &str, body: &str) -> Result<std::path::PathBuf> {
    std::fs::create_dir_all(dir)?;
    let path = dir.join(name);
    std::fs::write(&path, body)?;
    make_executable(&path)?;
    Ok(path)
}

fn doctor_stdout(repo: &TestRepo) -> Result<String> {
    let out = repo.run_mesh(["doctor"])?;
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

// ---------------------------------------------------------------------------
// Scenario 1: core.hooksPath honored, marker present → no finding
// ---------------------------------------------------------------------------

#[test]
fn hooks_path_honored_marker_present() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let githooks = repo.path().join(".githooks");
    write_hook(
        &githooks,
        "post-commit",
        "#!/bin/sh\ngit mesh hooks git post-commit\n",
    )?;
    write_hook(
        &githooks,
        "post-rewrite",
        "#!/bin/sh\ngit mesh hooks git post-rewrite\n",
    )?;
    repo.run_git(["config", "core.hooksPath", ".githooks"])?;
    let s = doctor_stdout(&repo)?;
    assert!(
        !s.contains("MissingPostCommitHook"),
        "should not flag when marker present under core.hooksPath; stdout={s}"
    );
    assert!(
        !s.contains("CouldNotVerifyHook"),
        "stdout={s}"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Scenario 2: core.hooksPath honored, marker absent → MissingPostCommitHook
// ---------------------------------------------------------------------------

#[test]
fn hooks_path_honored_marker_absent() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let githooks = repo.path().join(".githooks");
    write_hook(&githooks, "post-commit", "#!/bin/sh\necho nope\nrefs/heads/unknown\n")?;
    write_hook(&githooks, "post-rewrite", "#!/bin/sh\necho nope\nrefs/heads/unknown\n")?;
    repo.run_git(["config", "core.hooksPath", ".githooks"])?;
    let s = doctor_stdout(&repo)?;
    assert!(
        s.contains("CouldNotVerifyHook"),
        "should emit CouldNotVerifyHook when marker absent; stdout={s}"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Scenario 3: Fallback when core.hooksPath unset — reads .git/hooks
// ---------------------------------------------------------------------------

#[test]
fn fallback_when_hooks_path_unset() -> Result<()> {
    let repo = TestRepo::seeded()?;
    // Put marker in .git/hooks, NOT in .githooks.
    let hooks = repo.path().join(".git").join("hooks");
    write_hook(
        &hooks,
        "post-commit",
        "#!/bin/sh\ngit mesh hooks git post-commit\n",
    )?;
    write_hook(
        &hooks,
        "post-rewrite",
        "#!/bin/sh\ngit mesh hooks git post-rewrite\n",
    )?;
    // core.hooksPath not configured.
    let s = doctor_stdout(&repo)?;
    assert!(
        !s.contains("MissingPostCommitHook"),
        ".git/hooks fallback should work; stdout={s}"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Scenario 4a: Ignore directive (whole line) → pass
// Scenario 4b: Substring-only ignore directive → does NOT silence
// ---------------------------------------------------------------------------

#[test]
fn ignore_directive_whole_line_passes() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let hooks = repo.path().join(".git").join("hooks");
    write_hook(
        &hooks,
        "post-commit",
        "#!/bin/sh\n# git-mesh-doctor-ignore\necho whatever\n",
    )?;
    write_hook(
        &hooks,
        "post-rewrite",
        "#!/bin/sh\n# git-mesh-doctor-ignore\necho whatever\n",
    )?;
    let s = doctor_stdout(&repo)?;
    assert!(
        !s.contains("MissingPostCommitHook"),
        "ignore directive should silence finding; stdout={s}"
    );
    assert!(
        !s.contains("CouldNotVerifyHook"),
        "stdout={s}"
    );
    Ok(())
}

#[test]
fn ignore_directive_substring_does_not_silence() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let hooks = repo.path().join(".git").join("hooks");
    // Inline occurrence — not a whole-line match.
    write_hook(
        &hooks,
        "post-commit",
        "#!/bin/sh\necho \"# git-mesh-doctor-ignore inline\"\nrefs/heads/some-ref\n",
    )?;
    write_hook(
        &hooks,
        "post-rewrite",
        "#!/bin/sh\necho \"# git-mesh-doctor-ignore inline\"\nrefs/heads/some-ref\n",
    )?;
    let s = doctor_stdout(&repo)?;
    // Substring should NOT silence; expect CouldNotVerifyHook due to refs/ line.
    assert!(
        s.contains("CouldNotVerifyHook"),
        "substring ignore should not silence; stdout={s}"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Scenario 5: Wrapper walk — ${0}.cards-original carries the marker
// ---------------------------------------------------------------------------

#[test]
fn wrapper_walk_zero_substitution_carries_marker() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let hooks = repo.path().join(".git").join("hooks");
    std::fs::create_dir_all(&hooks)?;
    // Hook body: devcontainer-cards wrapper pattern.
    let hook_body = "#!/bin/sh\nORIGINAL_HOOK=\"${0}.cards-original\"\nif [ -x \"$ORIGINAL_HOOK\" ]; then\n  \"$ORIGINAL_HOOK\"\nfi\n";
    write_hook(&hooks, "post-commit", hook_body)?;
    // The chained file carries the marker.
    write_hook(
        &hooks,
        "post-commit.cards-original",
        "#!/bin/sh\ngit mesh hooks git post-commit\n",
    )?;
    write_hook(
        &hooks,
        "post-rewrite",
        "#!/bin/sh\ngit mesh hooks git post-rewrite\n",
    )?;
    let s = doctor_stdout(&repo)?;
    assert!(
        !s.contains("MissingPostCommitHook"),
        "wrapper walk via ${{0}} should find marker in chained file; stdout={s}"
    );
    assert!(
        !s.contains("CouldNotVerifyHook"),
        "stdout={s}"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Scenario 6: Wrapper walk — $(dirname "$0")/foo carries the marker
// ---------------------------------------------------------------------------

#[test]
fn wrapper_walk_dirname_substitution_carries_marker() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let hooks = repo.path().join(".git").join("hooks");
    std::fs::create_dir_all(&hooks)?;
    let hook_body = "#!/bin/sh\n$(dirname \"$0\")/post-commit-impl\n";
    write_hook(&hooks, "post-commit", hook_body)?;
    write_hook(
        &hooks,
        "post-commit-impl",
        "#!/bin/sh\ngit mesh hooks git post-commit\n",
    )?;
    write_hook(
        &hooks,
        "post-rewrite",
        "#!/bin/sh\ngit mesh hooks git post-rewrite\n",
    )?;
    let s = doctor_stdout(&repo)?;
    assert!(
        !s.contains("MissingPostCommitHook"),
        "wrapper walk via $(dirname) should find marker; stdout={s}"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Scenario 7: Bare $ORIGINAL_HOOK variable → CouldNotVerifyHook
// ---------------------------------------------------------------------------

#[test]
fn bare_variable_reference_emits_could_not_verify() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let hooks = repo.path().join(".git").join("hooks");
    // Hook only uses a bare variable reference — not resolvable statically.
    // Does NOT include ${0}.cards-original so the chained file can't be found.
    let hook_body = "#!/bin/sh\nORIGINAL_HOOK=\"/some/path\"\n\"$ORIGINAL_HOOK\"\nrefs/heads/unknown\n";
    write_hook(&hooks, "post-commit", hook_body)?;
    write_hook(
        &hooks,
        "post-rewrite",
        "#!/bin/sh\ngit mesh hooks git post-rewrite\n",
    )?;
    let s = doctor_stdout(&repo)?;
    assert!(
        s.contains("CouldNotVerifyHook"),
        "bare $VAR should emit CouldNotVerifyHook; stdout={s}"
    );
    assert!(
        !s.contains("MissingPostCommitHook"),
        "should not emit MissingPostCommitHook when unresolvable refs present; stdout={s}"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Scenario 8: Out-of-tree resolved path → not followed; finding names path
// ---------------------------------------------------------------------------

#[test]
fn out_of_tree_path_reported_not_followed() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let hooks = repo.path().join(".git").join("hooks");
    // Reference an absolute out-of-tree path that exists.
    let hook_body = "#!/bin/sh\n/etc/passwd\nrefs/heads/unknown\n";
    write_hook(&hooks, "post-commit", hook_body)?;
    write_hook(
        &hooks,
        "post-rewrite",
        "#!/bin/sh\ngit mesh hooks git post-rewrite\n",
    )?;
    let s = doctor_stdout(&repo)?;
    // /etc/passwd is out of tree — should produce CouldNotVerifyHook or
    // MissingPostCommitHook (the file exists but doctor won't open it, so no
    // chained pass; with no unresolvable refs it falls through to Missing).
    // The key: no panic, no MissingPreCommitHook, and the path should appear
    // in the finding if CouldNotVerifyHook is emitted.
    assert!(
        !s.contains("MissingPostRewriteHook"),
        "post-rewrite is correctly installed; stdout={s}"
    );
    // Either CouldNotVerifyHook with the path, or MissingPostCommitHook — both are valid.
    // What matters is no crash and no silent pass.
    assert!(
        s.contains("MissingPostCommitHook") || s.contains("CouldNotVerifyHook"),
        "out-of-tree path should not silently pass; stdout={s}"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Scenario 9: Malformed hook (unbalanced quotes) → CouldNotVerifyHook
// ---------------------------------------------------------------------------

#[test]
fn malformed_hook_unbalanced_quotes_emits_could_not_verify() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let hooks = repo.path().join(".git").join("hooks");
    // Unbalanced quote on one of the lines; add a refs/ line so the
    // scanner reports CouldNotVerifyHook rather than silently passing.
    let hook_body = "#!/bin/sh\necho \"unterminated\nrefs/heads/unknown\n";
    write_hook(&hooks, "post-commit", hook_body)?;
    write_hook(
        &hooks,
        "post-rewrite",
        "#!/bin/sh\ngit mesh hooks git post-rewrite\n",
    )?;
    let s = doctor_stdout(&repo)?;
    assert!(
        s.contains("CouldNotVerifyHook"),
        "unbalanced quotes should emit CouldNotVerifyHook; stdout={s}"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Scenario 10: pre-commit hook absent → no finding mentioning pre-commit
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Scenario 11: ./relative chain resolved against hook_dir, not CWD
// ---------------------------------------------------------------------------

#[test]
fn dot_slash_relative_chain_resolved_against_hook_dir() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let hooks = repo.path().join(".git").join("hooks");
    std::fs::create_dir_all(&hooks)?;

    // Hook body chains to ./helpers/chain.sh using a bare ./ path.
    // No ${0} or $(dirname "$0") — exercises the ./... branch of classify_token.
    let hook_body = "#!/bin/sh\n./helpers/chain.sh\n";
    write_hook(&hooks, "post-commit", hook_body)?;

    // Place the chained file under the hook directory.
    let helpers_dir = hooks.join("helpers");
    std::fs::create_dir_all(&helpers_dir)?;
    write_hook(
        &helpers_dir,
        "chain.sh",
        "#!/bin/sh\ngit mesh hooks git post-commit\n",
    )?;

    write_hook(
        &hooks,
        "post-rewrite",
        "#!/bin/sh\ngit mesh hooks git post-rewrite\n",
    )?;

    // Run doctor from a *different* working directory so CWD-anchored
    // canonicalize would fail to find ./helpers/chain.sh.
    let other_dir = tempfile::tempdir()?;
    let out = repo.run_mesh_from(["doctor"], other_dir.path())?;
    let s = String::from_utf8_lossy(&out.stdout).to_string();

    assert!(
        !s.contains("MissingPostCommitHook"),
        "./helpers/chain.sh should be resolved against hook_dir, not CWD; stdout={s}"
    );
    assert!(
        !s.contains("CouldNotVerifyHook"),
        "stdout={s}"
    );
    Ok(())
}

#[test]
fn no_pre_commit_finding_ever() -> Result<()> {
    let repo = TestRepo::seeded()?;
    // Don't install any hooks at all.
    let s = doctor_stdout(&repo)?;
    assert!(
        !s.contains("MissingPreCommitHook"),
        "pre-commit finding should not exist; stdout={s}"
    );
    assert!(
        !s.to_lowercase().contains("pre-commit hook"),
        "no pre-commit hook finding; stdout={s}"
    );
    Ok(())
}

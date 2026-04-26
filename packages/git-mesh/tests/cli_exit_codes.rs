//! Exit-code convention tests.
//!
//! `git-mesh` follows the POSIX/`git`/`cargo` convention:
//!
//! - 0 — success
//! - 1 — operational failure (well-formed command, environment or
//!   state prevents completion: missing remote, missing mesh, nothing
//!   staged, …)
//! - 2 — usage error (clap rejected the argv: bad flag, missing
//!   required arg, unknown subcommand)
//!
//! The split lives in `packages/git-mesh/src/main.rs`: the dispatch
//! wrapper downcasts `anyhow::Error` to `clap::Error` and lets clap's
//! own `.exit()` produce code 2; everything else maps to code 1.

mod support;

use anyhow::Result;
use support::TestRepo;

#[test]
fn fetch_runtime_vs_usage() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let runtime = repo.run_mesh(["fetch", "absent"])?;
    assert_eq!(runtime.status.code(), Some(1), "runtime missing-remote");

    let usage = repo.run_mesh(["fetch", "--bogus"])?;
    assert_eq!(usage.status.code(), Some(2), "clap usage error");
    Ok(())
}

#[test]
fn push_runtime_vs_usage() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let runtime = repo.run_mesh(["push", "absent"])?;
    assert_eq!(runtime.status.code(), Some(1));

    let usage = repo.run_mesh(["push", "--bogus"])?;
    assert_eq!(usage.status.code(), Some(2));
    Ok(())
}

#[test]
fn delete_missing_mesh_exits_one() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let out = repo.run_mesh(["delete", "never-existed"])?;
    assert_eq!(out.status.code(), Some(1));
    Ok(())
}

#[test]
fn commit_with_nothing_staged_exits_one() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let out = repo.run_mesh(["commit", "no-such-mesh"])?;
    assert_eq!(out.status.code(), Some(1));
    Ok(())
}

#[test]
fn unknown_subcommand_is_runtime_show_failure() -> Result<()> {
    // Bare `git mesh <name>` routes to `show <name>`; an unknown
    // mesh name is an operational failure (exit 1), not a usage
    // error — clap accepted the argv.
    let repo = TestRepo::seeded()?;
    let out = repo.run_mesh(["definitely-not-a-mesh"])?;
    assert_eq!(out.status.code(), Some(1));
    Ok(())
}

#[test]
fn help_exits_zero() -> Result<()> {
    // `--help` / `--version` are clap-handled and exit 0 via
    // `clap::Error::exit()` — the wrapper must not redirect them
    // through the runtime exit-1 path.
    let repo = TestRepo::seeded()?;
    let help = repo.run_mesh(["--help"])?;
    assert_eq!(help.status.code(), Some(0));
    let version = repo.run_mesh(["--version"])?;
    assert_eq!(version.status.code(), Some(0));
    Ok(())
}

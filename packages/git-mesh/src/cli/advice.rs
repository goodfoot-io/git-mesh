//! `git mesh advice` subcommand — session-scoped advice stream.

use anyhow::Result;
use clap::{ArgGroup, Subcommand};
use serde_json::json;

use crate::advice;

#[derive(Debug, clap::Args)]
pub struct AdviceArgs {
    /// Session identifier (used to isolate per-session state).
    pub session_id: String,

    #[command(subcommand)]
    pub command: Option<AdviceCommand>,

    /// Append per-reason documentation blocks to the flush output.
    #[arg(long)]
    pub documentation: bool,
}

#[derive(Debug, Subcommand)]
pub enum AdviceCommand {
    /// Append a typed event to the session store.
    Add(AdviceAddArgs),
}

#[derive(Debug, clap::Args)]
#[command(group(ArgGroup::new("kind").required(true)))]
pub struct AdviceAddArgs {
    /// Record a read event for the given path (optionally range-qualified).
    #[arg(long, group = "kind", value_name = "PATH[#Ls-Le]")]
    pub read: Option<String>,

    /// Record a write event for the given path (optionally range-qualified).
    #[arg(long, group = "kind", value_name = "PATH[#Ls-Le]")]
    pub write: Option<String>,

    /// Record a commit event for the given SHA.
    #[arg(long, group = "kind", value_name = "SHA")]
    pub commit: Option<String>,

    /// Record a snapshot event (captures current tree and index state).
    #[arg(long, group = "kind")]
    pub snapshot: bool,
}

/// Top-level entry: dispatches to `add` or, when no subcommand is given,
/// runs the flush pipeline and prints the rendered advice.
pub fn run_advice(repo: &gix::Repository, args: AdviceArgs) -> Result<i32> {
    match args.command {
        Some(AdviceCommand::Add(add_args)) => run_advice_add(repo, &args.session_id, add_args),
        None => run_advice_flush(repo, &args.session_id, args.documentation),
    }
}

/// Open the session store, append the requested event, and exit silently.
///
/// On success: no stdout, no stderr, exit 0. On failure: error bubbles up
/// to the CLI boundary which prints `error: <msg>` to stderr and exits 2
/// (loud, fail-closed for direct callers — the bash shims wrap the call
/// with `|| true`, see Phase 5).
pub fn run_advice_add(
    repo: &gix::Repository,
    session_id: &str,
    args: AdviceAddArgs,
) -> Result<i32> {
    let conn = advice::open_store(session_id)?;

    let audit_line = if let Some(spec) = args.read.as_deref() {
        advice::append_read(&conn, repo, spec)?;
        json!({ "kind": "read", "spec": spec })
    } else if let Some(spec) = args.write.as_deref() {
        advice::append_write(&conn, repo, spec)?;
        json!({ "kind": "write", "spec": spec })
    } else if let Some(sha) = args.commit.as_deref() {
        advice::append_commit(&conn, repo, sha)?;
        json!({ "kind": "commit", "sha": sha })
    } else if args.snapshot {
        advice::append_snapshot(&conn, repo)?;
        json!({ "kind": "snapshot" })
    } else {
        // Clap's ArgGroup(required=true) prevents this branch.
        anyhow::bail!("git mesh advice add: one of --read/--write/--commit/--snapshot is required");
    };

    let sanitized = advice::sanitize_session_id(session_id);
    let jsonl = advice::db::jsonl_path(&sanitized);
    advice::audit::append_jsonl(&jsonl, &audit_line)?;

    Ok(0)
}

/// Open the session store, run the flush pipeline, and print the rendered
/// advice (only when non-empty). Records a JSONL audit line for the flush.
fn run_advice_flush(repo: &gix::Repository, session_id: &str, documentation: bool) -> Result<i32> {
    let mut conn = advice::open_store(session_id)?;
    let rendered = advice::run_flush(&mut conn, repo, documentation)?;
    if !rendered.is_empty() {
        // Render output already carries its own trailing newlines per line;
        // print exactly what flush produced.
        print!("{rendered}");
    }

    let sanitized = advice::sanitize_session_id(session_id);
    let jsonl = advice::db::jsonl_path(&sanitized);
    let line = json!({
        "kind": "flush",
        "documentation": documentation,
        "output_len": rendered.len(),
    });
    advice::audit::append_jsonl(&jsonl, &line)?;

    Ok(0)
}

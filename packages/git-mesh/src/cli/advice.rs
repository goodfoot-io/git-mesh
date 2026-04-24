//! `git mesh advice` subcommand — session-scoped advice stream.

use clap::{ArgGroup, Subcommand};

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

pub fn run_advice(_repo: &gix::Repository, _args: AdviceArgs) -> anyhow::Result<i32> {
    Ok(0)
}

pub fn run_advice_add(
    _repo: &gix::Repository,
    _session_id: &str,
    _args: AdviceAddArgs,
) -> anyhow::Result<i32> {
    Ok(0)
}

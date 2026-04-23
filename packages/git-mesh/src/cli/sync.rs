//! `git mesh fetch` / `git mesh push` — §7.

use crate::cli::{FetchArgs, PushArgs};
use crate::sync::{default_remote, fetch_mesh_refs, push_mesh_refs};
use anyhow::Result;

pub fn run_fetch(repo: &gix::Repository, args: FetchArgs) -> Result<i32> {
    let remote = match args.remote {
        Some(r) => r,
        None => default_remote(repo)?,
    };
    fetch_mesh_refs(repo, &remote)?;
    println!("fetched mesh refs from {remote}");
    Ok(0)
}

pub fn run_push(repo: &gix::Repository, args: PushArgs) -> Result<i32> {
    let remote = match args.remote {
        Some(r) => r,
        None => default_remote(repo)?,
    };
    push_mesh_refs(repo, &remote)?;
    println!("pushed mesh refs to {remote}");
    Ok(0)
}

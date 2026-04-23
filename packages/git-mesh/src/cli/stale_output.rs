//! `git mesh stale` output rendering — §10.4.
//!
//! Phase 1 types slice: the renderer is rewritten around `Finding` /
//! `PendingFinding` in the renderer slice (see
//! `docs/stale-layers-plan.md` §Phase 1 "Renderers"). For now, the
//! public entry points remain to preserve the CLI dispatch wiring; the
//! rendering body is stubbed with `todo!()`. `format_relative` is still
//! used by `cli/show.rs` and keeps its real implementation.

#![allow(unused_variables, dead_code)]

use crate::cli::StaleArgs;
use anyhow::Result;

pub fn run_stale(_repo: &gix::Repository, _args: StaleArgs) -> Result<i32> {
    todo!("stale renderer is rewritten atop Finding/PendingFinding in the renderer slice")
}

pub(crate) fn format_relative(committer_time: i64) -> String {
    let now = chrono::Utc::now().timestamp();
    let diff = now - committer_time;
    if diff < 0 {
        return "in the future".into();
    }
    let secs = diff;
    let mins = secs / 60;
    let hours = mins / 60;
    let days = hours / 24;
    let weeks = days / 7;
    let months = days / 30;
    let years = days / 365;
    if years > 0 {
        format!("{years} year{} ago", plural(years))
    } else if months > 0 {
        format!("{months} month{} ago", plural(months))
    } else if weeks > 0 {
        format!("{weeks} week{} ago", plural(weeks))
    } else if days > 0 {
        format!("{days} day{} ago", plural(days))
    } else if hours > 0 {
        format!("{hours} hour{} ago", plural(hours))
    } else if mins > 0 {
        format!("{mins} minute{} ago", plural(mins))
    } else {
        format!("{secs} second{} ago", plural(secs))
    }
}

fn plural(n: i64) -> &'static str {
    if n == 1 { "" } else { "s" }
}

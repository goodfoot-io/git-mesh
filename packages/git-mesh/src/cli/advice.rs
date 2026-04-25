//! `git mesh advice` subcommand — session-scoped advice stream.

use anyhow::{Result, bail};
use clap::{ArgGroup, Subcommand};
use serde_json::json;

use crate::advice;
use crate::git::work_dir;

/// Allowed character set for `<sessionId>`, documented in error messages
/// and clap help. Path separators (`/`, `\`), NUL, and ASCII control
/// characters are forbidden so the id maps unambiguously to a single
/// `<sessionDir>/<id>.{db,jsonl}` filename without collision rewrites.
const SESSION_ID_RULE: &str =
    "non-empty; ASCII letters, digits, `-`, `_`, and `.`; \
     no `/`, no `\\`, no NUL, no whitespace or other control characters";

#[derive(Debug, clap::Args)]
pub struct AdviceArgs {
    /// Session identifier (used to isolate per-session state).
    ///
    /// Allowed characters: ASCII letters, digits, `-`, `_`, and `.`.
    /// Path separators (`/`, `\`), NUL, whitespace, and other control
    /// characters are rejected — the id becomes a filename component
    /// under the per-session state directory and silent rewrites would
    /// collide distinct ids onto the same store.
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
    validate_session_id(&args.session_id)?;
    match args.command {
        Some(AdviceCommand::Add(add_args)) => run_advice_add(repo, &args.session_id, add_args),
        None => run_advice_flush(repo, &args.session_id, args.documentation),
    }
}

/// Reject session ids that would silently collide on disk or escape the
/// per-session directory. See `SESSION_ID_RULE`.
fn validate_session_id(id: &str) -> Result<()> {
    if id.is_empty() {
        bail!("invalid <sessionId>: must not be empty ({SESSION_ID_RULE})");
    }
    for ch in id.chars() {
        let ok = ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.');
        if !ok {
            bail!(
                "invalid <sessionId> `{id}`: contains disallowed character `{}` ({SESSION_ID_RULE})",
                ch.escape_debug()
            );
        }
    }
    if id == "." || id == ".." {
        bail!("invalid <sessionId> `{id}`: reserved path component ({SESSION_ID_RULE})");
    }
    Ok(())
}

/// Reject `--read` / `--write` specs that point at non-existent paths or
/// out-of-range / inverted line ranges. Path existence is resolved
/// relative to the worktree root.
fn validate_read_write_spec(repo: &gix::Repository, spec: &str) -> Result<()> {
    if spec.is_empty() {
        bail!("invalid spec: path must not be empty");
    }
    let (path_str, range) = match spec.split_once("#L") {
        Some((p, frag)) => {
            let (s, e) = frag.split_once("-L").ok_or_else(|| {
                anyhow::anyhow!(
                    "invalid range `{spec}`; expected <path>#L<start>-L<end>"
                )
            })?;
            let start: u32 = s
                .parse()
                .map_err(|_| anyhow::anyhow!("invalid range start in `{spec}`"))?;
            let end: u32 = e
                .parse()
                .map_err(|_| anyhow::anyhow!("invalid range end in `{spec}`"))?;
            if start < 1 {
                bail!("invalid range `{spec}`: start must be at least 1");
            }
            if end < start {
                bail!("invalid range `{spec}`: end ({end}) is before start ({start})");
            }
            (p, Some((start, end)))
        }
        None => (spec, None),
    };
    if path_str.is_empty() {
        bail!("invalid spec `{spec}`: path must not be empty");
    }
    let wd = work_dir(repo)?;
    let abs = wd.join(path_str);
    if !abs.exists() {
        bail!("path not found in worktree: `{path_str}`");
    }
    if let Some((start, end)) = range {
        // For ranges, count lines in the current worktree file.
        let bytes = std::fs::read(&abs)
            .map_err(|e| anyhow::anyhow!("read `{path_str}`: {e}"))?;
        let line_count = String::from_utf8_lossy(&bytes).lines().count() as u32;
        if end > line_count {
            bail!(
                "invalid range `{spec}`: end ({end}) is past EOF (file has {line_count} lines)"
            );
        }
        // start <= end already verified above; start <= line_count follows when end <= line_count
        let _ = start;
    }
    Ok(())
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
    // Validate read/write specs before opening the store so a malformed
    // call does not create state files. Fail-closed per CLAUDE.md.
    if let Some(spec) = args.read.as_deref() {
        validate_read_write_spec(repo, spec)?;
    }
    if let Some(spec) = args.write.as_deref() {
        validate_read_write_spec(repo, spec)?;
    }

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

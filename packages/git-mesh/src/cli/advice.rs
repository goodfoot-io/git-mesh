//! `git mesh advice` subcommand — session-scoped advice stream.

use anyhow::{Result, bail};
use clap::{ArgGroup, Subcommand};

use crate::advice;
use crate::advice::CONTENT_BYTE_CAP;
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

    /// Debug helper: regenerate the JSONL audit log from the SQL store.
    ///
    /// Truncates `<sessionDir>/<id>.jsonl` and replays every row in
    /// `events` (ordered by id) as a canonical audit line. The result is
    /// byte-identical to the live JSONL — see `docs/advice-notes.md` §7.
    #[arg(long)]
    pub rebuild_audit_from_db: bool,
}

#[derive(Debug, Subcommand)]
pub enum AdviceCommand {
    /// Append a typed event to the session store.
    Add(AdviceAddArgs),
    /// Capture the current workspace tree into the file-backed session store.
    Snapshot,
    /// Record one or more read events in the file-backed session store.
    Read {
        /// Paths (optionally range-qualified) to record as reads.
        paths: Vec<String>,
    },
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

    /// Pre-edit content for `--write`. `<PATH>` reads from a file. The
    /// pre side is not stdin-eligible (only `--post` is) so two stdin
    /// inputs cannot collide. Maximum size: 1 MiB. Must be valid UTF-8.
    #[arg(long, value_name = "PATH", requires = "write")]
    pub pre: Option<String>,

    /// Post-edit content for `--write`. `<PATH>` reads from a file; `-`
    /// reads from stdin. Maximum size: 1 MiB. Must be valid UTF-8. The
    /// post line count is intentionally NOT used as an upper bound on
    /// the `--write` range — that range describes the pre extent (the
    /// bytes about to be overwritten); a post that is shorter is the
    /// signal T4 ("range collapse") consumes.
    #[arg(long, value_name = "PATH-or--", requires = "write")]
    pub post: Option<String>,

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
    if args.rebuild_audit_from_db {
        if args.command.is_some() {
            bail!("--rebuild-audit-from-db cannot be combined with `add`");
        }
        if args.documentation {
            bail!("--rebuild-audit-from-db cannot be combined with --documentation");
        }
        return run_rebuild_audit(&args.session_id);
    }
    match args.command {
        Some(AdviceCommand::Add(add_args)) => run_advice_add(repo, &args.session_id, add_args),
        Some(AdviceCommand::Snapshot) => run_advice_snapshot(args.session_id),
        Some(AdviceCommand::Read { paths }) => run_advice_read(args.session_id, paths),
        None => run_advice_flush(repo, &args.session_id, args.documentation),
    }
}

/// Capture the current workspace tree into the file-backed session store.
#[allow(dead_code)]
fn run_advice_snapshot(_session_id: String) -> Result<i32> {
    unimplemented!()
}

/// Record read events in the file-backed session store.
#[allow(dead_code)]
fn run_advice_read(_session_id: String, _paths: Vec<String>) -> Result<i32> {
    unimplemented!()
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

/// Read a `--pre` / `--post` argument. `-` is stdin (post only); any
/// other value is a filesystem path. Enforces the 1 MiB cap and UTF-8.
fn read_content_arg(arg: &str, who: &str, allow_stdin: bool) -> Result<String> {
    let bytes = if arg == "-" {
        if !allow_stdin {
            bail!(
                "{who}: stdin (`-`) is not accepted; pass a file path. \
                 Convention: only --post may read from stdin to avoid \
                 ambiguous two-stdin inputs."
            );
        }
        let mut buf = Vec::new();
        std::io::Read::read_to_end(&mut std::io::stdin(), &mut buf)
            .map_err(|e| anyhow::anyhow!("{who}: read stdin: {e}"))?;
        buf
    } else {
        std::fs::read(arg).map_err(|e| anyhow::anyhow!("{who}: read `{arg}`: {e}"))?
    };
    if bytes.len() > CONTENT_BYTE_CAP {
        bail!(
            "{who}: content is {} bytes, exceeds the {} byte cap (1 MiB). \
             Larger writes are out of advice's scope.",
            bytes.len(),
            CONTENT_BYTE_CAP
        );
    }
    String::from_utf8(bytes)
        .map_err(|_| anyhow::anyhow!("{who}: content is not valid UTF-8"))
}

/// Reject `--read` / `--write` specs that point at non-existent paths or
/// out-of-range / inverted line ranges. When `pre_line_count` is supplied,
/// it overrides the worktree-line bound for range validation: a `--write`
/// range describes the bytes that were *overwritten* (the pre extent),
/// not the post bytes. The `--post` line count is intentionally NOT a
/// bound — T4 ("range collapse") is exactly the case where the post
/// extent is smaller than the recorded `--write` range.
fn validate_read_write_spec(
    repo: &gix::Repository,
    spec: &str,
    pre_line_count: Option<u32>,
) -> Result<()> {
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
        let line_count = if let Some(n) = pre_line_count {
            n
        } else {
            let bytes = std::fs::read(&abs)
                .map_err(|e| anyhow::anyhow!("read `{path_str}`: {e}"))?;
            String::from_utf8_lossy(&bytes).lines().count() as u32
        };
        if end > line_count {
            bail!(
                "invalid range `{spec}`: end ({end}) is past EOF (extent has {line_count} lines)"
            );
        }
        let _ = start;
    }
    Ok(())
}

/// Open the session store, append the requested event, and exit silently.
///
/// Order is **SQL first, audit append second**: a failure of the audit
/// append surfaces as an error to the caller (fail-closed), but the
/// audit log can never be ahead of SQL.
pub fn run_advice_add(
    repo: &gix::Repository,
    session_id: &str,
    args: AdviceAddArgs,
) -> Result<i32> {
    // For --write, materialize pre/post content first so range validation
    // can use the post line count.
    let mut pre_content: Option<String> = None;
    let mut post_content: Option<String> = None;
    if args.write.is_some() {
        if let Some(p) = args.pre.as_deref() {
            pre_content = Some(read_content_arg(p, "--pre", false)?);
        }
        if let Some(p) = args.post.as_deref() {
            post_content = Some(read_content_arg(p, "--post", true)?);
        }
    } else if args.pre.is_some() || args.post.is_some() {
        // Clap's `requires = "write"` already prevents this; defense in depth.
        bail!("--pre / --post are only valid with --write");
    }

    if let Some(spec) = args.read.as_deref() {
        validate_read_write_spec(repo, spec, None)?;
    }
    if let Some(spec) = args.write.as_deref() {
        // Bound by --pre line count if supplied (the range describes the
        // PRE extent — the bytes about to be overwritten). --post is
        // intentionally not a bound: T4 ("range collapse") relies on the
        // post being shorter than the recorded write range.
        let pre_lines = pre_content
            .as_ref()
            .map(|s| s.lines().count() as u32);
        validate_read_write_spec(repo, spec, pre_lines)?;
    }

    let conn = advice::open_store(session_id)?;

    let record = if let Some(spec) = args.read.as_deref() {
        advice::append_read(&conn, repo, spec)?
    } else if let Some(spec) = args.write.as_deref() {
        advice::append_write(&conn, repo, spec, pre_content, post_content)?
    } else if let Some(sha) = args.commit.as_deref() {
        advice::append_commit(&conn, repo, sha)?
    } else if args.snapshot {
        advice::append_snapshot(&conn, repo)?
    } else {
        // Clap's ArgGroup(required=true) prevents this branch.
        anyhow::bail!("git mesh advice add: one of --read/--write/--commit/--snapshot is required");
    };

    let sanitized = advice::sanitize_session_id(session_id);
    let jsonl = advice::db::jsonl_path(&sanitized);
    advice::audit::append_record(&jsonl, &record)?;

    Ok(0)
}

/// Open the session store, run the flush pipeline, and print the rendered
/// advice (only when non-empty). Records a JSONL audit line for the flush
/// using the canonical payload from `events.payload`.
fn run_advice_flush(repo: &gix::Repository, session_id: &str, documentation: bool) -> Result<i32> {
    let mut conn = advice::open_store(session_id)?;
    let (rendered, flush_record) = advice::run_flush(&mut conn, repo, documentation)?;
    if !rendered.is_empty() {
        print!("{rendered}");
    }

    let sanitized = advice::sanitize_session_id(session_id);
    let jsonl = advice::db::jsonl_path(&sanitized);
    advice::audit::append_record(&jsonl, &flush_record)?;

    Ok(0)
}

/// Regenerate the JSONL audit log deterministically from the SQL store.
fn run_rebuild_audit(session_id: &str) -> Result<i32> {
    let conn = advice::open_store(session_id)?;
    let sanitized = advice::sanitize_session_id(session_id);
    let jsonl = advice::db::jsonl_path(&sanitized);
    advice::audit::rebuild_from_db(&conn, &jsonl)?;
    Ok(0)
}

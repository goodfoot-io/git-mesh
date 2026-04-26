//! CLI top-level — parses args and dispatches to library functions.
//!
//! Design choices:
//!
//! * **`anyhow::Result<i32>` at the CLI boundary.** CLI handlers return
//!   `anyhow::Result<i32>` so exit codes are first-class (§10.4
//!   distinguishes `0`, `1`, `2` for `git mesh stale`). Library errors
//!   (`crate::Error`) convert via `?`; `anyhow` keeps the dispatch
//!   layer from having to enumerate variants.
//!
//! * **`git mesh <name>` vs `git mesh <subcommand>`.** Clap cannot
//!   disambiguate a positional-name from a subcommand without help.
//!   We handle this in [`crate::main`] by checking the first argument
//!   against [`crate::validation::RESERVED_MESH_NAMES`] (the spec's
//!   reserved list, §10.2) before parsing. A reserved token is treated
//!   as a subcommand; anything else is a mesh name passed to the
//!   `Show` handler.

pub mod advice;
pub mod commit;
pub mod pre_commit;
pub mod show;
pub mod stale_output;
pub mod structural;
pub mod sync;

use clap::{Parser, Subcommand, ValueEnum};

/// Top-level `git-mesh` command.
#[derive(Debug, Parser)]
#[command(
    name = "git-mesh",
    about = "Track implicit semantic dependencies in a git repo.",
    version,
    after_help = "A mesh anchors the line ranges (or whole files) — in code or prose — that participate in a coupling no schema, type, or test enforces, and carries a `why` that names the relationship between them in one sentence: what they form, what one promises, what one governs, what one cites.\n\nBare invocations:\n  git mesh                 list every mesh in the repo\n  git mesh <name>          show one mesh (ranges, why, config)"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Commands>,
}

/// Every subcommand the CLI accepts. Mirrors §10.2.
#[derive(Debug, Subcommand)]
pub enum Commands {
    /// Show the named mesh (like `git show`). This variant is also
    /// used by [`crate::main`] to handle the bare `git mesh <name>`
    /// positional form.
    #[command(name = "show", hide = true)]
    Show(ShowArgs),

    /// List files and ranges currently tracked by a mesh.
    Ls(LsArgs),

    /// Report ranges whose content has drifted from their anchored state.
    Stale(StaleArgs),

    /// Stage ranges to add on the next mesh commit.
    Add(AddArgs),

    /// Stage ranges to remove on the next mesh commit.
    Rm(RmArgs),

    /// Read or stage the mesh's why — the durable one-sentence
    /// definition of the relationship the anchored ranges hold.
    ///
    /// Name the relationship: what the ranges form together, what
    /// one promises, what one governs, what one cites. Write it so
    /// it survives a rewrite of either side. The why is prose: the
    /// mesh name carries the label, so don't restate the name as a
    /// prefix or use a git-style leading keyword (`contract:`,
    /// `spec:`, `gov:`). The ranges carry the paths; describe the
    /// relationship in role-words ("the doc," "the parser," "the
    /// runbook," "the migration") rather than repeating filenames.
    /// Name a path only when the path itself is part of the
    /// dependency (a hard-coded script reference, a generated file
    /// invoked by name). For asymmetric relationships, name which
    /// side is normative in prose ("the doc is the source of truth
    /// when they disagree," "X promises the shape Y honors"). Avoid
    /// restating the diff, embedding incidental implementation
    /// properties (parser strictness, current field names), or
    /// bundling ownership and review triggers. If you're stuck,
    /// reach for vocabulary like subsystem, specification,
    /// mechanism, consumer role, or contract — but the rule is one
    /// prose sentence, stable across implementation churn at either
    /// anchor.
    ///
    /// Bare `git mesh why <name>` prints the current why; the writer
    /// flags `-m`/`-F`/`--edit` stage a new one.
    Why(WhyArgs),

    /// Resolve staged operations and write a mesh commit.
    Commit(CommitArgs),

    /// Clear the staging area.
    Restore(RestoreArgs),

    /// Fast-forward a mesh to a past state.
    Revert(RevertArgs),

    /// Delete a mesh ref.
    Delete(DeleteArgs),

    /// Rename a mesh ref.
    Mv(MvArgs),

    /// Read or stage mesh-level resolver options.
    Config(ConfigArgs),

    /// Fetch mesh and range refs from a remote.
    Fetch(FetchArgs),

    /// Push mesh and range refs to a remote.
    Push(PushArgs),

    /// Audit the local mesh setup.
    Doctor(DoctorArgs),

    /// Fail the current commit if any drift is visible in the staged tree.
    #[command(name = "pre-commit")]
    PreCommit(PreCommitArgs),

    /// Append events and flush session-scoped advice.
    Advice(advice::AdviceArgs),
}

/// `git mesh <name>` / `git mesh show <name>`.
#[derive(Debug, clap::Args)]
pub struct ShowArgs {
    /// Mesh name. Required (the bare `git mesh` form with no name is
    /// handled by the `Commands::None` branch in `main`, which lists
    /// every mesh).
    pub name: String,

    /// One line per Range, no commit header.
    #[arg(long)]
    pub oneline: bool,

    /// Format-string override. Supported placeholders:
    ///
    /// Commit-level (one line per mesh commit):
    ///   %H   full mesh commit SHA
    ///   %h   abbreviated mesh commit SHA (7 chars)
    ///   %an  author name
    ///   %ae  author email
    ///   %ad  author date (RFC 2822)
    ///   %ar  author date, relative
    ///   %s   subject (first line of message)
    ///
    /// Per-range (one line per range when any of these is present):
    ///   %p   range path
    ///   %r   range extent (#L<start>-L<end>, or empty for whole-file)
    ///   %P   path + extent (path#L<start>-L<end>, or just path for whole-file)
    ///   %a   anchor SHA (full 40 chars)
    ///   %A   anchor SHA (abbreviated 8 chars; full when --no-abbrev is set)
    ///
    /// Special: %% → literal %; %n → newline.
    ///
    /// Unknown placeholders are rejected with exit code 2.
    #[arg(long, value_name = "FMT")]
    pub format: Option<String>,

    /// Full 40-char anchor shas.
    #[arg(long)]
    pub no_abbrev: bool,

    /// Show state at a past commit. Accepts either a source commit-ish
    /// (e.g. HEAD~3, a branch, a source SHA) — which selects the mesh
    /// state that was current at that source commit — or a mesh-ref
    /// commit SHA directly.
    #[arg(long, value_name = "COMMIT-ISH")]
    pub at: Option<String>,

    /// Walk the mesh's commit history instead of showing the tip.
    #[arg(long)]
    pub log: bool,

    /// Cap the `--log` walk.
    #[arg(long, value_name = "N", requires = "log")]
    pub limit: Option<usize>,
}

#[derive(Debug, clap::Args)]
pub struct LsArgs {
    /// Optional `<path>` or `<path>#L<start>-L<end>` to filter by.
    pub target: Option<String>,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, ValueEnum)]
#[value(rename_all = "kebab-case")]
pub enum StaleFormat {
    Human,
    Porcelain,
    Json,
    Junit,
    GithubActions,
}

#[derive(Debug, clap::Args)]
pub struct StaleArgs {
    /// Optional mesh name; omit for a workspace-wide scan.
    pub name: Option<String>,

    #[arg(long, value_enum, default_value_t = StaleFormat::Human)]
    pub format: StaleFormat,

    /// Exit 0 even when drift is found (report-only mode).
    #[arg(long)]
    pub no_exit_code: bool,

    /// Skip the working-tree layer; scan only HEAD (and the index unless `--no-index`).
    #[arg(long)]
    pub no_worktree: bool,

    /// Skip the index layer.
    #[arg(long)]
    pub no_index: bool,

    /// Skip the staged-mesh layer (`.git/mesh/staging/`).
    #[arg(long)]
    pub no_staged_mesh: bool,

    /// Report unreadable content as informational instead of failing.
    #[arg(long)]
    pub ignore_unavailable: bool,

    /// One line per finding: `<STATUS> <path>#L<start>-L<end>`.
    #[arg(long, conflicts_with_all = ["stat", "patch"])]
    pub oneline: bool,

    /// Per-range summary with line counts added/removed relative to the anchor.
    #[arg(long, conflicts_with_all = ["oneline", "patch"])]
    pub stat: bool,

    /// Show the diff between the anchored content and the current content.
    #[arg(long, conflicts_with_all = ["oneline", "stat"])]
    pub patch: bool,

    /// Only ranges anchored at or after this commit.
    #[arg(long, value_name = "COMMIT-ISH")]
    pub since: Option<String>,
}

#[derive(Debug, clap::Args)]
pub struct PreCommitArgs {
    /// Exit 0 even when drift is found (report-only mode).
    #[arg(long)]
    pub no_exit_code: bool,
}

#[derive(Debug, clap::Args)]
pub struct AddArgs {
    /// Mesh name to stage into.
    pub name: String,

    // Annotated `trailing_var_arg = false` + `allow_hyphen_values = false`
    // so a trailing `--at <commit-ish>` is parsed as the named flag,
    // not greedily consumed into `ranges`.
    #[arg(
        required = true,
        trailing_var_arg = false,
        allow_hyphen_values = false,
        help = "One or more targets to stage (<path> for whole-file, or <path>#L<start>-L<end> for line range)",
        long_help = "One or more targets to stage. Each is either:\n  <path>                       whole-file pin\n  <path>#L<start>-L<end>       line range (1-indexed, inclusive)\n\nExample: git mesh add api-contract src/api.ts#L1-L3 tests/api.test.ts"
    )]
    pub ranges: Vec<String>,

    /// Anchor every staged range in this invocation at `<commit-ish>`.
    /// Default is HEAD resolved at commit time.
    #[arg(long, value_name = "COMMIT-ISH")]
    pub at: Option<String>,
}

#[derive(Debug, clap::Args)]
pub struct RmArgs {
    /// Mesh to stage the removal into.
    pub name: String,

    /// Range(s) to remove, as `<path>` or `<path>#L<start>-L<end>`
    /// (must match an existing range on the mesh).
    #[arg(required = true)]
    pub ranges: Vec<String>,
}

#[derive(Debug, clap::Args)]
#[command(group(
    clap::ArgGroup::new("source")
        .args(["m", "file", "edit"])
        .required(false)
        .multiple(false)
))]
pub struct WhyArgs {
    /// Mesh whose why text to read (no writer flag) or stage
    /// (`-m` / `-F` / `--edit`). The why names the relationship the
    /// anchored ranges hold.
    pub name: String,

    /// Inline why text (`-m "..."`). Writer flag. One sentence
    /// naming the relationship the ranges hold; stable across
    /// implementation churn at either anchor.
    #[arg(short = 'm', value_name = "MSG")]
    pub m: Option<String>,

    /// Read why text from a file (`-F <file>`). Writer flag.
    #[arg(short = 'F', value_name = "FILE")]
    pub file: Option<String>,

    /// Open `$EDITOR` on a pre-populated template. Writer flag.
    #[arg(long, conflicts_with = "at")]
    pub edit: bool,

    /// Reader-only: print the why text as of a past commit. Accepts
    /// either a source commit-ish (e.g. HEAD~3, a branch, a source SHA)
    /// — which selects the mesh state that was current at that source
    /// commit — or a mesh-ref commit SHA directly. Mutually exclusive
    /// with `-m`/`-F`/`--edit`.
    #[arg(long, value_name = "COMMIT-ISH", conflicts_with_all = ["m", "file", "edit"])]
    pub at: Option<String>,
}

#[derive(Debug, clap::Args)]
pub struct CommitArgs {
    /// Mesh name to commit. Omit to commit every mesh that has a
    /// non-empty staging area.
    pub name: Option<String>,
}

#[derive(Debug, clap::Args)]
pub struct RestoreArgs {
    /// Mesh whose pending staging area should be cleared.
    pub name: String,
}

#[derive(Debug, clap::Args)]
pub struct RevertArgs {
    /// Mesh ref to move.
    pub name: String,

    /// Prior mesh commit (or source commit-ish) to fast-forward the mesh to.
    #[arg(value_name = "COMMIT-ISH")]
    pub commit_ish: String,
}

#[derive(Debug, clap::Args)]
pub struct DeleteArgs {
    /// Mesh ref to delete (removes `refs/meshes/v1/<name>`).
    pub name: String,
}

#[derive(Debug, clap::Args)]
pub struct MvArgs {
    /// Existing mesh name.
    pub old: String,

    /// New mesh name (must not already exist).
    pub new: String,
}

#[derive(Debug, clap::Args)]
pub struct ConfigArgs {
    /// Mesh whose resolver options to read or stage.
    pub name: String,

    #[arg(
        help = "Config key. Omit to print all keys. Known: copy-detection, ignore-whitespace",
        long_help = "Config key. Omit to print all keys. Known keys:\n  copy-detection     off | same-file | same-commit | any\n  ignore-whitespace  true | false"
    )]
    pub key: Option<String>,

    /// Value to stage for `<KEY>`. Omit to read the current value.
    pub value: Option<String>,

    /// Stage a reset to the built-in default for `<key>`.
    #[arg(long, value_name = "KEY", conflicts_with_all = ["key", "value"])]
    pub unset: Option<String>,
}

#[derive(Debug, clap::Args)]
pub struct FetchArgs {
    /// Remote to fetch from.
    /// Defaults to `mesh.defaultRemote`, or `origin` if unset.
    pub remote: Option<String>,
}

#[derive(Debug, clap::Args)]
pub struct PushArgs {
    /// Remote to push to.
    /// Defaults to `mesh.defaultRemote`, or `origin` if unset.
    pub remote: Option<String>,
}

#[derive(Debug, clap::Args)]
pub struct DoctorArgs {
    /// Promote INFO and WARN findings to a non-zero exit.
    #[arg(long)]
    pub strict: bool,
}

/// Parse a `<path>#L<start>-L<end>` range address.
///
/// Utility lives here (rather than `validation.rs`) because it's a CLI
/// concern — the library side takes already-split `(path, start, end)`
/// arguments.
pub fn parse_range_address(text: &str) -> anyhow::Result<(String, u32, u32)> {
    let (path, fragment) = text.split_once("#L").ok_or_else(|| {
        anyhow::anyhow!("invalid range `{text}`; expected <path>#L<start>-L<end>")
    })?;
    let (start, end) = fragment.split_once("-L").ok_or_else(|| {
        anyhow::anyhow!("invalid range `{text}`; expected <path>#L<start>-L<end>")
    })?;
    anyhow::ensure!(!path.is_empty(), "range path cannot be empty");
    let start: u32 = start.parse()?;
    let end: u32 = end.parse()?;
    anyhow::ensure!(start >= 1, "range start must be at least 1");
    anyhow::ensure!(end >= start, "range end must be at least start");
    Ok((path.to_string(), start, end))
}

/// Dispatch a parsed [`Commands`] to its handler. Called from `main`.
pub fn dispatch(repo: &gix::Repository, command: Commands) -> anyhow::Result<i32> {
    match command {
        Commands::Show(args) => show::run_show(repo, args),
        Commands::Ls(args) => show::run_ls(repo, args),
        Commands::Stale(args) => stale_output::run_stale(repo, args),
        Commands::Add(args) => commit::run_add(repo, args),
        Commands::Rm(args) => commit::run_rm(repo, args),
        Commands::Why(args) => commit::run_why(repo, args),
        Commands::Commit(args) => commit::run_commit(repo, args),
        Commands::Config(args) => commit::run_config(repo, args),
        Commands::Restore(args) => structural::run_restore(repo, args),
        Commands::Revert(args) => structural::run_revert(repo, args),
        Commands::Delete(args) => structural::run_delete(repo, args),
        Commands::Mv(args) => structural::run_mv(repo, args),
        Commands::Doctor(args) => structural::run_doctor(repo, args),
        Commands::Fetch(args) => sync::run_fetch(repo, args),
        Commands::Push(args) => sync::run_push(repo, args),
        Commands::PreCommit(args) => pre_commit::run_pre_commit(repo, args),
        Commands::Advice(args) => advice::run_advice(repo, args),
    }
}

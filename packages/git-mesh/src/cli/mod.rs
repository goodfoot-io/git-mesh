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
    about = "Attach tracked, updatable metadata to line ranges in a git repo.",
    version
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

    /// List files / ranges via the file index (§3.4).
    Ls(LsArgs),

    /// Run the resolver and report drift (§10.4).
    Stale(StaleArgs),

    /// Stage ranges to add on the next mesh commit (§6.3).
    Add(AddArgs),

    /// Stage ranges to remove on the next mesh commit (§6.3).
    Rm(RmArgs),

    /// Read or stage the mesh's why text (§6.3, §10.2; plan
    /// `docs/why-plan.md`). Bare `git mesh why <name>` prints the current
    /// why; the writer flags `-m`/`-F`/`--edit` stage a new one.
    Why(WhyArgs),

    /// Resolve staged operations and write a mesh commit (§6.2).
    Commit(CommitArgs),

    /// Clear the staging area (§6.8).
    Restore(RestoreArgs),

    /// Fast-forward a mesh to a past state (§6.6).
    Revert(RevertArgs),

    /// Delete a mesh ref (§6.8).
    Delete(DeleteArgs),

    /// Rename a mesh ref (§6.8).
    Mv(MvArgs),

    /// Read or stage mesh-level resolver options (§10.5).
    Config(ConfigArgs),

    /// Fetch mesh and range refs from a remote (§7).
    Fetch(FetchArgs),

    /// Push mesh and range refs to a remote (§7).
    Push(PushArgs),

    /// Audit the local mesh setup (§6.7).
    Doctor(DoctorArgs),

    /// Pre-commit hook body — fail the commit if the in-flight changes
    /// would leave the mesh stale (plan §"Phase 4").
    #[command(name = "pre-commit-check")]
    PreCommitCheck,
}

/// `git mesh <name>` / `git mesh show <name>`.
#[derive(Debug, clap::Args)]
pub struct ShowArgs {
    /// Mesh name. Required (the bare `git mesh` form with no name is
    /// handled by the `Commands::None` branch in `main`, which lists
    /// every mesh).
    pub name: String,

    /// One line per Range, no commit header (§10.4).
    #[arg(long)]
    pub oneline: bool,

    /// Format-string override (§10.4).
    #[arg(long, value_name = "FMT")]
    pub format: Option<String>,

    /// Full 40-char anchor shas.
    #[arg(long)]
    pub no_abbrev: bool,

    /// Show state at a past revision of the mesh ref.
    #[arg(long, value_name = "COMMIT-ISH")]
    pub at: Option<String>,

    /// Walk the mesh's commit history instead of showing the tip.
    #[arg(long)]
    pub log: bool,

    /// Cap the `--log` walk (§6.6).
    #[arg(long, value_name = "N", requires = "log")]
    pub limit: Option<usize>,
}

#[derive(Debug, clap::Args)]
pub struct LsArgs {
    /// Optional `<path>` or `<path>#L<s>-L<e>` to filter by (§3.4).
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
    /// Optional mesh name; omit for workspace-wide scan (§10.4).
    pub name: Option<String>,

    #[arg(long, value_enum, default_value_t = StaleFormat::Human)]
    pub format: StaleFormat,

    /// Force exit 0 even with findings (§10.4).
    #[arg(long)]
    pub no_exit_code: bool,

    /// Disable the worktree layer (plan §B4 — part of the HEAD-only CI fast path).
    #[arg(long)]
    pub no_worktree: bool,

    /// Disable the index layer.
    #[arg(long)]
    pub no_index: bool,

    /// Disable the staged-mesh layer (`.git/mesh/staging/`).
    #[arg(long)]
    pub no_staged_mesh: bool,

    /// Downgrade `ContentUnavailable` findings: they print but do not
    /// drive the exit code (plan §B3).
    #[arg(long)]
    pub ignore_unavailable: bool,

    #[arg(long, conflicts_with_all = ["stat", "patch"])]
    pub oneline: bool,

    #[arg(long, conflicts_with_all = ["oneline", "patch"])]
    pub stat: bool,

    #[arg(long, conflicts_with_all = ["oneline", "stat"])]
    pub patch: bool,

    /// Only ranges anchored at or after this commit (§10.4).
    #[arg(long, value_name = "COMMIT-ISH")]
    pub since: Option<String>,
}

#[derive(Debug, clap::Args)]
pub struct AddArgs {
    /// Mesh name to stage into.
    pub name: String,

    /// One or more `<path>#L<start>-L<end>` ranges.
    ///
    /// Annotated `trailing_var_arg = false` + `allow_hyphen_values = false`
    /// so a trailing `--at <commit-ish>` is parsed as the named flag,
    /// not greedily consumed into `ranges` (Slice 6e of the review plan).
    #[arg(required = true, trailing_var_arg = false, allow_hyphen_values = false)]
    pub ranges: Vec<String>,

    /// Anchor every staged range in this invocation at `<commit-ish>`.
    /// Default is HEAD resolved at commit time (§6.3).
    #[arg(long, value_name = "COMMIT-ISH")]
    pub at: Option<String>,
}

#[derive(Debug, clap::Args)]
pub struct RmArgs {
    pub name: String,
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
    pub name: String,

    /// Inline why text (`-m "..."`). Writer flag.
    #[arg(short = 'm', value_name = "MSG")]
    pub m: Option<String>,

    /// Read why text from a file (`-F <file>`). Writer flag.
    #[arg(short = 'F', value_name = "FILE")]
    pub file: Option<String>,

    /// Open `$EDITOR` on a pre-populated template. Writer flag.
    #[arg(long, conflicts_with = "at")]
    pub edit: bool,

    /// Reader-only: print the why text at a historical commit on the
    /// mesh ref. Requires no writer flag (`-m`/`-F`/`--edit`).
    #[arg(long, value_name = "COMMIT-ISH", conflicts_with_all = ["m", "file", "edit"])]
    pub at: Option<String>,
}

#[derive(Debug, clap::Args)]
pub struct CommitArgs {
    /// Mesh name to commit. Omit to commit every mesh that has a
    /// non-empty staging area (post-commit hook path, §10.2).
    pub name: Option<String>,
}

#[derive(Debug, clap::Args)]
pub struct RestoreArgs {
    pub name: String,
}

#[derive(Debug, clap::Args)]
pub struct RevertArgs {
    pub name: String,
    #[arg(value_name = "COMMIT-ISH")]
    pub commit_ish: String,
}

#[derive(Debug, clap::Args)]
pub struct DeleteArgs {
    pub name: String,
}

#[derive(Debug, clap::Args)]
pub struct MvArgs {
    pub old: String,
    pub new: String,
}

#[derive(Debug, clap::Args)]
pub struct ConfigArgs {
    pub name: String,
    /// Config key (e.g. `copy-detection`, `ignore-whitespace`).
    pub key: Option<String>,
    /// If present, stage a mutation. Otherwise read-only.
    pub value: Option<String>,
    /// Stage a reset to the built-in default for `<key>` (§10.5).
    #[arg(long, value_name = "KEY", conflicts_with_all = ["key", "value"])]
    pub unset: Option<String>,
}

#[derive(Debug, clap::Args)]
pub struct FetchArgs {
    /// Override `mesh.defaultRemote`.
    pub remote: Option<String>,
}

#[derive(Debug, clap::Args)]
pub struct PushArgs {
    pub remote: Option<String>,
}

#[derive(Debug, clap::Args)]
pub struct DoctorArgs {
    /// Promote INFO and WARN findings to a non-zero exit (§6.7).
    #[arg(long)]
    pub strict: bool,
}

/// Parse a `<path>#L<start>-L<end>` range address (§10.3).
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
        Commands::PreCommitCheck => pre_commit::run_pre_commit_check(repo),
    }
}

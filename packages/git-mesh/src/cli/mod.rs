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

    /// Set the staged commit message (§6.3, §10.2).
    Message(MessageArgs),

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
    Doctor,

    /// Show staging-area state; `--check` is the pre-commit gate (§6.4).
    Status(StatusArgs),
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
    #[arg(required = true)]
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
pub struct MessageArgs {
    pub name: String,

    /// Inline message body (`-m "..."`).
    #[arg(short = 'm', value_name = "MSG")]
    pub m: Option<String>,

    /// Read message from file (`-F <file>`).
    #[arg(short = 'F', value_name = "FILE")]
    pub file: Option<String>,

    /// Open `$EDITOR` on a pre-populated template. Bare `git mesh
    /// message <name>` with no other flags also triggers this (§10.2).
    #[arg(long)]
    pub edit: bool,
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
pub struct StatusArgs {
    /// Mesh name. Required unless `--check` is passed.
    pub name: Option<String>,

    /// Exit non-zero if any staged range differs from the working tree;
    /// used by the suggested pre-commit hook (§6.4).
    #[arg(long, conflicts_with = "name")]
    pub check: bool,
}

/// Parse a `<path>#L<start>-L<end>` range address (§10.3).
///
/// Utility lives here (rather than `validation.rs`) because it's a CLI
/// concern — the library side takes already-split `(path, start, end)`
/// arguments.
pub fn parse_range_address(text: &str) -> anyhow::Result<(String, u32, u32)> {
    let (path, fragment) = text
        .split_once("#L")
        .ok_or_else(|| anyhow::anyhow!("invalid range `{text}`; expected <path>#L<start>-L<end>"))?;
    let (start, end) = fragment
        .split_once("-L")
        .ok_or_else(|| anyhow::anyhow!("invalid range `{text}`; expected <path>#L<start>-L<end>"))?;
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
        Commands::Message(args) => commit::run_message(repo, args),
        Commands::Commit(args) => commit::run_commit(repo, args),
        Commands::Status(args) => commit::run_status(repo, args),
        Commands::Config(args) => commit::run_config(repo, args),
        Commands::Restore(args) => structural::run_restore(repo, args),
        Commands::Revert(args) => structural::run_revert(repo, args),
        Commands::Delete(args) => structural::run_delete(repo, args),
        Commands::Mv(args) => structural::run_mv(repo, args),
        Commands::Doctor => structural::run_doctor(repo),
        Commands::Fetch(args) => sync::run_fetch(repo, args),
        Commands::Push(args) => sync::run_push(repo, args),
    }
}

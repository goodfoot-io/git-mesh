//! `git-mesh` CLI entrypoint.

use anyhow::{Context, Result};
use clap::{CommandFactory, Parser};
use git_mesh::cli::{self, Cli, Commands, ShowArgs};
use git_mesh::validation::RESERVED_MESH_NAMES;

fn main() {
    // Slice 6a: restore the default Unix SIGPIPE handler so a broken
    // downstream pipe (`git mesh ... | head`) becomes a clean exit
    // rather than a Rust panic on `println!`.
    #[cfg(unix)]
    // SAFETY: `signal` with `SIG_DFL` is async-signal-safe and is the
    // canonical recipe for restoring the default disposition that Rust
    // overrides on startup. Called once before any I/O.
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }

    match run() {
        Ok(code) => std::process::exit(code),
        Err(error) => {
            // Clap usage errors (bad flag, missing arg) keep clap's
            // own exit-2 contract (and its formatted message, plus
            // exit 0 for `--help` / `--version`). Everything else is
            // an operational failure → exit 1, matching the convention
            // used by `git`, `cargo`, and POSIX tooling.
            match error.downcast::<clap::Error>() {
                Ok(clap_err) => clap_err.exit(),
                Err(error) => {
                    eprintln!("error: {error:#}");
                    std::process::exit(1);
                }
            }
        }
    }
}

fn run() -> Result<i32> {
    let args: Vec<String> = std::env::args().collect();

    // §10.2: `git mesh` with no arg lists every mesh; `git mesh <name>`
    // is a positional show. Clap can't distinguish a bare-name positional
    // from a subcommand, so we pre-classify before invoking the parser.
    // A token on the §10.2 reserved list is a subcommand; anything else
    // is a mesh name and routes to `Commands::Show`.
    //
    // Repo discovery happens after parsing so `--help`, `--version`, and
    // any other clap-handled flag works outside a git repo.
    if args.len() == 1 {
        Cli::command().print_help()?;
        println!();
        return Ok(0);
    }
    let first = &args[1];
    let is_flag = first.starts_with('-');
    let is_reserved = RESERVED_MESH_NAMES.contains(&first.as_str())
        || matches!(
            first.as_str(),
            "show" | "help" | "--help" | "-h" | "--version" | "-V"
        );

    if !is_flag && !is_reserved {
        // Bare `git mesh <name> [--flags...]` — parse the tail as ShowArgs.
        let mut show_argv = vec![args[0].clone(), "show".to_string()];
        show_argv.extend(args[1..].iter().cloned());
        let cli = Cli::try_parse_from(show_argv)?;
        git_mesh::perf::init(cli.perf);
        let cmd = cli.command.unwrap_or_else(|| {
            Commands::Show(ShowArgs {
                name: first.clone(),
                oneline: false,
                format: None,
                at: None,
                log: false,
                limit: None,
            })
        });
        let repo = discover_repo()?;
        return cli::dispatch(&repo, cmd);
    }

    // Parse first so `--help` / `--version` short-circuit before we
    // touch the filesystem for repo discovery.
    let cli = Cli::parse();
    git_mesh::perf::init(cli.perf);

    let repo = discover_repo()?;
    match cli.command {
        Some(cmd) => cli::dispatch(&repo, cmd),
        None => {
            Cli::command().print_help()?;
            println!();
            Ok(0)
        }
    }
}

fn discover_repo() -> Result<gix::Repository> {
    let _perf = git_mesh::perf::span("git.discover");
    gix::discover(".").context("not inside a git repository")
}

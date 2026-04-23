//! `git-mesh` CLI entrypoint.

use anyhow::{Context, Result};
use clap::Parser;
use git_mesh::cli::{self, Cli, Commands, ShowArgs};
use git_mesh::validation::RESERVED_MESH_NAMES;

fn main() {
    match run() {
        Ok(code) => std::process::exit(code),
        Err(error) => {
            eprintln!("error: {error:#}");
            std::process::exit(2);
        }
    }
}

fn run() -> Result<i32> {
    let repo = gix::discover(".").context("not inside a git repository")?;
    let args: Vec<String> = std::env::args().collect();

    // §10.2: `git mesh` with no arg lists every mesh; `git mesh <name>`
    // is a positional show. Clap can't distinguish a bare-name positional
    // from a subcommand, so we pre-classify before invoking the parser.
    // A token on the §10.2 reserved list is a subcommand; anything else
    // is a mesh name and routes to `Commands::Show`.
    if args.len() == 1 {
        return cli::show::run_list(&repo);
    }
    let first = &args[1];
    let is_flag = first.starts_with('-');
    let is_reserved = RESERVED_MESH_NAMES.contains(&first.as_str())
        || matches!(first.as_str(), "show" | "help" | "--help" | "-h" | "--version" | "-V");

    if !is_flag && !is_reserved {
        // Bare `git mesh <name> [--flags...]` — parse the tail as ShowArgs.
        let mut show_argv = vec![args[0].clone(), "show".to_string()];
        show_argv.extend(args[1..].iter().cloned());
        let cli = Cli::try_parse_from(show_argv)?;
        let cmd = cli
            .command
            .unwrap_or_else(|| Commands::Show(ShowArgs {
                name: first.clone(),
                oneline: false,
                format: None,
                no_abbrev: false,
                at: None,
                log: false,
                limit: None,
            }));
        return cli::dispatch(&repo, cmd);
    }

    let cli = Cli::parse();
    match cli.command {
        Some(cmd) => cli::dispatch(&repo, cmd),
        None => cli::show::run_list(&repo),
    }
}

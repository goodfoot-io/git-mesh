//! Cross-platform test-helper binary used by integration tests in place of
//! shell scripts (which are not portable to Windows). Behavior is selected
//! by the `HELPER_MODE` env var. Dependency-free (std only).
//!
//! Modes:
//! - `editor-replace`: overwrite argv[1] with `HELPER_CONTENT`.
//! - `editor-append`: append a newline + `HELPER_CONTENT` to argv[1].
//! - `editor-capture`: copy argv[1] to `<argv[1]>.seen`, then overwrite
//!   argv[1] with `HELPER_CONTENT`.
//! - `exec`: spawn `HELPER_TARGET` with this process's argv[1..], inherit
//!   stdio, and exit with the child's exit code.

use std::process::{Command, ExitCode};

fn main() -> ExitCode {
    let mode = std::env::var("HELPER_MODE").unwrap_or_default();
    let args: Vec<String> = std::env::args().skip(1).collect();

    match mode.as_str() {
        "editor-replace" => {
            let target = &args[0];
            let content = std::env::var("HELPER_CONTENT").unwrap_or_default();
            std::fs::write(target, content).expect("editor-replace write");
            ExitCode::SUCCESS
        }
        "editor-append" => {
            let target = &args[0];
            let content = std::env::var("HELPER_CONTENT").unwrap_or_default();
            let mut existing = std::fs::read_to_string(target).unwrap_or_default();
            existing.push('\n');
            existing.push_str(&content);
            std::fs::write(target, existing).expect("editor-append write");
            ExitCode::SUCCESS
        }
        "editor-capture" => {
            let target = &args[0];
            let content = std::env::var("HELPER_CONTENT").unwrap_or_default();
            std::fs::copy(target, format!("{target}.seen")).expect("editor-capture copy");
            std::fs::write(target, content).expect("editor-capture write");
            ExitCode::SUCCESS
        }
        "exec" => {
            let prog = std::env::var("HELPER_TARGET").expect("HELPER_TARGET must be set");
            let status = Command::new(prog)
                .args(&args)
                .status()
                .expect("exec spawn target");
            ExitCode::from(status.code().unwrap_or(1) as u8)
        }
        other => {
            eprintln!("git-mesh-test-helper: unknown HELPER_MODE: {other:?}");
            ExitCode::from(2)
        }
    }
}

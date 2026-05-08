//! Structural invariants for the cargo configuration layout.
//!
//! The CI runner has no `sccache` on `PATH`. A workspace-root `.cargo/config.toml`
//! that pins `rustc-wrapper = "sccache"` is picked up by every root-CWD cargo
//! invocation (notably `Swatinem/rust-cache@v2`'s `cargo metadata`) and breaks
//! the build with `could not execute process \`sccache rustc -vV\``. Cargo also
//! cannot find a `Cargo.toml` from the repo root, which surfaces as
//! `could not find \`Cargo.toml\``. Both failures are eliminated by keeping cargo
//! configuration package-local and routing the wrapper through environment
//! variables that only sccache-equipped environments set.
use std::path::PathBuf;

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

#[test]
fn no_cargo_config_at_workspace_root() {
    let path = workspace_root().join(".cargo").join("config.toml");
    assert!(
        !path.exists(),
        "{} must not exist: rust-cache + non-sccache CI runners pick up its rustc-wrapper and fail",
        path.display()
    );
}

#[test]
fn package_local_cargo_config_has_no_rustc_wrapper() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join(".cargo")
        .join("config.toml");
    assert!(path.exists(), "{} must exist", path.display());
    let body = std::fs::read_to_string(&path).unwrap();
    let has_wrapper_key = body
        .lines()
        .map(|l| l.split('#').next().unwrap_or("").trim())
        .any(|l| l.starts_with("rustc-wrapper"));
    assert!(
        !has_wrapper_key,
        "package-local cargo config must not pin rustc-wrapper; CI runners lack sccache. Set RUSTC_WRAPPER via the environment instead."
    );
}

#[test]
fn devcontainer_sets_rustc_wrapper_env() {
    let path = workspace_root()
        .join(".devcontainer")
        .join("devcontainer.json");
    let body = std::fs::read_to_string(&path).unwrap();
    assert!(
        body.contains("RUSTC_WRAPPER"),
        "{} must set RUSTC_WRAPPER in remoteEnv so the devcontainer keeps sccache wrapping after the root .cargo/config.toml is removed",
        path.display()
    );
}

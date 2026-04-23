//! Name validation (§3.5, §10.2 reserved list).

use crate::{Error, Result};

/// Subcommands and reserved tokens that cannot be used as mesh names.
/// From §10.2 "Reserved names."
pub const RESERVED_MESH_NAMES: &[&str] = &[
    "add", "rm", "commit", "message", "restore", "revert", "delete", "mv", "stale", "fetch",
    "push", "doctor", "log", "config", "status", "ls", "help",
];

/// Validate a mesh name against §3.5 and §10.2.
pub fn validate_mesh_name(name: &str) -> Result<()> {
    if RESERVED_MESH_NAMES.contains(&name) {
        return Err(Error::ReservedName(name.to_string()));
    }
    validate_ref_component(name)
}

/// Validate a range id (UUID, ref-legal).
pub fn validate_range_id(id: &str) -> Result<()> {
    validate_ref_component(id)
}

fn validate_ref_component(value: &str) -> Result<()> {
    fn bad(msg: impl Into<String>) -> Error {
        Error::InvalidName(msg.into())
    }
    if value.is_empty() {
        return Err(bad("name must not be empty"));
    }
    if value.starts_with('-') {
        return Err(bad(format!("`{value}` must not start with `-`")));
    }
    if value.starts_with('.') {
        return Err(bad(format!("`{value}` must not start with `.`")));
    }
    if value.ends_with('.') {
        return Err(bad(format!("`{value}` must not end with `.`")));
    }
    if value.ends_with(".lock") {
        return Err(bad(format!("`{value}` must not end with `.lock`")));
    }
    if value == "@" {
        return Err(bad("`@` is not allowed"));
    }
    if value.contains("..") {
        return Err(bad(format!("`{value}` must not contain `..`")));
    }
    if value.contains("@{") {
        return Err(bad(format!("`{value}` must not contain `@{{`")));
    }
    for ch in value.chars() {
        if ch == '/' {
            return Err(bad(format!("`{value}` must not contain `/`")));
        }
        if ch.is_whitespace() {
            return Err(bad(format!("`{value}` must not contain whitespace")));
        }
        if ch.is_control() {
            return Err(bad(format!("`{value}` must not contain control characters")));
        }
        if matches!(ch, '~' | '^' | ':' | '?' | '*' | '[' | '\\') {
            return Err(bad(format!("`{value}` must not contain `{ch}`")));
        }
    }
    Ok(())
}

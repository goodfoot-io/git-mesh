//! Name validation (§3.5, §10.2 reserved list).

use crate::{Error, Result};

/// Subcommands and reserved tokens that cannot be used as mesh names.
/// From §10.2 "Reserved names."
pub const RESERVED_MESH_NAMES: &[&str] = &[
    "add",
    "rm",
    "commit",
    "why",
    "restore",
    "revert",
    "delete",
    "mv",
    "stale",
    "fetch",
    "push",
    "doctor",
    "log",
    "config",
    "ls",
    "help",
    "pre-commit",
    "advice",
];

/// Mesh-name shape, per `docs/advice-notes.md` §12.12 T7 and the handbook:
/// one or more kebab-case segments separated by `/`. The recommended
/// hierarchical form is `<category>/<subcategory>/<identifier-slug>`,
/// but a bare slug or any depth `>= 1` is accepted.
/// Concretely: `^[a-z0-9][a-z0-9-]*(/[a-z0-9][a-z0-9-]*)*$`.
pub const MESH_NAME_RULE: &str = "kebab-case segments separated by `/` (e.g. `<slug>`, `<category>/<slug>`, \
     or `<category>/<subcategory>/<identifier-slug>`); lowercase a-z, 0-9, \
     and `-`; each segment must start with a letter or digit";

/// Validate a mesh name against §3.5, §10.2, and the §12.12 T7 naming rule.
pub fn validate_mesh_name(name: &str) -> Result<()> {
    if RESERVED_MESH_NAMES.contains(&name) {
        return Err(Error::ReservedName(name.to_string()));
    }
    validate_mesh_name_shape(name)
}

/// Validate a anchor id (UUID, ref-legal).
pub fn validate_anchor_id(id: &str) -> Result<()> {
    validate_ref_component(id)
}

fn validate_mesh_name_shape(value: &str) -> Result<()> {
    fn bad(msg: impl Into<String>) -> Error {
        Error::InvalidName(msg.into())
    }
    if value.is_empty() {
        return Err(bad("mesh name must not be empty"));
    }
    // Split hierarchical `<a>/<b>/<c>/...` into one or more segments.
    let segments: Vec<&str> = value.split('/').collect();
    for segment in &segments {
        if segment.is_empty() {
            return Err(bad(format!(
                "`{value}` has an empty segment ({MESH_NAME_RULE})"
            )));
        }
        let first = segment.chars().next().unwrap();
        if !first.is_ascii_lowercase() && !first.is_ascii_digit() {
            return Err(bad(format!(
                "`{value}` segment `{segment}` must start with a-z or 0-9 ({MESH_NAME_RULE})"
            )));
        }
        for ch in segment.chars() {
            let ok = ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-';
            if !ok {
                return Err(bad(format!(
                    "`{value}` segment `{segment}` contains invalid character `{ch}` ({MESH_NAME_RULE})"
                )));
            }
        }
    }
    // Belt-and-braces ref-legality checks (the kebab-case rule already
    // forbids most of these, but keep the explicit refusals for clarity).
    if value.contains("..") {
        return Err(bad(format!("`{value}` must not contain `..`")));
    }
    if value.ends_with(".lock") {
        return Err(bad(format!("`{value}` must not end with `.lock`")));
    }
    Ok(())
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
            return Err(bad(format!(
                "`{value}` must not contain control characters"
            )));
        }
        if matches!(ch, '~' | '^' | ':' | '?' | '*' | '[' | '\\') {
            return Err(bad(format!("`{value}` must not contain `{ch}`")));
        }
    }
    Ok(())
}

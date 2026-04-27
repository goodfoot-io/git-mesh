//! Path filter for the advice pipeline.
//!
//! Mirrors `isAcceptablePath` from `docs/analyze-v4.mjs` line 97.
//! Drops paths that would pollute the co-edit signal: absolute paths,
//! directory listings (trailing `/`), tool-internal files, and the
//! analyzer scripts themselves.

use std::path::Path;

/// Returns `true` if `p` is an acceptable path to include in analysis.
///
/// Rejection rules (mirrors `isAcceptablePath` in `docs/analyze-v4.mjs`):
///
/// 1. Empty or non-string → reject.
/// 2. Absolute paths (starts with `/`) → reject.
/// 3. Directory listings (ends with `/`) → reject.
/// 4. Paths with no `/` and no `.` → reject (bare words with no extension).
/// 5. `yarn-validate-output.log` anywhere in the path → reject.
/// 6. `.last-flush` anywhere in the path → reject.
/// 7. Paths with no extension component and ≤ 2 path segments → reject
///    (mirrors the `!/\.[a-zA-Z0-9]{1,8}$/` + `<= 2 segments` guard).
/// 8. `analyze-v<N>.mjs` basename → reject.
/// 9. `mesh-suggestions` or `mesh-suggestions-v<N>` with `.mjs` or `.md`
///    extension → reject.
pub fn is_acceptable_path(p: &Path) -> bool {
    let s = match p.to_str() {
        Some(s) if !s.is_empty() => s,
        _ => return false,
    };

    // Rule 2: absolute paths.
    if s.starts_with('/') {
        return false;
    }

    // Rule 3: directory listings.
    if s.ends_with('/') {
        return false;
    }

    // Rule 4: no slash and no dot → bare word.
    if !s.contains('/') && !s.contains('.') {
        return false;
    }

    // Rule 5: yarn-validate-output.log anywhere.
    if path_component_matches(s, "yarn-validate-output.log") {
        return false;
    }

    // Rule 6: .last-flush anywhere.
    if path_component_matches(s, ".last-flush") {
        return false;
    }

    // Rule 7: no extension and ≤ 2 segments.
    // A "valid" extension is 1–8 ASCII alphanumeric chars after the final dot.
    let has_extension = {
        let basename = s.rsplit('/').next().unwrap_or(s);
        if let Some(pos) = basename.rfind('.') {
            let ext = &basename[pos + 1..];
            !ext.is_empty()
                && ext.len() <= 8
                && ext.bytes().all(|b| b.is_ascii_alphanumeric())
        } else {
            false
        }
    };
    let segment_count = s.split('/').count();
    if !has_extension && segment_count <= 2 {
        return false;
    }

    // Rule 8: analyze-v<N>.mjs basename.
    let basename = s.rsplit('/').next().unwrap_or(s);
    if is_analyze_script(basename) {
        return false;
    }

    // Rule 9: mesh-suggestions* basename.
    if is_mesh_suggestions(basename) {
        return false;
    }

    true
}

/// Returns true if `component` appears as a path component in `path_str`
/// (either as the whole path or after a `/`).
fn path_component_matches(path_str: &str, component: &str) -> bool {
    path_str == component
        || path_str.ends_with(&format!("/{component}"))
        || path_str.contains(&format!("/{component}/"))
        || path_str.starts_with(&format!("{component}/"))
}

/// `analyze-v<digits>.mjs`
fn is_analyze_script(basename: &str) -> bool {
    if let Some(rest) = basename.strip_prefix("analyze-v")
        && let Some(rest2) = rest.strip_suffix(".mjs")
    {
        return !rest2.is_empty() && rest2.bytes().all(|b| b.is_ascii_digit());
    }
    false
}

/// `mesh-suggestions.mjs`, `mesh-suggestions.md`,
/// `mesh-suggestions-v<N>.mjs`, `mesh-suggestions-v<N>.md`
fn is_mesh_suggestions(basename: &str) -> bool {
    let Some(stem) = basename
        .strip_suffix(".mjs")
        .or_else(|| basename.strip_suffix(".md"))
    else {
        return false;
    };

    if stem == "mesh-suggestions" {
        return true;
    }

    if let Some(rest) = stem.strip_prefix("mesh-suggestions-v") {
        return !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit());
    }

    false
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn p(s: &str) -> &Path {
        Path::new(s)
    }

    // Rule 1: empty string
    #[test]
    fn rejects_empty() {
        assert!(!is_acceptable_path(p("")));
    }

    // Rule 2: absolute paths
    #[test]
    fn rejects_absolute() {
        assert!(!is_acceptable_path(p("/home/user/foo.ts")));
        assert!(!is_acceptable_path(p("/foo/bar.rs")));
    }

    // Rule 3: trailing slash (directory listing)
    #[test]
    fn rejects_trailing_slash() {
        assert!(!is_acceptable_path(p("src/foo/")));
        assert!(!is_acceptable_path(p("foo/")));
    }

    // Rule 4: bare word (no slash, no dot)
    #[test]
    fn rejects_bare_word() {
        assert!(!is_acceptable_path(p("README")));
        assert!(!is_acceptable_path(p("Makefile")));
    }

    // Rule 4 inverse: single dot at root is fine (has a dot)
    #[test]
    fn accepts_dotfile_with_slash() {
        // has slash so rule 4 doesn't fire; rule 7 may still apply
        // e.g. src/.env has extension "" → no ext, 2 segments → rejected by rule 7
        // But src/a/b.env → 3 segments, has ext → accepted
        assert!(is_acceptable_path(p("src/a/b.env")));
    }

    // Rule 5: yarn-validate-output.log
    #[test]
    fn rejects_yarn_validate_log() {
        assert!(!is_acceptable_path(p("yarn-validate-output.log")));
        assert!(!is_acceptable_path(p("logs/yarn-validate-output.log")));
    }

    // Rule 6: .last-flush
    #[test]
    fn rejects_last_flush() {
        assert!(!is_acceptable_path(p(".last-flush")));
        assert!(!is_acceptable_path(p("sessions/abc/.last-flush")));
    }

    // Rule 7: no extension + ≤ 2 segments
    #[test]
    fn rejects_no_ext_two_segments() {
        // 2 segments, no extension
        assert!(!is_acceptable_path(p("src/Makefile")));
        // 3 segments, no extension → accepted (rule 7 only fires for ≤ 2)
        assert!(is_acceptable_path(p("a/b/Makefile")));
    }

    // Rule 8: analyze-v<N>.mjs
    #[test]
    fn rejects_analyze_script() {
        assert!(!is_acceptable_path(p("analyze-v4.mjs")));
        assert!(!is_acceptable_path(p("docs/analyze-v4.mjs")));
        assert!(!is_acceptable_path(p("analyze-v10.mjs")));
        // Not a digit-only version → acceptable
        assert!(is_acceptable_path(p("docs/analyze-v4x.mjs")));
    }

    // Rule 9: mesh-suggestions*
    #[test]
    fn rejects_mesh_suggestions() {
        assert!(!is_acceptable_path(p("mesh-suggestions.mjs")));
        assert!(!is_acceptable_path(p("mesh-suggestions.md")));
        assert!(!is_acceptable_path(p("mesh-suggestions-v2.mjs")));
        assert!(!is_acceptable_path(p("mesh-suggestions-v2.md")));
        assert!(!is_acceptable_path(p("docs/mesh-suggestions.md")));
    }

    // Positive cases — paths that should pass all rules
    #[test]
    fn accepts_normal_paths() {
        assert!(is_acceptable_path(p("src/main.rs")));
        assert!(is_acceptable_path(p("packages/git-mesh/src/lib.rs")));
        assert!(is_acceptable_path(p("docs/spec.md")));
        assert!(is_acceptable_path(p("README.md")));
        // .gitignore: dot + 9-char ext ("gitignore") → ext > 8 chars → no match,
        // but it has no slash and only 1 segment → rejected by rule 7. Confirm.
        assert!(!is_acceptable_path(p(".gitignore")));
        // A path under a directory passes rule 7 (3 segments, short ext)
        assert!(is_acceptable_path(p("src/a/.env")));
    }
}

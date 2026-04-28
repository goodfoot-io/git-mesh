//! Debug trace for the advice pipeline.
//!
//! When `GIT_MESH_ADVICE_DEBUG=1` (or `true`) is set, each trace call emits
//! one line to stderr prefixed `git-mesh-advice-debug:`. Disabled by default
//! with a single cached `OnceLock<bool>` read.

use std::sync::OnceLock;

static ENABLED: OnceLock<bool> = OnceLock::new();

/// Returns `true` when `GIT_MESH_ADVICE_DEBUG` is set to `1` or `true`
/// (case-insensitive, whitespace-trimmed). The result is cached for the
/// lifetime of the process.
pub fn enabled() -> bool {
    *ENABLED.get_or_init(|| {
        std::env::var("GIT_MESH_ADVICE_DEBUG")
            .map(|v| {
                let t = v.trim().to_ascii_lowercase();
                t == "1" || t == "true"
            })
            .unwrap_or(false)
    })
}

/// Build a single debug line.
///
/// Format: `git-mesh-advice-debug: {tag} k1=v1 k2=v2\n`
/// Key-value pairs are sorted by key for stability.
pub(crate) fn format_line(tag: &str, kvs: &[(&str, &str)]) -> String {
    let mut pairs: Vec<(&str, &str)> = kvs.to_vec();
    pairs.sort_by_key(|(k, _)| *k);
    let mut line = format!("git-mesh-advice-debug: {tag}");
    for (k, v) in pairs {
        line.push(' ');
        line.push_str(k);
        line.push('=');
        line.push_str(v);
    }
    line.push('\n');
    line
}

/// Emit one debug line to stderr. Best-effort; never panics.
pub fn trace(tag: &str, kvs: &[(&str, &str)]) {
    if enabled() {
        eprint!("{}", format_line(tag, kvs));
    }
}

/// Short-circuit macro: checks `enabled()` before any formatting cost.
#[macro_export]
macro_rules! advice_debug {
    ($tag:expr, $($k:expr => $v:expr),* $(,)?) => {
        if $crate::advice::debug::enabled() {
            $crate::advice::debug::trace($tag, &[$(($k, &$v.to_string() as &str)),*]);
        }
    };
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_line_prefix() {
        let line = format_line("test-tag", &[]);
        assert!(line.starts_with("git-mesh-advice-debug: test-tag"), "got: {line:?}");
    }

    #[test]
    fn format_line_single_newline() {
        let line = format_line("tag", &[("k", "v")]);
        assert!(line.ends_with('\n'), "must end with newline");
        assert_eq!(line.chars().filter(|&c| c == '\n').count(), 1, "exactly one newline");
    }

    #[test]
    fn format_line_sorted_kv() {
        let line = format_line("tag", &[("z", "last"), ("a", "first"), ("m", "mid")]);
        // sorted: a=first m=mid z=last
        let expected = "git-mesh-advice-debug: tag a=first m=mid z=last\n";
        assert_eq!(line, expected);
    }

    #[test]
    fn format_line_detector_hit() {
        let line = format_line("detect_partner_drift", &[
            ("mesh", "my-mesh"),
            ("reason_kind", "Terminal"),
            ("partner", "src/foo.rs#L1-L10"),
        ]);
        assert!(line.contains("mesh=my-mesh"), "got: {line:?}");
        assert!(line.contains("reason_kind=Terminal"), "got: {line:?}");
    }

    #[test]
    fn format_line_drop_reason() {
        let line = format_line("dropped", &[
            ("mesh", "m"),
            ("reason", "advice-seen"),
        ]);
        assert!(line.contains("reason=advice-seen"), "got: {line:?}");
    }
}

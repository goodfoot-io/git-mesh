//! Debug trace for the advice pipeline.
//!
//! When `GIT_MESH_ADVICE_DEBUG=1` (or `true`) is set, each trace call emits
//! one line to stderr prefixed `git-mesh-advice-debug:`. Disabled by default
//! with a single cached `OnceLock<bool>` read.

use std::borrow::Cow;
use std::sync::OnceLock;

static ENABLED: OnceLock<bool> = OnceLock::new();

/// Returns `true` when `GIT_MESH_ADVICE_DEBUG` is set to `1` or `true`
/// (case-insensitive, whitespace-trimmed). The result is cached for the
/// lifetime of the process.
///
/// **Caching note:** the env var is read exactly once via [`OnceLock`] and
/// the result is frozen for the process lifetime. In-process tests that call
/// `std::env::set_var` after the first call to `enabled()` will not observe
/// the change. Integration tests that need to toggle the flag must spawn a
/// child process (e.g. via `Command::new(env!("CARGO_BIN_EXE_git-mesh"))`)
/// rather than relying on `set_var`.
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

/// Escape a value for inclusion in a debug line.
///
/// Bare values pass through unchanged. Values containing a space, tab, `"`,
/// `\`, or newline are wrapped in double quotes with internal `"`, `\`, and
/// newlines backslash-escaped, so a downstream split-on-whitespace parser can
/// reliably tokenise the output.
fn escape_value(v: &str) -> Cow<'_, str> {
    if v.bytes().any(|b| matches!(b, b' ' | b'\t' | b'\r' | b'"' | b'\\' | b'\n')) {
        let mut out = String::with_capacity(v.len() + 2);
        out.push('"');
        for c in v.chars() {
            match c {
                '"' => out.push_str("\\\""),
                '\\' => out.push_str("\\\\"),
                '\n' => out.push_str("\\n"),
                '\t' => out.push_str("\\t"),
                '\r' => out.push_str("\\r"),
                _ => out.push(c),
            }
        }
        out.push('"');
        Cow::Owned(out)
    } else {
        Cow::Borrowed(v)
    }
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
        line.push_str(&escape_value(v));
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
    fn escape_value_bare_passthrough() {
        // A plain value with no special characters is returned as-is (Borrowed).
        let v = "src/foo.rs";
        assert_eq!(escape_value(v), v);
    }

    #[test]
    fn escape_value_space_quoted() {
        // A value containing a space must be wrapped in double quotes.
        let v = "my dir/file.rs";
        let got = escape_value(v);
        assert_eq!(got, "\"my dir/file.rs\"");
    }

    #[test]
    fn escape_value_embedded_quote_and_newline() {
        // Internal `"` and `\n` must be backslash-escaped inside the quotes.
        let v = "say \"hi\"\nbye";
        let got = escape_value(v);
        assert_eq!(got, "\"say \\\"hi\\\"\\nbye\"");
    }

    #[test]
    fn format_line_drop_reason() {
        let line = format_line("dropped", &[
            ("mesh", "m"),
            ("reason", "advice-seen"),
        ]);
        assert!(line.contains("reason=advice-seen"), "got: {line:?}");
    }

    #[test]
    fn escape_value_tab() {
        // A value containing a tab must be wrapped in double quotes with \t escaped.
        let v = "has\ttab";
        let got = escape_value(v);
        assert_eq!(got, "\"has\\ttab\"");
    }

    #[test]
    fn escape_value_carriage_return() {
        // A value containing a carriage return must be wrapped in double quotes with \r escaped.
        let v = "has\rreturn";
        let got = escape_value(v);
        assert_eq!(got, "\"has\\rreturn\"");
    }
}

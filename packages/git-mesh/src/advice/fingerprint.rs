//! FNV-64 fingerprinting for advice candidates.
//!
//! The fingerprint hashes structural fields only — wording, whitespace, and
//! render timestamps do not affect the result.

use crate::advice::candidates::Candidate;

/// FNV-64 hash over byte sequence.
#[cfg(test)]
fn fnv64(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

/// Feed a string slice into the running hash state, terminated by `\0`.
/// NUL is illegal in POSIX paths and Git path names, making it a safe
/// field boundary that cannot be confused with path content.
fn feed(h: &mut u64, s: &str) {
    for &b in s.as_bytes() {
        *h ^= b as u64;
        *h = h.wrapping_mul(0x100000001b3);
    }
    // NUL separator — safe field boundary
    *h ^= 0u64;
    *h = h.wrapping_mul(0x100000001b3);
}

/// Return a lowercase-hex FNV-64 fingerprint of the structural fields of `c`.
///
/// Hash inputs (fixed order, each terminated by `\0`):
/// 1. reason_kind string
/// 2. mesh name
/// 3. partner_path
/// 4. partner_start (decimal, or "0")
/// 5. partner_end (decimal, or "0")
/// 6. trigger_path
/// 7. trigger_start (decimal, or "0")
/// 8. trigger_end (decimal, or "0")
/// 9. partner_marker
/// 10. old_path (empty string when None)
/// 11. new_path (empty string when None)
/// 12. old_blob (empty string when None)
/// 13. new_blob (empty string when None)
///
/// `command` is intentionally omitted — command is rendered text whose wording
/// must not affect deduplication.
#[allow(dead_code)]
pub fn fingerprint(c: &Candidate) -> String {
    let mut h: u64 = 0xcbf29ce484222325;

    feed(&mut h, c.reason_kind.as_str());
    feed(&mut h, &c.mesh);
    feed(&mut h, &c.partner_path);
    feed(&mut h, &c.partner_start.map(|v| v.to_string()).unwrap_or_else(|| "0".to_string()));
    feed(&mut h, &c.partner_end.map(|v| v.to_string()).unwrap_or_else(|| "0".to_string()));
    feed(&mut h, &c.trigger_path);
    feed(&mut h, &c.trigger_start.map(|v| v.to_string()).unwrap_or_else(|| "0".to_string()));
    feed(&mut h, &c.trigger_end.map(|v| v.to_string()).unwrap_or_else(|| "0".to_string()));
    feed(&mut h, &c.partner_marker);
    feed(&mut h, c.old_path.as_deref().unwrap_or(""));
    feed(&mut h, c.new_path.as_deref().unwrap_or(""));
    feed(&mut h, c.old_blob.as_deref().unwrap_or(""));
    feed(&mut h, c.new_blob.as_deref().unwrap_or(""));

    format!("{h:016x}")
}

/// Return a lowercase-hex FNV-64 fingerprint for an n-ary `Suggestion`
/// produced by the suggester pipeline (`meta.is_none()` case).
///
/// Hash inputs (fixed order, each terminated by `\0`):
/// 1. literal tag `"suggest"`
/// 2. for each participant in order: path, start, end, whole-flag (`"1"`/`"0"`)
///
/// `score`, `band`, `viability`, and `label` are intentionally excluded —
/// the same set of (path, anchor) participants must dedup across runs even
/// when scoring drifts. Drift-detector suggestions (`meta.is_some()`) are
/// fingerprinted via `fingerprint(&Candidate)` upstream and never reach
/// this helper.
pub fn fingerprint_suggestion(s: &crate::advice::suggestion::Suggestion) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    feed(&mut h, "suggest");
    for p in &s.participants {
        feed(&mut h, p.path.to_string_lossy().as_ref());
        feed(&mut h, &p.start.to_string());
        feed(&mut h, &p.end.to_string());
        feed(&mut h, if p.whole { "1" } else { "0" });
    }
    format!("{h:016x}")
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::advice::candidates::{Density, ReasonKind};

    /// Build a minimal Candidate with all structural fields set to defaults.
    fn make_candidate() -> Candidate {
        Candidate {
            mesh: "test-mesh".to_string(),
            mesh_why: "why".to_string(),
            reason_kind: ReasonKind::Partner,
            partner_path: "src/partner.rs".to_string(),
            partner_start: Some(1),
            partner_end: Some(10),
            trigger_path: "src/trigger.rs".to_string(),
            trigger_start: None,
            trigger_end: None,
            touched_path: String::new(),
            touched_start: None,
            touched_end: None,
            partner_marker: String::new(),
            partner_clause: String::new(),
            density: Density::L0,
            command: String::new(),
            excerpt_of_path: String::new(),
            excerpt_start: None,
            excerpt_end: None,
            old_blob: None,
            new_blob: None,
            old_path: None,
            new_path: None,
        }
    }

    /// The same Candidate must always produce the same fingerprint (stability).
    #[test]
    fn same_candidate_yields_same_fingerprint() {
        let c = make_candidate();
        assert_eq!(fingerprint(&c), fingerprint(&c));
        // Must be non-empty once implemented
        assert!(!fingerprint(&c).is_empty());
    }

    /// Changing reason_kind must change the fingerprint.
    #[test]
    fn different_reason_yields_different_fingerprint() {
        let mut a = make_candidate();
        let mut b = make_candidate();
        a.reason_kind = ReasonKind::Partner;
        b.reason_kind = ReasonKind::Terminal;
        assert_ne!(fingerprint(&a), fingerprint(&b));
    }

    /// Changing mesh name must change the fingerprint.
    #[test]
    fn different_mesh_yields_different_fingerprint() {
        let mut a = make_candidate();
        let mut b = make_candidate();
        a.mesh = "mesh-alpha".to_string();
        b.mesh = "mesh-beta".to_string();
        assert_ne!(fingerprint(&a), fingerprint(&b));
    }

    /// Changing partner_start/partner_end must change the fingerprint.
    #[test]
    fn different_partner_range_yields_different_fingerprint() {
        let mut a = make_candidate();
        let mut b = make_candidate();
        a.partner_start = Some(1);
        a.partner_end = Some(10);
        b.partner_start = Some(20);
        b.partner_end = Some(30);
        assert_ne!(fingerprint(&a), fingerprint(&b));
    }

    /// Changing trigger extents must change the fingerprint so a later
    /// whole-file edit is not suppressed by an earlier anchor read.
    #[test]
    fn different_trigger_range_yields_different_fingerprint() {
        let mut a = make_candidate();
        let mut b = make_candidate();
        a.trigger_start = Some(1);
        a.trigger_end = Some(3);
        b.trigger_start = None;
        b.trigger_end = None;
        assert_ne!(fingerprint(&a), fingerprint(&b));
    }

    /// Any change to command text must NOT change the fingerprint — command is
    /// rendered text and is intentionally excluded from the hash.
    #[test]
    fn whitespace_in_command_does_not_change_fingerprint() {
        let mut a = make_candidate();
        let mut b = make_candidate();
        a.command = "git mesh add foo src/bar.rs#L1-10".to_string();
        b.command = "  completely different command text  ".to_string();
        assert_eq!(fingerprint(&a), fingerprint(&b));
    }

    /// Changing new_blob must change the fingerprint (blob identity matters).
    #[test]
    fn blob_id_change_yields_different_fingerprint() {
        let mut a = make_candidate();
        let mut b = make_candidate();
        a.new_blob = Some("oid-a".into());
        b.new_blob = Some("oid-b".into());
        assert_ne!(fingerprint(&a), fingerprint(&b));
    }

    /// FNV-64 produces a 64-bit hash → 16 lowercase hex characters.
    #[test]
    fn format_is_lowercase_hex_16_chars() {
        let c = make_candidate();
        let fp = fingerprint(&c);
        assert_eq!(fp.len(), 16, "expected 16 hex chars, got {:?}", fp);
        assert!(fp.chars().all(|ch| ch.is_ascii_hexdigit() && !ch.is_uppercase()),
            "fingerprint must be lowercase hex, got {:?}", fp);
    }

    /// fnv64 is exercised in tests to prevent dead_code warnings.
    #[test]
    fn fnv64_is_deterministic() {
        assert_eq!(fnv64(b"hello"), fnv64(b"hello"));
        assert_ne!(fnv64(b"hello"), fnv64(b"world"));
    }
}

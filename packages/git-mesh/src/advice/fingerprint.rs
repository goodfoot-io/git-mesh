//! FNV-64 fingerprinting for advice candidates.
//!
//! The fingerprint hashes structural fields only — wording, whitespace, and
//! render timestamps do not affect the result.

use crate::advice::candidates::Candidate;

/// FNV-64 hash over byte sequence. Same algorithm as `blake_short` in flush.rs.
fn fnv64(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

/// Feed a string slice into the running hash state, terminated by `\n`.
fn feed(h: &mut u64, s: &str) {
    for &b in s.as_bytes() {
        *h ^= b as u64;
        *h = h.wrapping_mul(0x100000001b3);
    }
    // newline separator to prevent boundary collisions
    *h ^= b'\n' as u64;
    *h = h.wrapping_mul(0x100000001b3);
}

/// Return a lowercase-hex FNV-64 fingerprint of the structural fields of `c`.
///
/// Hash inputs (fixed order, each terminated by `\n`):
/// 1. reason_kind string
/// 2. mesh name
/// 3. partner_path
/// 4. partner_start (decimal, or "0")
/// 5. partner_end (decimal, or "0")
/// 6. trigger_path
/// 7. partner_marker
/// 8. command (ASCII whitespace trimmed before hashing)
/// 9. old_path (empty — field not yet on Candidate)
/// 10. new_path (empty — field not yet on Candidate)
/// 11. old_blob (empty — field not yet on Candidate)
/// 12. new_blob (empty — field not yet on Candidate)
#[allow(dead_code)]
pub fn fingerprint(c: &Candidate) -> String {
    let mut h: u64 = 0xcbf29ce484222325;

    feed(&mut h, c.reason_kind.as_str());
    feed(&mut h, &c.mesh);
    feed(&mut h, &c.partner_path);
    feed(&mut h, &c.partner_start.map(|v| v.to_string()).unwrap_or_else(|| "0".to_string()));
    feed(&mut h, &c.partner_end.map(|v| v.to_string()).unwrap_or_else(|| "0".to_string()));
    feed(&mut h, &c.trigger_path);
    feed(&mut h, &c.partner_marker);
    // Trim ASCII whitespace from command before hashing — canonicalizes
    // leading/trailing spaces that may appear in generated command strings.
    feed(&mut h, c.command.trim());
    // Fields 9-12: old_path, new_path, old_blob, new_blob not yet on Candidate.
    feed(&mut h, "");
    feed(&mut h, "");
    feed(&mut h, "");
    feed(&mut h, "");

    // Suppress unused warning on fnv64 — it's kept for readability
    let _ = fnv64;

    format!("{h:016x}")
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::advice::intersections::{Density, ReasonKind};

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
            partner_marker: String::new(),
            partner_clause: String::new(),
            density: Density::L0,
            command: String::new(),
            excerpt_of_path: String::new(),
            excerpt_start: None,
            excerpt_end: None,
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

    /// Leading/trailing whitespace in command must not change the fingerprint
    /// (the implementation canonicalizes command text before hashing).
    #[test]
    fn whitespace_in_command_does_not_change_fingerprint() {
        let mut a = make_candidate();
        let mut b = make_candidate();
        a.command = "git mesh add foo src/bar.rs#L1-10".to_string();
        b.command = "  git mesh add foo src/bar.rs#L1-10  ".to_string();
        assert_eq!(fingerprint(&a), fingerprint(&b));
    }

    /// Changing the command text that encodes blob identity (e.g. an OID
    /// embedded in command) must change the fingerprint.
    ///
    /// Note: Candidate does not yet carry `old_blob`/`new_blob` fields
    /// (Sub-card C will add them). Until then, blob identity is tested via
    /// `command` text: if the command references different blob OIDs, the
    /// fingerprint must differ. When `old_blob`/`new_blob` fields are added
    /// in Sub-card C, this test should be updated to use those fields directly.
    #[test]
    fn blob_id_change_yields_different_fingerprint() {
        let mut a = make_candidate();
        let mut b = make_candidate();
        // Embed distinct blob OIDs in command text as a proxy for blob identity
        a.command = "git mesh add foo src/x.rs # blob:aabbccdd".to_string();
        b.command = "git mesh add foo src/x.rs # blob:11223344".to_string();
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
}

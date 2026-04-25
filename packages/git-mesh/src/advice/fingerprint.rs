//! FNV-64 fingerprinting for advice candidates.
//!
//! The fingerprint hashes structural fields only — wording, whitespace, and
//! render timestamps do not affect the result.

use crate::advice::candidates::Candidate;

/// Return a lowercase-hex FNV-64 fingerprint of the structural fields of `c`.
///
/// # Note
/// This stub returns an empty string. Phase C will implement the FNV-64 hash
/// over the twelve fixed fields listed in the plan §Phase 6.
#[allow(dead_code)]
pub fn fingerprint(_c: &Candidate) -> String {
    // TODO Phase C
    String::new()
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::advice::intersections::{Density, ReasonKind};

    /// Build a minimal Candidate with all structural fields set to defaults.
    #[allow(dead_code)]
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
    #[ignore]
    fn same_candidate_yields_same_fingerprint() {
        let c = make_candidate();
        assert_eq!(fingerprint(&c), fingerprint(&c));
        // Must be non-empty once implemented
        assert!(!fingerprint(&c).is_empty());
    }

    /// Changing reason_kind must change the fingerprint.
    #[test]
    #[ignore]
    fn different_reason_yields_different_fingerprint() {
        let mut a = make_candidate();
        let mut b = make_candidate();
        a.reason_kind = ReasonKind::Partner;
        b.reason_kind = ReasonKind::Terminal;
        assert_ne!(fingerprint(&a), fingerprint(&b));
    }

    /// Changing mesh name must change the fingerprint.
    #[test]
    #[ignore]
    fn different_mesh_yields_different_fingerprint() {
        let mut a = make_candidate();
        let mut b = make_candidate();
        a.mesh = "mesh-alpha".to_string();
        b.mesh = "mesh-beta".to_string();
        assert_ne!(fingerprint(&a), fingerprint(&b));
    }

    /// Changing partner_start/partner_end must change the fingerprint.
    #[test]
    #[ignore]
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
    #[ignore]
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
    #[ignore]
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
    #[ignore]
    fn format_is_lowercase_hex_16_chars() {
        let c = make_candidate();
        let fp = fingerprint(&c);
        assert_eq!(fp.len(), 16, "expected 16 hex chars, got {:?}", fp);
        assert!(fp.chars().all(|ch| ch.is_ascii_hexdigit() && !ch.is_uppercase()),
            "fingerprint must be lowercase hex, got {:?}", fp);
    }
}

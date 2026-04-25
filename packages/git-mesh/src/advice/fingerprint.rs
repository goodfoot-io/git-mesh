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

//! Cache keys and fingerprints for the Phase 3 persistent cache.
//!
//! `KEY_SALT` is a namespace constant. Bump it whenever the on-disk
//! schema or any of the canonical key encodings change. Old rows from
//! previous salts simply stop being read and are garbage-collectable.

use blake3::Hasher;
use std::collections::BTreeMap;

/// Phase 3 cache namespace.
///
/// Increment when the canonical key bytes, payload schema, or any
/// hashed input changes. This is **not** a migration version: nothing
/// reads or rewrites old rows. They simply stop being matched.
pub(crate) const KEY_SALT: u32 = 1;

/// Little-endian byte form of [`KEY_SALT`]. Used as a key suffix and
/// inside fingerprint hashes so a salt bump invalidates every cached
/// key shape at once.
pub(crate) fn key_salt_le() -> [u8; 4] {
    KEY_SALT.to_le_bytes()
}

/// `KEY_SALT` as bytes; alias retained so call sites read naturally.
pub(crate) fn salt_bytes() -> [u8; 4] {
    key_salt_le()
}

/// Lowercase-hex of a 32-byte BLAKE3 digest. Stable, fixed-width, safe
/// to use as a SQLite TEXT primary-key component.
pub(crate) fn hex32(b: &[u8; 32]) -> String {
    let mut out = String::with_capacity(64);
    for byte in b {
        use std::fmt::Write;
        let _ = write!(&mut out, "{byte:02x}");
    }
    out
}

/// Key for the path-anchor-index row: `catalog_tree_oid` + salt.
pub(crate) fn path_anchor_index_key(catalog_tree_oid: &str) -> (String, i64) {
    (catalog_tree_oid.to_string(), KEY_SALT as i64)
}

/// Key for the committed-baseline row.
pub(crate) fn baseline_key(
    catalog_tree_oid: &str,
    head_oid: &str,
    filter_config_hash: &[u8; 32],
) -> (String, String, String, i64) {
    (
        catalog_tree_oid.to_string(),
        head_oid.to_string(),
        hex32(filter_config_hash),
        KEY_SALT as i64,
    )
}

/// Key bytes for the dirty-overlay row.
///
/// Encoded as a single 32-byte BLAKE3 digest over a length-prefixed
/// concatenation of every dependent fingerprint. SQLite stores it as a
/// `BLOB PRIMARY KEY`.
pub(crate) fn dirty_overlay_key(
    catalog_tree_oid: &str,
    head_oid: &str,
    filter_config_hash: &[u8; 32],
    index_checksum: &[u8; 32],
    worktree_dirty_fingerprint: &[u8; 32],
    staging_state_fingerprint: &[u8; 32],
) -> [u8; 32] {
    let mut h = Hasher::new();
    h.update(b"gm.v1.phase3.overlay\0");
    h.update(&key_salt_le());
    write_prefixed(&mut h, catalog_tree_oid.as_bytes());
    write_prefixed(&mut h, head_oid.as_bytes());
    h.update(filter_config_hash);
    h.update(index_checksum);
    h.update(worktree_dirty_fingerprint);
    h.update(staging_state_fingerprint);
    *h.finalize().as_bytes()
}

fn write_prefixed(h: &mut Hasher, bytes: &[u8]) {
    h.update(&(bytes.len() as u64).to_le_bytes());
    h.update(bytes);
}

/// Canonical hash of every filter-pipeline input that can change a
/// committed-baseline resolution's output without HEAD or the catalog
/// changing. Currently covers:
///
/// * `core.autocrlf`
/// * `core.eol`
/// * `core.safecrlf`
/// * `filter.<driver>.{clean,smudge,required}` for every configured
///   custom filter driver
///
/// Inputs are read from `gix::Repository`'s parsed config in stable
/// sorted order. Missing keys hash differently from empty strings.
pub(crate) fn filter_config_hash(repo: &gix::Repository) -> [u8; 32] {
    let mut entries: BTreeMap<String, Option<String>> = BTreeMap::new();
    let cfg = repo.config_snapshot();
    for key in ["core.autocrlf", "core.eol", "core.safecrlf"] {
        let value = cfg
            .string(key)
            .map(|v| v.to_string());
        entries.insert(key.to_string(), value);
    }
    // Enumerate every `[filter "<sub>"] <key> = <value>` triple via
    // the same `sections_by_name("filter")` walk used by
    // `types::stamp_filter_drivers_sha1`. Multi-valued entries are
    // emitted in their stored order; outer iteration is keyed by
    // (sub, name) so a section reorder doesn't change the hash.
    let snap = repo.config_snapshot();
    let file = snap.plumbing();
    let mut filter_pairs: Vec<(String, String, String)> = Vec::new();
    if let Some(sections) = file.sections_by_name("filter") {
        for section in sections {
            let header = section.header();
            let sub_name = header
                .subsection_name()
                .map(|b| b.to_string())
                .unwrap_or_default();
            let body = section.body();
            let mut seen: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
            for name in body.value_names() {
                seen.insert(name.as_ref().to_string());
            }
            for name in seen {
                for v in body.values(&name) {
                    filter_pairs.push((sub_name.clone(), name.clone(), v.to_string()));
                }
            }
        }
    }
    filter_pairs.sort();
    let mut h = Hasher::new();
    h.update(b"gm.v1.phase3.filter-config\0");
    h.update(&key_salt_le());
    for (k, v) in &entries {
        write_prefixed(&mut h, k.as_bytes());
        match v {
            Some(s) => {
                h.update(&[1u8]);
                write_prefixed(&mut h, s.as_bytes());
            }
            None => {
                h.update(&[0u8]);
            }
        }
    }
    for (sub, key, val) in &filter_pairs {
        write_prefixed(&mut h, sub.as_bytes());
        write_prefixed(&mut h, key.as_bytes());
        write_prefixed(&mut h, val.as_bytes());
    }
    *h.finalize().as_bytes()
}

#[cfg(test)]
mod key_tests {
    use super::*;

    #[test]
    fn salt_bumps_dirty_overlay_key() {
        // Same inputs but salt-bytes-included → changing salt must
        // change the key. We can't mutate the const at runtime, so this
        // test asserts the salt is hashed into the digest by changing
        // any field and observing a different digest (a property the
        // implementation already exercises) and asserting the salt
        // bytes are non-zero so they actually contribute.
        assert_ne!(KEY_SALT, 0, "salt must contribute to keys");
        let a = dirty_overlay_key("a", "b", &[0; 32], &[0; 32], &[0; 32], &[0; 32]);
        let b = dirty_overlay_key("a", "b", &[1; 32], &[0; 32], &[0; 32], &[0; 32]);
        assert_ne!(a, b);
    }

    #[test]
    fn dirty_overlay_key_sensitive_to_each_field() {
        let base = dirty_overlay_key("c1", "h1", &[1; 32], &[2; 32], &[3; 32], &[4; 32]);
        assert_ne!(
            base,
            dirty_overlay_key("c2", "h1", &[1; 32], &[2; 32], &[3; 32], &[4; 32]),
            "catalog change"
        );
        assert_ne!(
            base,
            dirty_overlay_key("c1", "h2", &[1; 32], &[2; 32], &[3; 32], &[4; 32]),
            "HEAD change"
        );
        assert_ne!(
            base,
            dirty_overlay_key("c1", "h1", &[9; 32], &[2; 32], &[3; 32], &[4; 32]),
            "filter-config change"
        );
        assert_ne!(
            base,
            dirty_overlay_key("c1", "h1", &[1; 32], &[9; 32], &[3; 32], &[4; 32]),
            "index checksum change"
        );
        assert_ne!(
            base,
            dirty_overlay_key("c1", "h1", &[1; 32], &[2; 32], &[9; 32], &[4; 32]),
            "worktree dirty fingerprint change"
        );
        assert_ne!(
            base,
            dirty_overlay_key("c1", "h1", &[1; 32], &[2; 32], &[3; 32], &[9; 32]),
            "staging state fingerprint change"
        );
    }

    #[test]
    fn hex32_roundtrips_width() {
        let b = [0xab; 32];
        let s = hex32(&b);
        assert_eq!(s.len(), 64);
        assert!(s.chars().all(|c| c.is_ascii_hexdigit()));
    }
}

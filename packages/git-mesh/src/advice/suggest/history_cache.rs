//! Session-local history cache for the suggest pipeline (Step 4).
//!
//! Caches the result of `load_git_history` to `<session_dir>/history_cache.json`
//! so repeated flushes within the same session do not re-walk git history.
//!
//! # Invalidation
//!
//! The cache is rebuilt on any of:
//! - `schema_version != 1`
//! - `head_sha` mismatch with the current HEAD commit
//! - `cfg_fingerprint` mismatch (any change to the four history config knobs)
//! - current `seed_paths` is **not a subset** of cached `seed_paths`
//! - `complete == false`
//!
//! Cache read errors (missing file, truncated JSON, any I/O error) silently
//! degrade to a rebuild.  Cache write errors are logged to stderr at debug
//! level only; the in-memory index is always returned.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::advice::session::store::atomic_write;
use crate::advice::suggest::{HistoryIndex, SuggestConfig};

// ── Schema ────────────────────────────────────────────────────────────────────

const SCHEMA_VERSION: u32 = 1;

/// On-disk representation of the history cache.
#[derive(Serialize, Deserialize)]
pub(super) struct HistoryCacheFile {
    pub schema_version: u32,
    pub head_sha: String,
    pub seed_paths: Vec<String>,
    pub cfg_fingerprint: String,
    pub complete: bool,
    pub commits_by_path: BTreeMap<String, BTreeSet<String>>,
    pub commit_weight: BTreeMap<String, f64>,
    pub total_commits: usize,
    pub mass_refactor_cap: usize,
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Compute the FNV-64 hex fingerprint of the four history config knobs.
pub(super) fn cfg_fingerprint(cfg: &SuggestConfig) -> String {
    let input = format!(
        "{},{},{},{}",
        cfg.history_recency_commits,
        cfg.history_mass_refactor_default,
        cfg.history_half_life_commits,
        cfg.history_saturation,
    );
    fnv64_hex(input.as_bytes())
}

/// Try to load a valid `HistoryIndex` from the cache file.
///
/// Returns `None` on any cache miss (missing file, parse error, or any of the
/// 5 invalidation conditions listed in the module docs).
pub(super) fn try_load(
    session_dir: &Path,
    head_sha: &str,
    seed_paths: &[String],
    cfg: &SuggestConfig,
) -> Option<HistoryIndex> {
    let path = session_dir.join("history_cache.json");
    let bytes = std::fs::read(&path).ok()?;
    let cached: HistoryCacheFile = serde_json::from_slice(&bytes).ok()?;

    // Invalidation checks.
    if cached.schema_version != SCHEMA_VERSION {
        return None;
    }
    if cached.head_sha != head_sha {
        return None;
    }
    if cached.cfg_fingerprint != cfg_fingerprint(cfg) {
        return None;
    }
    // seed_paths must be a subset of cached seed_paths.
    let cached_set: BTreeSet<&str> = cached.seed_paths.iter().map(String::as_str).collect();
    if !seed_paths.iter().all(|p| cached_set.contains(p.as_str())) {
        return None;
    }
    if !cached.complete {
        return None;
    }

    Some(HistoryIndex {
        available: true,
        commits_by_path: cached.commits_by_path,
        commit_weight: cached.commit_weight,
        total_commits: cached.total_commits,
        mass_refactor_cap: cached.mass_refactor_cap,
    })
}

/// Write a `HistoryIndex` to `<session_dir>/history_cache.json` atomically.
///
/// Write errors are logged to stderr at debug level; the index is always used.
pub(super) fn try_write(
    session_dir: &Path,
    head_sha: &str,
    seed_paths: &[String],
    cfg: &SuggestConfig,
    index: &HistoryIndex,
) {
    let cache = HistoryCacheFile {
        schema_version: SCHEMA_VERSION,
        head_sha: head_sha.to_owned(),
        seed_paths: seed_paths.to_vec(),
        cfg_fingerprint: cfg_fingerprint(cfg),
        complete: true,
        commits_by_path: index.commits_by_path.clone(),
        commit_weight: index.commit_weight.clone(),
        total_commits: index.total_commits,
        mass_refactor_cap: index.mass_refactor_cap,
    };
    let bytes = match serde_json::to_vec(&cache) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("[git-mesh debug] history_cache: serialize failed: {e}");
            return;
        }
    };
    let dest = session_dir.join("history_cache.json");
    if let Err(e) = atomic_write(&dest, &bytes) {
        eprintln!("[git-mesh debug] history_cache: write failed: {e}");
    }
}

// ── Internal helpers ──────────────────────────────────────────────────────────

/// FNV-64 hash, lower-hex (16 digits).
fn fnv64_hex(input: &[u8]) -> String {
    let mut hash: u64 = 0xcbf29ce484222325;
    for b in input {
        hash ^= *b as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn default_cfg() -> SuggestConfig {
        SuggestConfig::default()
    }

    fn make_index() -> HistoryIndex {
        let mut cbp = BTreeMap::new();
        cbp.insert("a.rs".to_string(), ["c1".to_string()].into());
        let mut cw = BTreeMap::new();
        cw.insert("c1".to_string(), 1.0);
        HistoryIndex {
            available: true,
            commits_by_path: cbp,
            commit_weight: cw,
            total_commits: 1,
            mass_refactor_cap: 12,
        }
    }

    #[test]
    fn round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = default_cfg();
        let seed = vec!["a.rs".to_string()];
        let head = "abc123";
        let index = make_index();

        try_write(dir.path(), head, &seed, &cfg, &index);
        let loaded = try_load(dir.path(), head, &seed, &cfg).unwrap();
        assert!(loaded.available);
        assert_eq!(loaded.total_commits, 1);
        assert_eq!(loaded.mass_refactor_cap, 12);
        assert!(loaded.commits_by_path.contains_key("a.rs"));
    }

    #[test]
    fn miss_on_head_mismatch() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = default_cfg();
        let seed = vec!["a.rs".to_string()];
        let index = make_index();

        try_write(dir.path(), "head1", &seed, &cfg, &index);
        assert!(try_load(dir.path(), "head2", &seed, &cfg).is_none());
    }

    #[test]
    fn miss_on_seed_not_subset() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = default_cfg();
        let seed = vec!["a.rs".to_string()];
        let index = make_index();

        try_write(dir.path(), "head1", &seed, &cfg, &index);
        // New seed adds a path not in cache.
        let new_seed = vec!["a.rs".to_string(), "b.rs".to_string()];
        assert!(try_load(dir.path(), "head1", &new_seed, &cfg).is_none());
    }

    #[test]
    fn hit_when_seed_is_subset() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = default_cfg();
        // Write with two paths.
        let seed = vec!["a.rs".to_string(), "b.rs".to_string()];
        let index = make_index();

        try_write(dir.path(), "head1", &seed, &cfg, &index);
        // Load with subset of those paths — should hit.
        let subset = vec!["a.rs".to_string()];
        assert!(try_load(dir.path(), "head1", &subset, &cfg).is_some());
    }

    #[test]
    fn miss_on_cfg_fingerprint_change() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = default_cfg();
        let seed = vec!["a.rs".to_string()];
        let index = make_index();

        try_write(dir.path(), "head1", &seed, &cfg, &index);

        let mut cfg2 = default_cfg();
        cfg2.history_recency_commits = 999;
        assert!(try_load(dir.path(), "head1", &seed, &cfg2).is_none());
    }

    #[test]
    fn miss_on_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = default_cfg();
        let seed = vec!["a.rs".to_string()];
        assert!(try_load(dir.path(), "head1", &seed, &cfg).is_none());
    }

    #[test]
    fn miss_on_truncated_file() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = default_cfg();
        let seed = vec!["a.rs".to_string()];
        std::fs::write(dir.path().join("history_cache.json"), b"not json").unwrap();
        assert!(try_load(dir.path(), "head1", &seed, &cfg).is_none());
    }
}

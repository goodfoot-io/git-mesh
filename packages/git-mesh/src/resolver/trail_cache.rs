//! Cross-invocation rename-trail cache (Route C).
//!
//! Persists Pass 1's `(closed_paths, interesting_commits)` output across
//! invocations so hot-loop hook callers (`git mesh stale`) can skip the
//! per-anchor rename-trail rebuild on cache hit. Cache I/O failures
//! degrade silently to a miss; the caller never sees an error.
//!
//! ## Layout
//!
//! Root: `mesh_dir(repo).join("cache/rename-trail/v1/")`
//! Per-anchor: `<anchor_sha>.json`
//! Tempfile:   `<anchor_sha>.json.tmp.<pid>`
//!
//! ## Schema
//!
//! Line-oriented; first seven lines are header fields in fixed order,
//! then `seed <path>`, `closed <path>`, `interesting <sha>` records.

use crate::Result;
use crate::git;
use crate::types::CopyDetection;
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::io::Write;

/// Every component that must match for a cache hit.
pub(crate) struct TrailCacheKey {
    pub anchor_sha: String,
    pub head_sha: String,
    pub copy_detection: CopyDetection,
    pub rename_budget: usize,
    pub candidate_seed_hash: [u8; 32],
    pub replace_refs_hash: [u8; 32],
    pub git_config_hash: [u8; 32],
}

/// The cached Pass 1 output.
pub(crate) struct TrailCacheEntry {
    pub seed: Vec<String>,
    pub closed: HashSet<String>,
    pub interesting: HashSet<String>,
}

pub(crate) struct GcReport {
    pub removed_orphans: usize,
    pub removed_stale_tmp: usize,
}

/// Compute the cache key from all inputs that affect the rename trail.
pub(crate) fn compute_key(
    repo: &gix::Repository,
    anchor_sha: &str,
    head_sha: &str,
    copy_detection: CopyDetection,
    seed: &HashSet<String>,
) -> Result<TrailCacheKey> {
    let candidate_seed_hash = hash_sorted_paths(seed);
    let replace_refs_hash = hash_replace_refs(repo)?;
    let git_config_hash = hash_git_config(repo)?;
    let rename_budget = crate::resolver::walker::rename_budget();

    Ok(TrailCacheKey {
        anchor_sha: anchor_sha.to_string(),
        head_sha: head_sha.to_string(),
        copy_detection,
        rename_budget,
        candidate_seed_hash,
        replace_refs_hash,
        git_config_hash,
    })
}

/// Load a cache entry. Returns `Ok(Some(entry))` on hit, `Ok(None)` on
/// miss (file absent, key mismatch, or parse failure).
pub(crate) fn load(
    repo: &gix::Repository,
    key: &TrailCacheKey,
) -> Result<Option<TrailCacheEntry>> {
    let path = cache_file_path(repo, &key.anchor_sha);
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(crate::Error::Git(format!("trail_cache read: {e}"))),
    };
    Ok(parse_and_validate(&text, key))
}

/// Write a cache entry atomically: tmp → fsync → rename.
pub(crate) fn store(
    repo: &gix::Repository,
    key: &TrailCacheKey,
    entry: &TrailCacheEntry,
) -> Result<()> {
    let dir = cache_dir(repo);
    std::fs::create_dir_all(&dir)
        .map_err(|e| crate::Error::Git(format!("trail_cache mkdir: {e}")))?;

    let final_path = cache_file_path(repo, &key.anchor_sha);
    let tmp_path = dir.join(format!(
        "{}.json.tmp.{}",
        key.anchor_sha,
        std::process::id()
    ));

    let content = serialize(key, entry);

    let mut f = std::fs::File::create(&tmp_path)
        .map_err(|e| crate::Error::Git(format!("trail_cache tmp create: {e}")))?;
    f.write_all(content.as_bytes())
        .map_err(|e| crate::Error::Git(format!("trail_cache tmp write: {e}")))?;
    f.sync_all()
        .map_err(|e| crate::Error::Git(format!("trail_cache tmp sync: {e}")))?;
    drop(f);

    std::fs::rename(&tmp_path, &final_path)
        .map_err(|e| crate::Error::Git(format!("trail_cache rename: {e}")))?;

    Ok(())
}

/// Delete the cache file for a single anchor (called by compact on CAS-advance).
pub(crate) fn clear(repo: &gix::Repository, anchor_sha: &str) -> Result<()> {
    let path = cache_file_path(repo, anchor_sha);
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(crate::Error::Git(format!("trail_cache clear: {e}"))),
    }
}

/// Sweep orphan `<sha>.json` files whose anchor is not in `live_anchors`
/// and `*.tmp.*` files older than 1 hour.
pub(crate) fn gc(
    repo: &gix::Repository,
    live_anchors: &HashSet<String>,
) -> Result<GcReport> {
    let dir = cache_dir(repo);
    let mut removed_orphans = 0usize;
    let mut removed_stale_tmp = 0usize;

    let entries = match std::fs::read_dir(&dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(GcReport { removed_orphans: 0, removed_stale_tmp: 0 });
        }
        Err(e) => return Err(crate::Error::Git(format!("trail_cache gc readdir: {e}"))),
    };

    let one_hour = std::time::Duration::from_secs(3600);
    let now = std::time::SystemTime::now();

    for entry in entries.flatten() {
        let name = entry.file_name();
        let name_str = name.to_string_lossy();

        if name_str.contains(".tmp.") {
            // Stale tmp: remove if older than 1 hour.
            if let Ok(meta) = entry.metadata()
                && let Ok(modified) = meta.modified()
                && now.duration_since(modified).unwrap_or_default() > one_hour
            {
                let _ = std::fs::remove_file(entry.path());
                removed_stale_tmp += 1;
            }
        } else if name_str.ends_with(".json") {
            // Orphan: anchor sha not in live set.
            let anchor_sha = name_str.trim_end_matches(".json");
            if !live_anchors.contains(anchor_sha) {
                let _ = std::fs::remove_file(entry.path());
                removed_orphans += 1;
            }
        }
    }

    Ok(GcReport { removed_orphans, removed_stale_tmp })
}

// ── internals ───────────────────────────────────────────────────────────────

fn cache_dir(repo: &gix::Repository) -> std::path::PathBuf {
    git::mesh_dir(repo).join("cache/rename-trail/v1")
}

fn cache_file_path(repo: &gix::Repository, anchor_sha: &str) -> std::path::PathBuf {
    cache_dir(repo).join(format!("{anchor_sha}.json"))
}

fn copy_detection_str(cd: CopyDetection) -> &'static str {
    match cd {
        CopyDetection::Off => "off",
        CopyDetection::SameCommit => "same-commit",
        CopyDetection::AnyFileInCommit => "any-file-in-commit",
        CopyDetection::AnyFileInRepo => "any-file-in-repo",
    }
}

fn parse_copy_detection(s: &str) -> Option<CopyDetection> {
    match s {
        "off" => Some(CopyDetection::Off),
        "same-commit" => Some(CopyDetection::SameCommit),
        "any-file-in-commit" => Some(CopyDetection::AnyFileInCommit),
        "any-file-in-repo" => Some(CopyDetection::AnyFileInRepo),
        _ => None,
    }
}

fn hex_str(bytes: &[u8; 32]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn parse_hex32(s: &str) -> Option<[u8; 32]> {
    if s.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, chunk) in s.as_bytes().chunks(2).enumerate() {
        let hi = (chunk[0] as char).to_digit(16)?;
        let lo = (chunk[1] as char).to_digit(16)?;
        out[i] = (hi * 16 + lo) as u8;
    }
    Some(out)
}

fn serialize(key: &TrailCacheKey, entry: &TrailCacheEntry) -> String {
    let mut s = String::new();
    s.push_str(&format!("anchor_sha {}\n", key.anchor_sha));
    s.push_str(&format!("head_sha {}\n", key.head_sha));
    s.push_str(&format!(
        "copy_detection {}\n",
        copy_detection_str(key.copy_detection)
    ));
    s.push_str(&format!("rename_budget {}\n", key.rename_budget));
    s.push_str(&format!(
        "candidate_seed_hash {}\n",
        hex_str(&key.candidate_seed_hash)
    ));
    s.push_str(&format!(
        "replace_refs_hash {}\n",
        hex_str(&key.replace_refs_hash)
    ));
    s.push_str(&format!(
        "git_config_hash {}\n",
        hex_str(&key.git_config_hash)
    ));
    for path in &entry.seed {
        s.push_str(&format!("seed {path}\n"));
    }
    let mut closed: Vec<&String> = entry.closed.iter().collect();
    closed.sort();
    for path in closed {
        s.push_str(&format!("closed {path}\n"));
    }
    let mut interesting: Vec<&String> = entry.interesting.iter().collect();
    interesting.sort();
    for sha in interesting {
        s.push_str(&format!("interesting {sha}\n"));
    }
    s
}

fn parse_and_validate(text: &str, key: &TrailCacheKey) -> Option<TrailCacheEntry> {
    let mut lines = text.lines();

    macro_rules! header {
        ($prefix:expr) => {{
            let line = lines.next()?;
            let rest = line.strip_prefix($prefix)?;
            rest.to_string()
        }};
    }

    let anchor_sha = header!("anchor_sha ");
    let head_sha = header!("head_sha ");
    let copy_detection_s = header!("copy_detection ");
    let rename_budget_s = header!("rename_budget ");
    let candidate_seed_hash_s = header!("candidate_seed_hash ");
    let replace_refs_hash_s = header!("replace_refs_hash ");
    let git_config_hash_s = header!("git_config_hash ");

    // Parse and validate each key field.
    if anchor_sha != key.anchor_sha {
        return None;
    }
    if head_sha != key.head_sha {
        return None;
    }
    let copy_detection = parse_copy_detection(&copy_detection_s)?;
    if copy_detection != key.copy_detection {
        return None;
    }
    let rename_budget: usize = rename_budget_s.parse().ok()?;
    if rename_budget != key.rename_budget {
        return None;
    }
    let candidate_seed_hash = parse_hex32(&candidate_seed_hash_s)?;
    if candidate_seed_hash != key.candidate_seed_hash {
        return None;
    }
    let replace_refs_hash = parse_hex32(&replace_refs_hash_s)?;
    if replace_refs_hash != key.replace_refs_hash {
        return None;
    }
    let git_config_hash = parse_hex32(&git_config_hash_s)?;
    if git_config_hash != key.git_config_hash {
        return None;
    }

    // Parse data records.
    let mut seed = Vec::new();
    let mut closed = HashSet::new();
    let mut interesting = HashSet::new();

    for line in lines {
        if let Some(path) = line.strip_prefix("seed ") {
            seed.push(path.to_string());
        } else if let Some(path) = line.strip_prefix("closed ") {
            closed.insert(path.to_string());
        } else if let Some(sha) = line.strip_prefix("interesting ") {
            interesting.insert(sha.to_string());
        } else if line.is_empty() {
            continue;
        } else {
            return None; // Unknown record type → miss.
        }
    }

    Some(TrailCacheEntry { seed, closed, interesting })
}

fn hash_sorted_paths(paths: &HashSet<String>) -> [u8; 32] {
    let mut sorted: Vec<&String> = paths.iter().collect();
    sorted.sort();
    let joined = sorted
        .iter()
        .map(|s| s.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    let mut h = Sha256::new();
    h.update(joined.as_bytes());
    h.finalize().into()
}

fn hash_replace_refs(repo: &gix::Repository) -> Result<[u8; 32]> {
    let work_dir = repo
        .workdir()
        .unwrap_or_else(|| std::path::Path::new("."));
    let out = std::process::Command::new("git")
        .current_dir(work_dir)
        .args(["for-each-ref", "refs/replace/"])
        .output()
        .map_err(|e| crate::Error::Git(format!("for-each-ref: {e}")))?;
    let mut h = Sha256::new();
    h.update(&out.stdout);
    Ok(h.finalize().into())
}

fn hash_git_config(repo: &gix::Repository) -> Result<[u8; 32]> {
    let config_keys = [
        "diff.renames",
        "diff.algorithm",
        "diff.renameLimit",
        "core.ignoreCase",
        "core.precomposeUnicode",
        "log.follow",
        "i18n.logOutputEncoding",
    ];
    let work_dir = repo
        .workdir()
        .unwrap_or_else(|| std::path::Path::new("."));

    let mut lines = Vec::new();
    for key in &config_keys {
        let out = std::process::Command::new("git")
            .current_dir(work_dir)
            .args(["config", "--get", key])
            .output()
            .map_err(|e| crate::Error::Git(format!("git config: {e}")))?;
        let val = if out.status.success() {
            String::from_utf8_lossy(&out.stdout).trim_end().to_string()
        } else {
            String::new()
        };
        lines.push(format!("{key}={val}"));
    }

    let joined = lines.join("\n");
    let mut h = Sha256::new();
    h.update(joined.as_bytes());
    Ok(h.finalize().into())
}

#[cfg(test)]
mod trail_cache_tests {
    use super::*;
    use std::process::Command;
    use tempfile::tempdir;

    fn run_git(dir: &std::path::Path, args: &[&str]) {
        let out = Command::new("git")
            .current_dir(dir)
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn rev_parse(dir: &std::path::Path, refspec: &str) -> String {
        String::from_utf8(
            Command::new("git")
                .current_dir(dir)
                .args(["rev-parse", refspec])
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap()
        .trim()
        .to_string()
    }

    fn init_repo() -> (tempfile::TempDir, gix::Repository) {
        let td = tempdir().unwrap();
        let dir = td.path();
        run_git(dir, &["init", "--initial-branch=main"]);
        run_git(dir, &["config", "user.email", "t@t"]);
        run_git(dir, &["config", "user.name", "t"]);
        run_git(dir, &["config", "commit.gpgsign", "false"]);
        // Need at least one commit for HEAD to exist.
        std::fs::write(dir.join("a.txt"), "hello\n").unwrap();
        run_git(dir, &["add", "."]);
        run_git(dir, &["commit", "-m", "init"]);
        let repo = gix::open(dir).unwrap();
        (td, repo)
    }

    fn make_key(repo: &gix::Repository, anchor_sha: &str, seed: &HashSet<String>) -> TrailCacheKey {
        let head_sha = git::head_oid(repo).unwrap();
        compute_key(repo, anchor_sha, &head_sha, CopyDetection::Off, seed).unwrap()
    }

    fn make_entry(seed: &[&str], closed: &[&str], interesting: &[&str]) -> TrailCacheEntry {
        TrailCacheEntry {
            seed: seed.iter().map(|s| s.to_string()).collect(),
            closed: closed.iter().map(|s| s.to_string()).collect(),
            interesting: interesting.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn roundtrip() {
        let (_td, repo) = init_repo();
        let dir = _td.path();
        let anchor_sha = rev_parse(dir, "HEAD");
        let mut seed = HashSet::new();
        seed.insert("foo.rs".to_string());

        let key = make_key(&repo, &anchor_sha, &seed);
        let entry = make_entry(
            &["foo.rs"],
            &["foo.rs", "bar.rs"],
            &["aabbcc1122334455667788990011223344556677889900112233445566778899aa"],
        );

        store(&repo, &key, &entry).unwrap();
        let loaded = load(&repo, &key).unwrap().expect("should hit");

        assert_eq!(loaded.seed, entry.seed);
        assert_eq!(loaded.closed, entry.closed);
        assert_eq!(loaded.interesting, entry.interesting);
    }

    #[test]
    fn key_anchor_sha_mismatch_is_miss() {
        let (_td, repo) = init_repo();
        let dir = _td.path();
        let anchor_sha = rev_parse(dir, "HEAD");
        let mut seed = HashSet::new();
        seed.insert("foo.rs".to_string());

        let key = make_key(&repo, &anchor_sha, &seed);
        let entry = make_entry(&["foo.rs"], &["foo.rs"], &[]);
        store(&repo, &key, &entry).unwrap();

        // Use a different anchor_sha for loading.
        let mut key2 = make_key(&repo, &anchor_sha, &seed);
        key2.anchor_sha = "0".repeat(40);
        assert!(load(&repo, &key2).unwrap().is_none());
    }

    #[test]
    fn key_head_sha_mismatch_is_miss() {
        let (_td, repo) = init_repo();
        let dir = _td.path();
        let anchor_sha = rev_parse(dir, "HEAD");
        let mut seed = HashSet::new();
        seed.insert("foo.rs".to_string());

        let key = make_key(&repo, &anchor_sha, &seed);
        let entry = make_entry(&["foo.rs"], &["foo.rs"], &[]);
        store(&repo, &key, &entry).unwrap();

        let mut key2 = make_key(&repo, &anchor_sha, &seed);
        key2.head_sha = "0".repeat(40);
        assert!(load(&repo, &key2).unwrap().is_none());
    }

    #[test]
    fn key_copy_detection_mismatch_is_miss() {
        let (_td, repo) = init_repo();
        let dir = _td.path();
        let anchor_sha = rev_parse(dir, "HEAD");
        let mut seed = HashSet::new();
        seed.insert("foo.rs".to_string());

        let key = make_key(&repo, &anchor_sha, &seed);
        let entry = make_entry(&["foo.rs"], &["foo.rs"], &[]);
        store(&repo, &key, &entry).unwrap();

        let mut key2 = make_key(&repo, &anchor_sha, &seed);
        key2.copy_detection = CopyDetection::SameCommit;
        assert!(load(&repo, &key2).unwrap().is_none());
    }

    #[test]
    fn key_rename_budget_mismatch_is_miss() {
        let (_td, repo) = init_repo();
        let dir = _td.path();
        let anchor_sha = rev_parse(dir, "HEAD");
        let mut seed = HashSet::new();
        seed.insert("foo.rs".to_string());

        let key = make_key(&repo, &anchor_sha, &seed);
        let entry = make_entry(&["foo.rs"], &["foo.rs"], &[]);
        store(&repo, &key, &entry).unwrap();

        let mut key2 = make_key(&repo, &anchor_sha, &seed);
        key2.rename_budget = key2.rename_budget.wrapping_add(1);
        assert!(load(&repo, &key2).unwrap().is_none());
    }

    #[test]
    fn key_candidate_seed_hash_mismatch_is_miss() {
        let (_td, repo) = init_repo();
        let dir = _td.path();
        let anchor_sha = rev_parse(dir, "HEAD");
        let mut seed = HashSet::new();
        seed.insert("foo.rs".to_string());

        let key = make_key(&repo, &anchor_sha, &seed);
        let entry = make_entry(&["foo.rs"], &["foo.rs"], &[]);
        store(&repo, &key, &entry).unwrap();

        // Different seed → different hash.
        let mut seed2 = HashSet::new();
        seed2.insert("other.rs".to_string());
        let key2 = make_key(&repo, &anchor_sha, &seed2);
        assert!(load(&repo, &key2).unwrap().is_none());
    }

    #[test]
    fn key_replace_refs_hash_mismatch_is_miss() {
        let (_td, repo) = init_repo();
        let dir = _td.path();
        let anchor_sha = rev_parse(dir, "HEAD");
        let mut seed = HashSet::new();
        seed.insert("foo.rs".to_string());

        let key = make_key(&repo, &anchor_sha, &seed);
        let entry = make_entry(&["foo.rs"], &["foo.rs"], &[]);
        store(&repo, &key, &entry).unwrap();

        let mut key2 = make_key(&repo, &anchor_sha, &seed);
        key2.replace_refs_hash = [0xffu8; 32];
        assert!(load(&repo, &key2).unwrap().is_none());
    }

    #[test]
    fn key_git_config_hash_mismatch_is_miss() {
        let (_td, repo) = init_repo();
        let dir = _td.path();
        let anchor_sha = rev_parse(dir, "HEAD");
        let mut seed = HashSet::new();
        seed.insert("foo.rs".to_string());

        let key = make_key(&repo, &anchor_sha, &seed);
        let entry = make_entry(&["foo.rs"], &["foo.rs"], &[]);
        store(&repo, &key, &entry).unwrap();

        let mut key2 = make_key(&repo, &anchor_sha, &seed);
        key2.git_config_hash = [0xaau8; 32];
        assert!(load(&repo, &key2).unwrap().is_none());
    }

    #[test]
    fn parse_failure_is_miss() {
        let (_td, repo) = init_repo();
        let dir = _td.path();
        let anchor_sha = rev_parse(dir, "HEAD");
        let mut seed = HashSet::new();
        seed.insert("foo.rs".to_string());

        let key = make_key(&repo, &anchor_sha, &seed);

        // Write garbage to the cache file.
        let cache_file = cache_file_path(&repo, &anchor_sha);
        std::fs::create_dir_all(cache_file.parent().unwrap()).unwrap();
        std::fs::write(&cache_file, "this is garbage\nnot a valid cache file\n").unwrap();

        let result = load(&repo, &key).unwrap();
        assert!(result.is_none(), "garbage file must be treated as miss");

        // On next store, the file is overwritten.
        let entry = make_entry(&["foo.rs"], &["foo.rs"], &[]);
        store(&repo, &key, &entry).unwrap();
        let loaded = load(&repo, &key).unwrap();
        assert!(loaded.is_some(), "valid write after garbage must succeed");
    }

    #[test]
    fn stale_tmp_is_reaped_but_live_and_orphan_logic_correct() {
        let (_td, repo) = init_repo();
        let dir = _td.path();
        let anchor_sha = rev_parse(dir, "HEAD");
        let mut seed = HashSet::new();
        seed.insert("foo.rs".to_string());

        let key = make_key(&repo, &anchor_sha, &seed);
        let entry = make_entry(&["foo.rs"], &["foo.rs"], &[]);

        // Store a live entry.
        store(&repo, &key, &entry).unwrap();

        // Create a stale tmp file with an old mtime.
        let tmp_path = cache_dir(&repo).join("deadbeef.json.tmp.12345");
        std::fs::write(&tmp_path, "stale").unwrap();
        // Set mtime to 2 hours ago via libc utimes.
        {
            use std::ffi::CString;
            let path_cstr = CString::new(tmp_path.to_str().unwrap()).unwrap();
            let two_hours_ago_secs = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs()
                .saturating_sub(7201);
            let times = [
                libc::timeval { tv_sec: two_hours_ago_secs as libc::time_t, tv_usec: 0 },
                libc::timeval { tv_sec: two_hours_ago_secs as libc::time_t, tv_usec: 0 },
            ];
            unsafe { libc::utimes(path_cstr.as_ptr(), times.as_ptr()) };
        }

        // Orphan: a cache file for an anchor not in live set.
        let orphan_path = cache_dir(&repo).join("orphansha.json");
        std::fs::write(&orphan_path, "orphan").unwrap();

        let mut live = HashSet::new();
        live.insert(anchor_sha.clone());

        let report = gc(&repo, &live).unwrap();
        assert_eq!(report.removed_orphans, 1, "orphan should be removed");
        assert_eq!(report.removed_stale_tmp, 1, "stale tmp should be removed");

        // Live entry still present.
        let loaded = load(&repo, &key).unwrap();
        assert!(loaded.is_some(), "live entry must survive gc");
    }
}

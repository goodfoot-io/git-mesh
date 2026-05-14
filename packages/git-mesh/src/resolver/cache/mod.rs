//! Content-addressed FS cache for `git mesh stale`.
//!
//! Single polymorphic surface: a [`Cache`] backed by an L1 in-memory map and
//! an L2 on-disk store under `<common_dir>/mesh/cache/v1/<kind>/<aa>/<rest>`,
//! keyed by BLAKE3 of canonical key bytes. Two [`Kind`]s today:
//! `RenameTrail`, `DriftLocus`.
//!
//! Persisted value framing: every L2 entry is bincode-serialized as
//! `(u8 format_version, Vec<String> bound_oids, V)`. The `bound_oids` slot
//! carries the oids the entry depends on for reachability — gc reads it to
//! decide whether the entry remains valid. Format version `0u8` is the
//! initial schema; on a version-byte mismatch (or any other parse failure)
//! the entry is treated as a miss and recomputed silently — no in-band
//! revocation, no warning log.
//!
//! See [`packages/git-mesh/plan/initial.md`](../../../plan/initial.md) for
//! the full design.

use crate::Result;
use crate::types::CopyDetection;
use crate::{Error, perf};
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

const FORMAT_VERSION: u8 = 0;

// ── Kind ───────────────────────────────────────────────────────────────────

/// Discriminant for cache subdirectories. Used as both an L1 key component
/// and as the on-disk path segment via [`Kind::as_dir`].
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub enum Kind {
    RenameTrail,
    DriftLocus,
}

impl Kind {
    pub fn as_dir(self) -> &'static str {
        match self {
            Kind::RenameTrail => "rename_trail",
            Kind::DriftLocus => "drift_locus",
        }
    }
}

// ── CacheKey trait ──────────────────────────────────────────────────────────

/// Trait implemented by each per-kind key struct. Implementors write a
/// domain-separation tag (`b"gm.v1.<kind>\0"`) followed by each field in
/// fixed network-byte-order. No `Hash`, no `bincode`, no serde for the key
/// path — the hash of `canonical_bytes` is the L1/L2 lookup key.
///
/// `bound_oids` returns the oids whose reachability the entry depends on.
/// gc reads these from the persisted payload and removes entries whose oids
/// are no longer reachable from any ref.
pub trait CacheKey {
    fn canonical_bytes(&self, out: &mut Vec<u8>);
    fn bound_oids(&self) -> Vec<String>;
}

// ── Per-kind key structs ────────────────────────────────────────────────────

/// Key for the `rename_trail` kind. Fields mirror the prior `TrailCacheKey`.
pub struct RenameTrailKey {
    pub anchor_sha: String,
    pub head_sha: String,
    pub copy_detection: CopyDetection,
    pub rename_budget: i64,
    pub candidate_seed_hash: [u8; 32],
    pub replace_refs_hash: [u8; 32],
    pub git_config_hash: [u8; 32],
}

impl CacheKey for RenameTrailKey {
    fn canonical_bytes(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(b"gm.v1.rename_trail\0");
        write_str(out, &self.anchor_sha);
        write_str(out, &self.head_sha);
        out.push(copy_detection_byte(self.copy_detection));
        out.extend_from_slice(&self.rename_budget.to_be_bytes());
        out.extend_from_slice(&self.candidate_seed_hash);
        out.extend_from_slice(&self.replace_refs_hash);
        out.extend_from_slice(&self.git_config_hash);
    }
    fn bound_oids(&self) -> Vec<String> {
        vec![self.anchor_sha.clone(), self.head_sha.clone()]
    }
}

/// Key for the `drift_locus` kind. Fields mirror the prior `DriftLocusCacheKey`.
pub struct DriftLocusKey {
    pub anchor_sha: String,
    pub path: String,
    pub blob_oid: String,
    pub range_start: u32,
    pub range_end: u32,
    pub copy_detection: CopyDetection,
    pub rename_budget: i64,
}

impl CacheKey for DriftLocusKey {
    fn canonical_bytes(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(b"gm.v1.drift_locus\0");
        write_str(out, &self.anchor_sha);
        write_str(out, &self.path);
        write_str(out, &self.blob_oid);
        out.extend_from_slice(&self.range_start.to_be_bytes());
        out.extend_from_slice(&self.range_end.to_be_bytes());
        out.push(copy_detection_byte(self.copy_detection));
        out.extend_from_slice(&self.rename_budget.to_be_bytes());
    }
    fn bound_oids(&self) -> Vec<String> {
        vec![self.anchor_sha.clone()]
    }
}

fn write_str(out: &mut Vec<u8>, s: &str) {
    let bytes = s.as_bytes();
    out.extend_from_slice(&(bytes.len() as u64).to_be_bytes());
    out.extend_from_slice(bytes);
}

fn copy_detection_byte(cd: CopyDetection) -> u8 {
    match cd {
        CopyDetection::Off => 0,
        CopyDetection::SameCommit => 1,
        CopyDetection::AnyFileInCommit => 2,
        CopyDetection::AnyFileInRepo => 3,
    }
}

// ── GcStats ─────────────────────────────────────────────────────────────────

#[derive(Default)]
pub struct GcStats {
    pub rename_trail_removed: usize,
    pub drift_locus_removed: usize,
}

// ── Outcome ─────────────────────────────────────────────────────────────────

/// Hit/miss outcome for a [`Cache::get_or_insert_with_outcome`] call. Callers
/// that drive per-kind `session.*-hits/misses` counters use this to attribute
/// the call without re-reading the global L1/L2 counters.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Outcome {
    L1Hit,
    L2Hit,
    Miss,
}

// ── Cache ───────────────────────────────────────────────────────────────────

/// Two-tier content-addressed cache. L1 is a [`Mutex<HashMap>`] of serialized
/// payloads keyed by `(Kind, blake3_hash)`; L2 is an on-disk store rooted at
/// `dir`. When `enabled == false` (env `GIT_MESH_CACHE=0` or
/// [`Cache::open_disabled`]), every [`Cache::get_or_insert_with`] call routes
/// straight to `compute` and skips both tiers.
type L1Map = HashMap<(Kind, [u8; 32]), Arc<[u8]>>;

pub struct Cache {
    dir: PathBuf,
    l1: Mutex<L1Map>,
    enabled: bool,
}

impl Cache {
    /// Open the cache rooted at `<common_dir>/mesh/cache/v1`. Honors
    /// `GIT_MESH_CACHE=0` to short-circuit to a disabled cache.
    pub fn open(repo: &gix::Repository) -> Result<Self> {
        if std::env::var("GIT_MESH_CACHE").as_deref() == Ok("0") {
            return Ok(Self::open_disabled());
        }
        let dir = crate::git::cache_dir(repo).join("v1");
        std::fs::create_dir_all(&dir)
            .map_err(|e| Error::Git(format!("create cache dir {dir:?}: {e}")))?;
        Ok(Cache {
            dir,
            l1: Mutex::new(HashMap::new()),
            enabled: true,
        })
    }

    /// Return a permanently-disabled cache. Calls to
    /// [`Cache::get_or_insert_with`] route straight to `compute` with no
    /// L1 or L2 I/O.
    pub fn open_disabled() -> Self {
        Cache {
            dir: PathBuf::new(),
            l1: Mutex::new(HashMap::new()),
            enabled: false,
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Cache the value produced by `compute` under `(kind, key)`. On L1 or L2
    /// hit, `compute` is not invoked; on miss, `compute` is invoked exactly
    /// once and the result is persisted to both tiers.
    pub fn get_or_insert_with<K, V, F>(&self, kind: Kind, key: &K, compute: F) -> Result<V>
    where
        K: CacheKey,
        V: Serialize + DeserializeOwned,
        F: FnOnce() -> Result<V>,
    {
        let (v, _) = self.get_or_insert_with_outcome(kind, key, compute)?;
        Ok(v)
    }

    /// Same as [`Cache::get_or_insert_with`] but also reports whether the
    /// call hit L1, hit L2, or missed (and ran `compute`). Used by callers
    /// that drive per-kind `session.*-hits/misses` counters.
    pub fn get_or_insert_with_outcome<K, V, F>(
        &self,
        kind: Kind,
        key: &K,
        compute: F,
    ) -> Result<(V, Outcome)>
    where
        K: CacheKey,
        V: Serialize + DeserializeOwned,
        F: FnOnce() -> Result<V>,
    {
        if !self.enabled {
            return Ok((compute()?, Outcome::Miss));
        }

        let mut buf = Vec::with_capacity(128);
        key.canonical_bytes(&mut buf);
        let hash: [u8; 32] = *blake3::hash(&buf).as_bytes();
        let map_key = (kind, hash);

        // L1 probe — release the mutex before deserializing.
        let l1_bytes = {
            let map = self.l1.lock().expect("cache L1 mutex poisoned");
            map.get(&map_key).cloned()
        };
        if let Some(bytes) = l1_bytes {
            perf::record_l1_hit();
            if let Some(v) = deserialize_payload::<V>(&bytes) {
                return Ok((v, Outcome::L1Hit));
            }
            // L1 holds bytes we wrote ourselves — a deserialize failure
            // here means schema drift between siblings in this process.
            // Treat as miss and overwrite.
        } else {
            perf::record_l1_miss();
        }

        // L2 probe.
        let path = self.l2_path(kind, &hash);
        let read_result = perf::time_l2_read(|| std::fs::read(&path));
        match read_result {
            Ok(bytes) => {
                perf::record_l2_bytes_read(bytes.len() as u64);
                if let Some(v) = deserialize_payload::<V>(&bytes) {
                    perf::record_l2_hit();
                    let arc: Arc<[u8]> = Arc::from(bytes.into_boxed_slice());
                    self.l1
                        .lock()
                        .expect("cache L1 mutex poisoned")
                        .insert(map_key, arc);
                    return Ok((v, Outcome::L2Hit));
                }
                // Parse failure → silent recompute (no in-band revocation).
                perf::record_l2_miss();
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::NotFound => {
                perf::record_l2_miss();
            }
            Err(e) => {
                return Err(Error::Git(format!(
                    "read cache entry {path:?}: {e}"
                )));
            }
        }

        // Miss path: compute, serialize with framing, write atomically.
        let value = compute()?;
        let bound_oids = key.bound_oids();
        let payload: (u8, Vec<String>, &V) = (FORMAT_VERSION, bound_oids, &value);
        let bytes = bincode::serialize(&payload)
            .map_err(|e| Error::Git(format!("bincode serialize cache value: {e}")))?;

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| Error::Git(format!("create cache parent {parent:?}: {e}")))?;
        }
        let parent = path.parent().expect("cache path has parent");
        perf::time_l2_write(|| -> Result<()> {
            let mut tmp = tempfile::NamedTempFile::new_in(parent)
                .map_err(|e| Error::Git(format!("create tempfile in {parent:?}: {e}")))?;
            use std::io::Write;
            tmp.write_all(&bytes)
                .map_err(|e| Error::Git(format!("write cache tempfile: {e}")))?;
            tmp.persist(&path)
                .map_err(|e| Error::Git(format!("persist cache entry {:?}: {}", path, e.error)))?;
            Ok(())
        })?;
        perf::record_l2_bytes_written(bytes.len() as u64);

        let arc: Arc<[u8]> = Arc::from(bytes.into_boxed_slice());
        self.l1
            .lock()
            .expect("cache L1 mutex poisoned")
            .insert(map_key, arc);

        Ok((value, Outcome::Miss))
    }

    fn l2_path(&self, kind: Kind, hash: &[u8; 32]) -> PathBuf {
        let hex = hex_encode(hash);
        self.dir.join(kind.as_dir()).join(&hex[0..2]).join(&hex[2..])
    }

    /// Sweep cache entries whose bound oids are no longer reachable from any
    /// ref. Reachability is determined via `git rev-list --all --objects`.
    pub fn gc(&self, repo: &gix::Repository) -> Result<GcStats> {
        let mut stats = GcStats::default();
        if !self.enabled {
            return Ok(stats);
        }
        let reachable = collect_reachable_oids(repo)?;

        for (kind, slot) in [
            (Kind::RenameTrail, &mut stats.rename_trail_removed),
            (Kind::DriftLocus, &mut stats.drift_locus_removed),
        ] {
            let kind_dir = self.dir.join(kind.as_dir());
            sweep_kind_dir(&kind_dir, &reachable, slot)?;
        }
        Ok(stats)
    }
}

fn sweep_kind_dir(
    kind_dir: &Path,
    reachable: &std::collections::HashSet<String>,
    removed: &mut usize,
) -> Result<()> {
    let read_dir = match std::fs::read_dir(kind_dir) {
        Ok(d) => d,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(Error::Git(format!("read_dir {kind_dir:?}: {e}"))),
    };
    for shard in read_dir {
        let shard = shard.map_err(|e| Error::Git(format!("read_dir entry: {e}")))?;
        if !shard.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        let shard_path = shard.path();
        let entries = std::fs::read_dir(&shard_path)
            .map_err(|e| Error::Git(format!("read_dir {shard_path:?}: {e}")))?;
        for entry in entries {
            let entry = entry.map_err(|e| Error::Git(format!("read_dir entry: {e}")))?;
            let path = entry.path();
            if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
                continue;
            }
            let bytes = match std::fs::read(&path) {
                Ok(b) => b,
                Err(_) => continue,
            };
            let bound = parse_bound_oids(&bytes);
            let should_remove = match bound {
                None => true, // malformed → reclaim
                Some(oids) => oids.iter().any(|o| !reachable.contains(o.as_str())),
            };
            if should_remove && std::fs::remove_file(&path).is_ok() {
                *removed += 1;
            }
        }
    }
    Ok(())
}

fn collect_reachable_oids(repo: &gix::Repository) -> Result<std::collections::HashSet<String>> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(repo.git_dir())
        .args(["rev-list", "--all", "--objects"])
        .output()
        .map_err(|e| Error::Git(format!("spawn git rev-list: {e}")))?;
    if !out.status.success() {
        return Err(Error::Git(format!(
            "git rev-list --all --objects failed: {}",
            String::from_utf8_lossy(&out.stderr)
        )));
    }
    let mut set = std::collections::HashSet::new();
    for line in out.stdout.split(|&b| b == b'\n') {
        if line.is_empty() {
            continue;
        }
        // Each line is `<oid>` or `<oid> <path>`; take the first whitespace-
        // separated token.
        let end = line.iter().position(|&b| b == b' ').unwrap_or(line.len());
        if let Ok(s) = std::str::from_utf8(&line[..end]) {
            set.insert(s.to_string());
        }
    }
    Ok(set)
}

fn deserialize_payload<V: DeserializeOwned>(bytes: &[u8]) -> Option<V> {
    // We can't borrow the V out of the tuple under bincode without owning it,
    // so deserialize the head (version, oids) and then re-deserialize the
    // value separately... bincode supports tuple-struct deserialization, so
    // it's actually simpler to deserialize the full (u8, Vec<String>, V).
    let parsed: bincode::Result<(u8, Vec<String>, V)> = bincode::deserialize(bytes);
    match parsed {
        Ok((v, _, value)) if v == FORMAT_VERSION => Some(value),
        _ => None,
    }
}

fn parse_bound_oids(bytes: &[u8]) -> Option<Vec<String>> {
    // Read the (u8, Vec<String>) head off a cursor; the trailing `V` bytes
    // remain in the cursor and are ignored.
    let mut cursor = std::io::Cursor::new(bytes);
    let version: u8 = bincode::deserialize_from(&mut cursor).ok()?;
    if version != FORMAT_VERSION {
        return None;
    }
    let oids: Vec<String> = bincode::deserialize_from(&mut cursor).ok()?;
    Some(oids)
}

fn hex_encode(bytes: &[u8; 32]) -> String {
    let mut s = String::with_capacity(64);
    for b in bytes {
        use std::fmt::Write;
        let _ = write!(s, "{b:02x}");
    }
    s
}

#[cfg(test)]
mod tests;

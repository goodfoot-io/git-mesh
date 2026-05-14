//! Changed-path Bloom filter reader for commit-graph BIDX/BDAT chunks.
//!
//! gix-commitgraph v0.35.0 does NOT parse Bloom filter chunks, so this module
//! reads them directly via `gix-chunk` from the mmap'd commit-graph file.
//!
//! # Format reference
//!
//! Git 2.39 commit-graph with `--changed-paths`:
//!
//! **BDAT header** (12 bytes = 3 x u32 BE):
//!   0..3: hash_version         (1 = SHA-1-based MurmurHash3)
//!   4..7: num_hashes           (7 hash functions)
//!   8..11: bits_per_entry      (10 bits per path entry)
//!
//! **BIDX** (`num_commits` x u32 BE):
//!   BIDX[i] = end offset of commit i's filter in the BDAT data area
//!   (data area = BDAT[12..]).  Start = 0 if i == 0 else BIDX[i-1].
//!   Filter length = end - start.
//!
//! **Hash** (MurmurHash3 32-bit, two seeds):
//!   hash0 = murmur3(0x293ae76f, path)
//!   hash1 = murmur3(0x7e646e2c, path)
//!   For i in 0..num_hashes: h = hash0 + i * hash1   (wrapping u32)
//!   Bit position: h % (filter_len * 8)
//!
//! No per-commit type byte; the filter data begins directly at the offset.

use gix::ObjectId;
use std::path::PathBuf;

/// A reader for the changed-path Bloom filter (BIDX/BDAT chunks) of a
/// single commit-graph file. Also provides OID-to-position lookup using
/// the OID fan (OIDF) and OID lookup (OIDL) chunks, avoiding the need
/// to open a separate `gix_commitgraph::File`.
pub(crate) struct CommitGraphBloom {
    data: memmap2::Mmap,
    bidx_start: usize,
    bdat_start: usize,
    num_hashes: u32,
    #[allow(dead_code)]
    bits_per_entry: u32,
    num_commits: u32,
    #[allow(dead_code)]
    path: PathBuf,
    /// Offset of the OID fan-out table (OIDF chunk).
    oidf_offset: usize,
    /// Offset of the OID lookup table (OIDL chunk).
    oidl_offset: usize,
    /// Hash length in bytes (20 for SHA-1, 32 for SHA-256).
    hash_len: usize,
}

impl CommitGraphBloom {
    /// Open the commit-graph file for `repo`, mmap it, parse chunks, and
    /// locate the BIDX/BDAT Bloom filter chunks.
    ///
    /// Returns `Err` with a descriptive message if the commit-graph is
    /// missing or lacks the required chunks.
    pub(crate) fn open(repo: &gix::Repository) -> Result<Self, String> {
        // Validate that a commit-graph exists via gix's own API.
        let _graph = repo.commit_graph().map_err(|_| {
            "Commit graph not found. Run: git commit-graph write --reachable --changed-paths".to_string()
        })?;

        // Build path to the commit-graph file.
        let info_dir = repo.objects.store_ref().path().join("info");
        let path = info_dir.join("commit-graph");

        let (data, path) = if path.exists() {
            (mmap_file(&path)?, path)
        } else {
            // Try the chain directory (objects/info/commit-graphs/).
            let chain_dir = info_dir.join("commit-graphs");
            let chain_file = chain_dir.join("commit-graph-chain");
            if chain_file.exists() {
                let chain = std::fs::read_to_string(&chain_file)
                    .map_err(|e| format!("Cannot read commit-graph chain at {}: {e}", chain_file.display()))?;
                let hash = chain
                    .lines()
                    .next()
                    .ok_or_else(|| format!("Empty commit-graph chain file at {}", chain_file.display()))?;
                let path = chain_dir.join(format!("graph-{hash}.graph"));
                (mmap_file(&path)?, path)
            } else {
                return Err(format!(
                    "Commit graph not found at {}. Run: git commit-graph write --reachable --changed-paths",
                    info_dir.display()
                ));
            }
        };

        // ----- 8-byte file header -----
        // bytes 0-3: signature "CGPH"
        // byte 4:   version (must be 1)
        // byte 5:   hash version
        // byte 6:   chunk count
        // byte 7:   base graph count (unused here)
        if &data[..4] != b"CGPH" {
            return Err(format!("Invalid commit-graph signature at {}", path.display()));
        }
        let _file_hash_version = data[5]; // 1 = SHA-1, 2 = SHA-256
        let chunk_count = data[6];

        // ----- Chunk index (at offset 8) -----
        let chunks = gix_chunk::file::Index::from_bytes(&data, 8, u32::from(chunk_count))
            .map_err(|e| format!("Failed to parse commit-graph chunk index: {e}"))?;

        // ----- Look up BIDX and BDAT -----
        let bidx_range = chunks.usize_offset_by_id(*b"BIDX").map_err(|_| {
            "Commit graph missing BIDX chunk. Run: git commit-graph write --reachable --changed-paths".to_string()
        })?;
        let bdat_range = chunks.usize_offset_by_id(*b"BDAT").map_err(|_| {
            "Commit graph missing BDAT chunk. Run: git commit-graph write --reachable --changed-paths".to_string()
        })?;

        // ----- BDAT global header (12 bytes = 3 x u32 BE) -----
        if data.len() <= bdat_range.start + 12 {
            return Err(format!("BDAT chunk too small at {}", path.display()));
        }
        let hdr_hash_version = u32::from_be_bytes(
            data[bdat_range.start..bdat_range.start + 4].try_into().unwrap(),
        );
        if hdr_hash_version != 1 {
            return Err(format!(
                "Unsupported Bloom filter hash version {hdr_hash_version} at {}",
                path.display()
            ));
        }
        let num_hashes =
            u32::from_be_bytes(data[bdat_range.start + 4..bdat_range.start + 8].try_into().unwrap());
        let bits_per_entry =
            u32::from_be_bytes(data[bdat_range.start + 8..bdat_range.start + 12].try_into().unwrap());

        // ----- OID fan table (OIDF) for position lookup -----
        let oidf_range = chunks.usize_offset_by_id(*b"OIDF").map_err(|_| {
            "Commit graph missing OIDF chunk".to_string()
        })?;
        let num_commits =
            u32::from_be_bytes(data[oidf_range.start + 255 * 4..oidf_range.start + 256 * 4].try_into().unwrap());

        // ----- OID lookup table (OIDL) for position lookup -----
        let oidl_range = chunks.usize_offset_by_id(*b"OIDL").map_err(|_| {
            "Commit graph missing OIDL chunk".to_string()
        })?;

        // ----- Hash length from file header -----
        let hash_len: usize = match data[5] {
            1 => 20, // SHA-1
            2 => 32, // SHA-256
            v => {
                return Err(format!(
                    "Unsupported commit-graph hash version {v} at {}",
                    path.display()
                ));
            }
        };

        Ok(Self {
            data,
            bidx_start: bidx_range.start,
            bdat_start: bdat_range.start,
            num_hashes,
            bits_per_entry,
            num_commits,
            path,
            oidf_offset: oidf_range.start,
            oidl_offset: oidl_range.start,
            hash_len,
        })
    }

    /// Query whether the commit at lexicographical `commit_pos` might have
    /// changed `path`.
    ///
    /// The Bloom filter has a ~1% false-positive rate. Returns `false` only
    /// when the path is **definitely NOT** changed in this commit.
    ///
    /// # Panics
    ///
    /// Panics if `commit_pos >= num_commits`.
    pub(crate) fn maybe_contains(&self, commit_pos: u32, path: &[u8]) -> bool {
        assert!(
            commit_pos < self.num_commits,
            "commit position {commit_pos} out of range (num_commits={})",
            self.num_commits
        );

        // Read the end offset from BIDX for this commit.
        let bidx_off = self.bidx_start + (commit_pos as usize) * 4;
        let end = u32::from_be_bytes(
            self.data[bidx_off..bidx_off + 4]
                .try_into()
                .expect("BIDX entry within file bounds"),
        ) as usize;

        // Start offset is previous BIDX entry (or 0 for first commit).
        let start = if commit_pos > 0 {
            let prev_off = bidx_off - 4;
            u32::from_be_bytes(
                self.data[prev_off..prev_off + 4]
                    .try_into()
                    .expect("BIDX entry within file bounds"),
            ) as usize
        } else {
            0
        };

        if start >= end {
            return false; // No filter data for this commit.
        }

        // Filter data begins after the 12-byte BDAT header, at offset `start`.
        let bdat_data_off = self.bdat_start + 12;
        let filter_len = end - start;
        let filter_data = &self.data[bdat_data_off + start..bdat_data_off + end];
        let mod_bits = filter_len * 8;
        if mod_bits == 0 {
            return false;
        }

        // MurmurHash3-based hashing (matches Git's fill_bloom_key).
        let hash0 = murmur3_32_seeded(path, 0x293ae76f);
        let hash1 = murmur3_32_seeded(path, 0x7e646e2c);

        for i in 0..self.num_hashes {
            let h = hash0.wrapping_add(i.wrapping_mul(hash1));
            let bit_pos = (h as usize) % mod_bits;
            let byte_idx = bit_pos / 8;
            let bit_idx = bit_pos % 8;

            if (filter_data[byte_idx] & (1 << bit_idx)) == 0 {
                return false;
            }
        }

        true
    }

    /// Look up the file-level (lexicographical) position of `oid` within
    /// this commit-graph file.
    ///
    /// Returns `Some(pos)` where `pos` is a `u32` suitable for passing
    /// to `maybe_contains`. Returns `None` when the OID is not found in
    /// this commit-graph file.
    #[allow(dead_code)]
    pub(crate) fn commit_position(&self, oid: &ObjectId) -> Option<u32> {
        let bytes = oid.as_bytes();
        let first_byte = bytes[0] as usize;
        let fan_base = self.oidf_offset;

        // Fan-out table gives the index range for this prefix byte.
        let lo = if first_byte == 0 {
            0u32
        } else {
            let start = fan_base + (first_byte - 1) * 4;
            u32::from_be_bytes(
                self.data[start..start + 4]
                    .try_into()
                    .expect("fan table entry"),
            )
        };
        let hi = {
            let start = fan_base + first_byte * 4;
            u32::from_be_bytes(
                self.data[start..start + 4]
                    .try_into()
                    .expect("fan table entry"),
            )
        };

        if lo >= hi {
            return None;
        }

        // Binary search within OIDL chunk range [lo, hi). The OIDL chunk
        // is a sorted array of hash_len-byte OIDs.
        let oidl_base = self.oidl_offset;
        let mut l = lo;
        let mut r = hi;
        while l < r {
            let mid = l + (r - l) / 2;
            let mid_off = oidl_base + (mid as usize) * self.hash_len;
            if mid_off + self.hash_len > self.data.len() {
                return None;
            }
            let mid_oid = &self.data[mid_off..mid_off + self.hash_len];
            match bytes.cmp(mid_oid) {
                std::cmp::Ordering::Less => r = mid,
                std::cmp::Ordering::Greater => l = mid + 1,
                std::cmp::Ordering::Equal => return Some(mid),
            }
        }
        None
    }
}

// ---------------------------------------------------------------------------
// MurmurHash3 32-bit — matches Git's `murmur3_seeded` in bloom.c
// ---------------------------------------------------------------------------

fn murmur3_32_seeded(data: &[u8], seed: u32) -> u32 {
    let c1: u32 = 0xcc9e2d51;
    let c2: u32 = 0x1b873593;
    let r1: u32 = 15;
    let r2: u32 = 13;
    let m: u32 = 5;
    let n: u32 = 0xe6546b64;

    let len = data.len();
    let nblocks = len / 4;
    let mut h = seed;

    for i in 0..nblocks {
        // Read as little-endian u32.
        let off = i * 4;
        let k1 = u32::from_le_bytes(data[off..off + 4].try_into().unwrap());

        let k1 = (k1.wrapping_mul(c1)).rotate_left(r1).wrapping_mul(c2);
        h ^= k1;
        h = h.rotate_left(r2).wrapping_mul(m).wrapping_add(n);
    }

    // Tail bytes (1-3 remaining after full 4-byte blocks).
    // NOTE: this uses intentional fallthrough to match Git's switch stmt.
    let tail_start = nblocks * 4;
    let remaining = len - tail_start;
    if remaining > 0 {
        let mut k1 = 0u32;
        // Fallthrough: remaining=3 also processes bytes 2 and 1; remaining=2 also processes byte 1.
        if remaining >= 3 {
            k1 ^= (data[tail_start + 2] as u32) << 16;
        }
        if remaining >= 2 {
            k1 ^= (data[tail_start + 1] as u32) << 8;
        }
        k1 ^= data[tail_start] as u32;
        let k1 = k1.wrapping_mul(c1).rotate_left(r1).wrapping_mul(c2);
        h ^= k1;
    }

    // Finalization mix.
    h ^= len as u32;
    h ^= h >> 16;
    h = h.wrapping_mul(0x85ebca6b);
    h ^= h >> 13;
    h = h.wrapping_mul(0xc2b2ae35);
    h ^= h >> 16;

    h
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn mmap_file(path: &std::path::Path) -> Result<memmap2::Mmap, String> {
    let file =
        std::fs::File::open(path).map_err(|e| format!("Cannot open commit-graph at {}: {e}", path.display()))?;
    // SAFETY: The file is opened read-only and we never mutate it. The mmap
    // is private to CommitGraphBloom and lives for the duration of the struct.
    unsafe {
        memmap2::MmapOptions::new()
            .map_copy_read_only(&file)
            .map_err(|e| format!("Cannot mmap commit-graph at {}: {e}", path.display()))
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    /// Create a temporary git repository with many files, generate a
    /// commit-graph with Bloom filters, and return the opened `Repository`
    /// plus the `TempDir` guard.
    fn repo_with_bloom_filter() -> (gix::Repository, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("create temp dir");
        let repo_path = dir.path();

        Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(repo_path)
            .status()
            .expect("git init");
        Command::new("git")
            .args(["config", "user.name", "test"])
            .current_dir(repo_path)
            .status()
            .expect("git config user.name");
        Command::new("git")
            .args(["config", "user.email", "test@test.com"])
            .current_dir(repo_path)
            .status()
            .expect("git config user.email");
        Command::new("git")
            .args(["config", "core.commitGraph", "true"])
            .current_dir(repo_path)
            .status()
            .expect("git config core.commitGraph");

        // Create 20 files to make the Bloom filter non-trivial.
        for i in 0u32..20 {
            let name = format!("file{i}.txt");
            std::fs::write(repo_path.join(&name), format!("content {i}")).expect("write file");
            Command::new("git")
                .args(["add", &name])
                .current_dir(repo_path)
                .status()
                .expect("git add");
        }
        Command::new("git")
            .args(["commit", "-m", "initial commit with many files"])
            .current_dir(repo_path)
            .status()
            .expect("git commit");

        let status = Command::new("git")
            .args(["commit-graph", "write", "--reachable", "--changed-paths"])
            .current_dir(repo_path)
            .status()
            .expect("git commit-graph write");
        assert!(status.success(), "git commit-graph write failed: {status:?}");

        let repo = gix::open(repo_path).expect("gix open");
        (repo, dir)
    }

    #[test]
    fn open_bloom_filter() {
        let (repo, _dir) = repo_with_bloom_filter();
        let bloom = CommitGraphBloom::open(&repo).expect("open bloom filter");
        assert!(bloom.num_commits >= 1, "expected at least 1 commit");
    }

    #[test]
    fn query_known_path() {
        let (repo, _dir) = repo_with_bloom_filter();
        let bloom = CommitGraphBloom::open(&repo).expect("open bloom filter");

        // The initial commit changed file0.txt through file19.txt, so the
        // Bloom filter for commit 0 MUST return true for any of those paths.
        assert!(
            bloom.maybe_contains(0, b"file0.txt"),
            "file0.txt was added in commit 0, must match"
        );
        assert!(
            bloom.maybe_contains(0, b"file19.txt"),
            "file19.txt was added in commit 0, must match"
        );
    }

    #[test]
    fn query_returns_bool_without_panicking() {
        let (repo, _dir) = repo_with_bloom_filter();
        let bloom = CommitGraphBloom::open(&repo).expect("open bloom filter");

        // Calling maybe_contains with a nonsense path must not panic.
        let _ = bloom.maybe_contains(0, b"xyznonexistent12345");
        // (no assertion beyond "didn't panic")
    }
}

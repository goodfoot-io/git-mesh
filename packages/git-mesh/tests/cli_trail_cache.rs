//! Integration tests for the cross-invocation rename-trail cache (Route C).
//!
//! Covers: warm-cache hits, invalidation on HEAD advance, config change,
//! seed change, fallback suppression, compact-time clear, and doctor gc.

mod support;

use anyhow::Result;
use std::path::PathBuf;
use support::TestRepo;

// ---------------------------------------------------------------------------
// Helpers.
// ---------------------------------------------------------------------------

fn seed(repo: &TestRepo, name: &str) -> Result<()> {
    repo.mesh_stdout(["add", name, "file1.txt#L1-L5"])?;
    repo.mesh_stdout(["why", name, "-m", "seed"])?;
    repo.mesh_stdout(["commit", name])?;
    Ok(())
}

/// Return the path to the trail-cache dir inside the repo's .git.
fn cache_dir(repo: &TestRepo) -> PathBuf {
    repo.path()
        .join(".git")
        .join("mesh")
        .join("cache")
        .join("rename-trail")
        .join("v1")
}

/// Return the cache file path for a given anchor sha.
fn cache_file(repo: &TestRepo, anchor_sha: &str) -> PathBuf {
    cache_dir(repo).join(format!("{anchor_sha}.json"))
}

/// Run `git mesh stale` (all meshes) with GIT_MESH_PERF=1, return stderr.
fn run_stale_all_with_perf(repo: &TestRepo) -> Result<String> {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_git-mesh"))
        .current_dir(repo.path())
        .env("GIT_MESH_PERF", "1")
        .args(["stale"])
        .output()?;
    Ok(String::from_utf8_lossy(&out.stderr).into_owned())
}

fn parse_counter(stderr: &str, label: &str) -> u64 {
    for line in stderr.lines() {
        if let Some(rest) = line.strip_prefix("git-mesh perf: ")
            && let Some(value_str) = rest.strip_prefix(&format!("{label} "))
            && let Ok(v) = value_str.trim().parse::<u64>()
        {
            return v;
        }
    }
    0
}

/// Get the anchor_sha for the first anchor in a mesh.
fn first_anchor_sha(repo: &TestRepo, mesh: &str) -> Result<String> {
    let gix = repo.gix_repo()?;
    let m = git_mesh::read_mesh(&gix, mesh)?;
    let (_, anchor) = m.anchors_v2.first().expect("one anchor");
    Ok(anchor.anchor_sha.clone())
}

// ---------------------------------------------------------------------------
// stale_warm_cache_hits: second run with no state drift hits cache fully.
//
// We advance HEAD with an unrelated commit so the anchor (Fresh, unchanged
// content) is not at HEAD — this bypasses the cheap-skip optimization and
// exercises the resolver + trail-cache path.
//
// Uses `git mesh stale` (all-meshes path) to get session.* perf counters.
// ---------------------------------------------------------------------------

#[test]
fn stale_warm_cache_hits() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed(&repo, "m")?;

    // Advance HEAD with an unrelated commit so anchor_sha != HEAD.
    // The cheap-skip only fires when anchor_sha == HEAD.
    repo.write_file("unrelated.txt", "x\n")?;
    repo.commit_all("advance HEAD — anchor becomes Fresh")?;

    // First run: populates the cache.
    run_stale_all_with_perf(&repo)?;

    // Second run (same HEAD, same config, same seed): should hit the cache.
    let stderr = run_stale_all_with_perf(&repo)?;

    let hits = parse_counter(&stderr, "session.trail-cache-hits");
    let walks = parse_counter(&stderr, "session.walks-len");
    let pass1_ms = parse_counter(&stderr, "session.pass1-ms");

    assert!(
        walks > 0,
        "expected at least one walk, got 0; stderr:\n{stderr}"
    );
    assert_eq!(
        hits, walks,
        "second run: trail-cache-hits ({hits}) must equal walks-len ({walks}); stderr:\n{stderr}"
    );
    assert_eq!(
        pass1_ms, 0,
        "second run: pass1-ms must be 0 on warm cache, got {pass1_ms}; stderr:\n{stderr}"
    );

    Ok(())
}

// ---------------------------------------------------------------------------
// stale_head_advance_invalidates: after a new commit, the cache misses.
// ---------------------------------------------------------------------------

#[test]
fn stale_head_advance_invalidates() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed(&repo, "m")?;

    // Advance HEAD once so anchor_sha != HEAD (resolver runs, cache fills).
    repo.write_file("unrelated.txt", "first\n")?;
    repo.commit_all("advance HEAD once")?;

    // Prime the cache.
    run_stale_all_with_perf(&repo)?;

    // Advance HEAD again — cache key changes (new head_sha).
    repo.write_file("unrelated.txt", "content\n")?;
    repo.commit_all("advance HEAD")?;

    // After HEAD moves, the cache key changes → miss.
    let stderr = run_stale_all_with_perf(&repo)?;
    let hits = parse_counter(&stderr, "session.trail-cache-hits");
    let misses = parse_counter(&stderr, "session.trail-cache-misses");

    assert_eq!(
        hits, 0,
        "expected 0 cache hits after HEAD advance, got {hits}; stderr:\n{stderr}"
    );
    assert!(
        misses > 0,
        "expected at least one miss after HEAD advance, got {misses}; stderr:\n{stderr}"
    );

    Ok(())
}

// ---------------------------------------------------------------------------
// stale_config_change_invalidates: flip diff.renameLimit, cache must miss.
// ---------------------------------------------------------------------------

#[test]
fn stale_config_change_invalidates() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed(&repo, "m")?;

    // Advance HEAD so anchor_sha != HEAD (resolver runs).
    repo.write_file("unrelated.txt", "x\n")?;
    repo.commit_all("advance HEAD")?;

    // Prime the cache.
    run_stale_all_with_perf(&repo)?;

    // Change a config value that is part of the cache key.
    repo.run_git(["config", "diff.renameLimit", "50"])?;

    let stderr = run_stale_all_with_perf(&repo)?;
    let hits = parse_counter(&stderr, "session.trail-cache-hits");

    assert_eq!(
        hits, 0,
        "expected 0 cache hits after diff.renameLimit change, got {hits}; stderr:\n{stderr}"
    );

    Ok(())
}

// ---------------------------------------------------------------------------
// stale_seed_change_invalidates: different candidate seed → miss.
//
// After priming the cache for m1 (file1.txt seed), delete m1 and create m2
// with the same anchor commit but a different file (file2.txt seed). The
// cache file for that anchor_sha now contains m1's seed hash; m2's first
// run must miss because the seed hash in the file doesn't match m2's seed.
// ---------------------------------------------------------------------------

#[test]
fn stale_seed_change_invalidates() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let initial_sha = repo.head_sha()?;

    // Seed m1 anchored to initial_sha.
    repo.mesh_stdout(["add", "m1", "file1.txt#L1-L5", "--at", &initial_sha])?;
    repo.mesh_stdout(["why", "m1", "-m", "seed m1"])?;
    repo.mesh_stdout(["commit", "m1"])?;

    let anchor_sha = first_anchor_sha(&repo, "m1")?;
    assert_eq!(anchor_sha, initial_sha, "anchor at initial sha");

    // Advance HEAD so m1 becomes Fresh and the resolver runs (populates cache).
    repo.write_file("extra.txt", "x\n")?;
    repo.commit_all("advance HEAD")?;

    // Prime cache for m1.
    run_stale_all_with_perf(&repo)?;

    let cf = cache_file(&repo, &anchor_sha);
    assert!(cf.exists(), "cache must exist after m1 stale run");

    // Delete m1 and create m2 with the same anchor commit but different file.
    // m2's anchor_sha == initial_sha, so it shares the cache file path.
    repo.mesh_stdout(["delete", "m1"])?;
    repo.mesh_stdout(["add", "m2", "file2.txt#L1-L5", "--at", &initial_sha])?;
    repo.mesh_stdout(["why", "m2", "-m", "seed m2"])?;
    repo.mesh_stdout(["commit", "m2"])?;

    // Run stale for m2. The cache file has m1's seed hash; m2's seed hash
    // is different → miss.
    let stderr = run_stale_all_with_perf(&repo)?;
    let hits = parse_counter(&stderr, "session.trail-cache-hits");

    assert_eq!(
        hits, 0,
        "m2 must miss the cache (different seed from m1); stderr:\n{stderr}"
    );

    Ok(())
}

// ---------------------------------------------------------------------------
// stale_fallback_does_not_populate: AnyFileInRepo copy detection triggers the
// fell_back path → no cache file written, second run still misses.
// ---------------------------------------------------------------------------

#[test]
fn stale_fallback_does_not_populate() -> Result<()> {
    let repo = TestRepo::seeded()?;

    // Use copy-detection=any-file-in-repo, which always falls back in
    // compute_rename_trail (the only code path that sets fell_back=true).
    // Step 1: create the mesh first (config requires the mesh to exist).
    repo.mesh_stdout(["add", "m", "file1.txt#L1-L5"])?;
    repo.mesh_stdout(["why", "m", "-m", "seed"])?;
    repo.mesh_stdout(["commit", "m"])?;
    // Step 2: stage the copy-detection config and commit again.
    repo.mesh_stdout(["config", "m", "copy-detection", "any-file-in-repo"])?;
    repo.mesh_stdout(["commit", "m"])?;

    // Advance HEAD so anchor_sha != HEAD (resolver runs).
    repo.write_file("unrelated.txt", "x\n")?;
    repo.commit_all("advance HEAD")?;

    let anchor_sha = first_anchor_sha(&repo, "m")?;
    let cf = cache_file(&repo, &anchor_sha);

    // First run: AnyFileInRepo → fell_back=true → no cache file.
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_git-mesh"))
        .current_dir(repo.path())
        .env("GIT_MESH_PERF", "1")
        .args(["stale"])
        .output()?;
    let stderr1 = String::from_utf8_lossy(&out.stderr).into_owned();

    assert!(
        !cf.exists(),
        "cache file must not be written on AnyFileInRepo fallback; file at {cf:?}; stderr:\n{stderr1}"
    );

    // Second run: still misses because nothing was cached.
    let out2 = std::process::Command::new(env!("CARGO_BIN_EXE_git-mesh"))
        .current_dir(repo.path())
        .env("GIT_MESH_PERF", "1")
        .args(["stale"])
        .output()?;
    let stderr2 = String::from_utf8_lossy(&out2.stderr).into_owned();
    let hits2 = parse_counter(&stderr2, "session.trail-cache-hits");

    assert_eq!(
        hits2, 0,
        "second run with AnyFileInRepo must still miss (nothing cached); stderr:\n{stderr2}"
    );

    Ok(())
}

// ---------------------------------------------------------------------------
// compact_clears_old_anchor_cache: after compact advances, old cache gone.
//
// Setup: advance HEAD so anchor is Fresh (anchor_sha != HEAD). This causes
// the resolver to run and populate the cache on the first stale call.
// Then advance HEAD again and compact — the old cache file must be cleared.
// ---------------------------------------------------------------------------

#[test]
fn compact_clears_old_anchor_cache() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed(&repo, "m")?;

    // First HEAD advance: anchor becomes Fresh (anchor_sha != HEAD but content
    // unchanged). The resolver runs and populates the trail cache.
    repo.write_file("unrelated.txt", "first-advance\n")?;
    repo.commit_all("advance HEAD (prime cache)")?;

    // Prime the cache.
    run_stale_all_with_perf(&repo)?;

    let old_anchor_sha = first_anchor_sha(&repo, "m")?;
    let cf = cache_file(&repo, &old_anchor_sha);

    // Verify cache was populated.
    assert!(
        cf.exists(),
        "expected cache file to exist before compact at {cf:?}"
    );

    // Second HEAD advance: still Fresh — compact will advance the anchor to the
    // new HEAD and the old anchor_sha cache file must be cleared.
    repo.write_file("unrelated.txt", "second-advance\n")?;
    repo.commit_all("advance HEAD (compact target)")?;

    // Run compact — advances the anchor and must clear the old cache file.
    repo.mesh_stdout(["stale", "m", "--compact"])?;

    assert!(
        !cf.exists(),
        "expected cache file to be deleted after compact at {cf:?}"
    );

    Ok(())
}

// ---------------------------------------------------------------------------
// doctor_gc_trail_cache_sweeps_orphans: orphan files removed, live preserved.
// ---------------------------------------------------------------------------

#[test]
fn doctor_gc_trail_cache_sweeps_orphans() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed(&repo, "m")?;

    // Advance HEAD so anchor becomes Fresh and resolver runs (populates cache).
    repo.write_file("unrelated.txt", "x\n")?;
    repo.commit_all("advance HEAD")?;

    // Prime the cache.
    run_stale_all_with_perf(&repo)?;

    let anchor_sha = first_anchor_sha(&repo, "m")?;
    let live_file = cache_file(&repo, &anchor_sha);

    assert!(live_file.exists(), "expected live cache file to exist");

    // Write an orphan cache file (no corresponding live anchor).
    let orphan_sha = "deadbeef".repeat(5); // 40-char string
    let orphan_file = cache_dir(&repo).join(format!("{orphan_sha}.json"));
    std::fs::write(&orphan_file, "garbage\n")?;

    assert!(orphan_file.exists(), "orphan file must exist before doctor");

    // Run doctor --gc-trail-cache.
    let out = repo.run_mesh(["doctor", "--gc-trail-cache"])?;
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();

    assert!(
        out.status.success(),
        "doctor --gc-trail-cache must succeed; stdout:\n{stdout}"
    );
    assert!(
        stdout.contains("trail cache gc"),
        "expected gc summary line; stdout:\n{stdout}"
    );

    // Orphan removed, live preserved.
    assert!(
        !orphan_file.exists(),
        "orphan cache file must be removed after doctor gc"
    );
    assert!(
        live_file.exists(),
        "live anchor cache file must be preserved after doctor gc"
    );

    Ok(())
}

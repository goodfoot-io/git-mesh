//! `ResolveSession` — engine-wide shared computation for one `stale` run.
//!
//! Groups anchors by `(repo, anchor_sha)` and walks `anchor..HEAD` exactly
//! once per group. The per-commit name-status entries (with rewrite
//! tracking enabled) are produced once per commit and shared across:
//!
//! - the per-anchor line-range HEAD walker (`resolve_at_head_shared`),
//! - the whole-file rename trail (`follow_path_to_head_shared`).
//!
//! The session is constructed once at the top of the `stale` CLI path and
//! threaded through `resolve_anchor_inner`. There is no caching across
//! runs — the session lives only for the duration of one engine call and
//! is dropped when it returns.
//!
//! ## Candidate-path filtering (Pass 1 + Pass 2)
//!
//! The expensive per-commit work is `name_status` with rewrite tracking
//! enabled. Most commits in `anchor..HEAD` don't touch any path the
//! mesh's anchors care about, so we skip the rewrite-aware tree-diff for
//! those commits. Skipped commits still appear in `commits` with
//! `entries: vec![]`, which `walker::advance_with_entries` treats as a
//! no-op (no path matches → `Change::Unchanged`). The `parent` slot
//! still threads to the previous commit in the walk so that, if a later
//! commit *is* interesting, the diff baseline is correct.
//!
//! Candidate paths are seeded by the caller via `prepare_group` with the
//! union of all anchor paths in the mesh. As the rewrite-aware pass on
//! interesting commits discovers `Renamed{from, to}` / `Copied{from,
//! to}` entries where `from` is already tracked, `to` is added to the
//! candidate set so subsequent commits that touch the new name are
//! correctly classified as interesting. Copy detection (anything other
//! than `CopyDetection::Off`) widens the trigger: any commit with at
//! least one added path is treated as interesting because a same-commit
//! copy can introduce a new tracked path with no parent-side change.
//!
//! "Sharing a single computation across consumers, not storing past
//! results."

use crate::Result;
use crate::git;
use crate::resolver::cache::{Cache, GroupedWalkKey, Kind, Outcome, RenameTrailKey};
use crate::resolver::walker::{self, NS};
use crate::types::{Anchor, CopyDetection};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};

/// One per-commit slice of the shared walk: `(parent_sha, commit_sha,
/// name_status_entries)`. Entries are produced with rewrite tracking
/// enabled; consumers that want the cheap "no-rewrites" view derive it by
/// projecting `Rename`/`Copied` back to `Added` (the `to`) plus
/// `Deleted` (the `from`). Per phase 3.
///
/// For commits that the candidate-path filter classifies as
/// non-interesting, `entries` is empty — `advance_with_entries` then
/// short-circuits to `Change::Unchanged` and `follow_path_to_head_shared`
/// finds no rename rows. `parent` still references the prior commit in
/// the walk so an interesting commit later in the walk diffs against the
/// correct baseline.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct CommitDelta {
    pub(crate) parent: String,
    pub(crate) commit: String,
    pub(crate) entries: Vec<NS>,
}

/// One grouped walk: the rev list (oldest-first) from `anchor_sha..HEAD`,
/// plus per-commit deltas. Computed exactly once per `(repo,
/// anchor_sha)`.
#[derive(serde::Serialize, serde::Deserialize)]
pub(crate) struct GroupedWalk {
    pub(crate) anchor_sha: String,
    pub(crate) head_sha: String,
    pub(crate) commits: Vec<CommitDelta>,
    /// Did any per-commit `name_status` call hit the rename-detection
    /// budget and emit a no-renames warning? If so, downstream consumers
    /// must accept that some `NS::Added`/`NS::Deleted` entries should
    /// have been paired as a rename but weren't.
    #[allow(dead_code)]
    pub(crate) renames_disabled: bool,
    /// Pass 1's closed rename-trail set: every historical name any
    /// candidate path has had in the in-range history. Cached on the
    /// walk so a future cross-invocation cache (Route C) can persist it
    /// without restructuring.
    ///
    /// `None` semantics:
    /// - No candidate seed was provided (single-anchor / test fallback
    ///   path bypassed Pass 1 entirely).
    /// - Pass 1 ran but fell back (`AnyFileInRepo`, rename-budget
    ///   exhaustion). The seed alone does not satisfy the
    ///   "every historical name" contract, so the field is dropped
    ///   rather than handed downstream as if it were a real closure.
    #[allow(dead_code)]
    pub(crate) closed_paths: Option<HashSet<String>>,
}

/// Engine-wide shared state: one entry per distinct anchor commit.
pub(crate) struct ResolveSession {
    walks: HashMap<(String, CopyDetection), GroupedWalk>,
    pub(crate) ensure_calls: u64,
    pub(crate) ensure_hits: u64,
    /// Counter: how many commits across all walks were skipped by the
    /// candidate-path filter (i.e. classified non-interesting).
    pub(crate) skipped_commits: u64,
    /// Counter: how many commits across all walks ran the full
    /// rewrite-aware `name_status`.
    pub(crate) interesting_commits: u64,
    /// Counter: total wall-clock milliseconds spent in Pass 1 (rename-trail
    /// closure via `git log --name-status` subprocesses) across all walks.
    pub(crate) pass1_ms: u64,
    /// Counter: rename-trail cache hits (per-`Kind` decomposition of
    /// `cache.l1-hits + cache.l2-hits`).
    pub(crate) rename_trail_hits: u64,
    /// Counter: rename-trail cache misses.
    pub(crate) rename_trail_misses: u64,
    /// Counter: grouped-walk cache hits.
    pub(crate) grouped_walk_hits: u64,
    /// Counter: grouped-walk cache misses.
    pub(crate) grouped_walk_misses: u64,
    /// Counter: drift-locus cache hits.
    pub(crate) drift_locus_hits: u64,
    /// Counter: drift-locus cache misses.
    pub(crate) drift_locus_misses: u64,
    /// Counter: per-path filter-attribute memo hits. Populated by
    /// `EngineState::filter_short_circuit` on cached `(rel_path)` reads.
    pub(crate) filter_attr_hits: u64,
    /// Counter: per-path filter-attribute memo misses (first lookup per
    /// distinct path). On a warm `stale` run, this equals the number of
    /// distinct paths probed across all anchors in the session.
    pub(crate) filter_attr_misses: u64,
    /// Content-addressed FS cache (BLAKE3-keyed) shared across all anchors
    /// in this resolver run.
    pub(crate) cache: Cache,
    /// Per-session set of commit ObjectIds known to be ancestors of HEAD.
    /// Populated by (a) successful `is_ancestor` checks in `drift_locus` wiring
    /// and (b) every commit observed during a miss-path `rev_walk` (those are
    /// ancestors of HEAD by construction since the walk runs `HEAD..anchor`).
    pub(crate) known_head_ancestors:
        std::collections::HashMap<gix::ObjectId, std::collections::HashSet<gix::ObjectId>>,
    /// Session-wide memoized hashes: `(replace_refs_hash, git_config_hash)`.
    /// Both are constant for the duration of one `git mesh stale` run — they
    /// depend on git refs and config, not on any anchor or HEAD. Computed once
    /// in `new()` and reused by every `build_grouped_walk` call, eliminating
    /// 8 subprocess forks per unique anchor SHA.
    pub(crate) session_hashes: Option<([u8; 32], [u8; 32])>,
    /// Session-scoped blob OID memo: `(commit_sha, path) → blob_oid`.
    ///
    /// `path_blob_at` requires a tree traversal for every `(commit, path)`
    /// pair. Many anchors in a mesh share the same underlying files and
    /// commit history, so the same pair is requested repeatedly across
    /// anchors. This memo eliminates the redundant traversals within a
    /// single `stale` run; it does not persist across invocations.
    blob_oid_memo: HashMap<(String, String), Option<String>>,
    /// Counters: anchors classified by terminal status. Updated from the
    /// per-anchor loop in `engine::resolve_loaded_mesh_with_state` after each
    /// `resolve_anchor_inner` call.
    pub(crate) anchors_fresh: u64,
    pub(crate) anchors_moved: u64,
    pub(crate) anchors_changed: u64,
    pub(crate) anchors_orphaned: u64,
    pub(crate) anchors_merge_conflict: u64,
    pub(crate) anchors_unavailable: u64,
    /// Counter: anchors that returned via [`clean_head_fast_path`] (early
    /// return). The remainder went through the full layer-comparison path
    /// (`anchors_total - anchors_fast_path_hits == anchors_full_resolution`).
    pub(crate) anchors_fast_path_hits: u64,
    /// Counter: anchors that went through the full per-layer resolution
    /// (`resolve_anchor_inner` past the fast-path).
    pub(crate) anchors_full_resolution: u64,
    /// Per-anchor wall-clock (microseconds), one entry per `resolve_anchor_inner`
    /// invocation. Sorted at end-of-run to compute `p50` / `p95` percentiles.
    /// Dropped immediately after emit; ~8 bytes per anchor.
    pub(crate) per_anchor_us: Vec<u128>,
    /// When `Some`, accumulates one `TraceRow` per anchor for `--perf-trace` CSV
    /// output. Remains `None` unless `enable_trace()` is called before resolution.
    pub(crate) per_anchor_trace: Option<Vec<crate::perf::TraceRow>>,
}

impl ResolveSession {
    pub(crate) fn new(repo: &gix::Repository) -> Self {
        let cache = Cache::open(repo).unwrap_or_else(|_| {
            // Cache failures degrade silently to a disabled cache.
            Cache::open_disabled()
        });
        // Compute session-wide hashes once. Failure degrades to None; each
        // build_grouped_walk call then falls back to per-call compute_key.
        let session_hashes = compute_session_hashes(repo);
        Self {
            walks: HashMap::new(),
            ensure_calls: 0,
            ensure_hits: 0,
            skipped_commits: 0,
            interesting_commits: 0,
            pass1_ms: 0,
            rename_trail_hits: 0,
            rename_trail_misses: 0,
            grouped_walk_hits: 0,
            grouped_walk_misses: 0,
            drift_locus_hits: 0,
            drift_locus_misses: 0,
            filter_attr_hits: 0,
            filter_attr_misses: 0,
            cache,
            session_hashes,
            blob_oid_memo: HashMap::new(),
            known_head_ancestors: std::collections::HashMap::new(),
            anchors_fresh: 0,
            anchors_moved: 0,
            anchors_changed: 0,
            anchors_orphaned: 0,
            anchors_merge_conflict: 0,
            anchors_unavailable: 0,
            anchors_fast_path_hits: 0,
            anchors_full_resolution: 0,
            per_anchor_us: Vec::new(),
            per_anchor_trace: None,
        }
    }

    pub(crate) fn enable_trace(&mut self) {
        self.per_anchor_trace = Some(Vec::new());
    }

    pub(crate) fn anchors_total(&self) -> u64 {
        self.anchors_fresh
            + self.anchors_moved
            + self.anchors_changed
            + self.anchors_orphaned
            + self.anchors_merge_conflict
            + self.anchors_unavailable
    }

    pub(crate) fn walks_len(&self) -> usize {
        self.walks.len()
    }

    /// Pre-build the grouped walk for `anchor_sha` with the caller-supplied
    /// candidate-path set. Idempotent — the first prepare_group call for
    /// a given `(anchor_sha, copy_detection)` wins. Subsequent
    /// `ensure_group` lookups return the cached walk.
    pub(crate) fn prepare_group(
        &mut self,
        repo: &gix::Repository,
        anchor_sha: &str,
        copy_detection: CopyDetection,
        candidate_paths: &HashSet<String>,
        warnings: &mut Vec<String>,
    ) -> Result<()> {
        let key = (anchor_sha.to_string(), copy_detection);
        if self.walks.contains_key(&key) {
            return Ok(());
        }
        let walk = build_grouped_walk(
            repo,
            anchor_sha,
            copy_detection,
            Some(candidate_paths),
            warnings,
            &mut self.skipped_commits,
            &mut self.interesting_commits,
            &mut self.pass1_ms,
            &mut self.rename_trail_hits,
            &mut self.rename_trail_misses,
            &mut self.grouped_walk_hits,
            &mut self.grouped_walk_misses,
            &self.cache,
            self.session_hashes,
        )?;
        self.walks.insert(key, walk);
        Ok(())
    }

    /// Ensure a grouped walk exists for `anchor_sha`. Idempotent. The
    /// `copy_detection` is used the first time a group is built; meshes
    /// share the same copy-detection knob across their anchors so this
    /// is unambiguous within one mesh, and walks are keyed by anchor
    /// commit so different meshes that share a anchor still get their
    /// own group only on first observation. (Greenfield: we don't try
    /// to merge mismatched copy-detection levels — the first wins
    /// because a single mesh-wide level is the authoritative source.)
    ///
    /// Callers that know the mesh's full anchor-path set should call
    /// `prepare_group` before this so the walk skips commits that don't
    /// touch any candidate path. When `prepare_group` was not called,
    /// this falls back to the unfiltered (every commit gets a full
    /// rewrite-aware `name_status`) path so single-anchor / test
    /// callers stay correct.
    pub(crate) fn ensure_group(
        &mut self,
        repo: &gix::Repository,
        anchor_sha: &str,
        copy_detection: CopyDetection,
        warnings: &mut Vec<String>,
    ) -> Result<&GroupedWalk> {
        let key = (anchor_sha.to_string(), copy_detection);
        self.ensure_calls += 1;
        if !self.walks.contains_key(&key) {
            let walk = build_grouped_walk(
                repo,
                anchor_sha,
                copy_detection,
                None,
                warnings,
                &mut self.skipped_commits,
                &mut self.interesting_commits,
                &mut self.pass1_ms,
                &mut self.rename_trail_hits,
                &mut self.rename_trail_misses,
                &mut self.grouped_walk_hits,
                &mut self.grouped_walk_misses,
                &self.cache,
                self.session_hashes,
            )?;
            self.walks.insert(key.clone(), walk);
        } else {
            self.ensure_hits += 1;
        }
        Ok(self.walks.get(&key).expect("just inserted"))
    }

    #[allow(dead_code)]
    pub(crate) fn group(&self, anchor_sha: &str) -> Option<&GroupedWalk> {
        self.walks
            .iter()
            .find_map(|((sha, _), walk)| (sha == anchor_sha).then_some(walk))
    }
}

#[allow(clippy::too_many_arguments)]
fn build_grouped_walk(
    repo: &gix::Repository,
    anchor_sha: &str,
    copy_detection: CopyDetection,
    candidate_paths: Option<&HashSet<String>>,
    warnings: &mut Vec<String>,
    skipped_counter: &mut u64,
    interesting_counter: &mut u64,
    pass1_ms_counter: &mut u64,
    rename_trail_hits_counter: &mut u64,
    rename_trail_misses_counter: &mut u64,
    grouped_walk_hits_counter: &mut u64,
    grouped_walk_misses_counter: &mut u64,
    cache: &Cache,
    // Pre-computed session-wide hashes: `(replace_refs_hash, git_config_hash)`.
    // Reused for every cache key in this function when `Some`; when `None`,
    // falls back to per-call computation.
    session_hashes: Option<([u8; 32], [u8; 32])>,
) -> Result<GroupedWalk> {
    // Phase 1: the entire walk routes through a single cache probe per
    // anchor. The compute closure runs Pass 1 (rename-trail closure, via its
    // own nested cache probe) followed by Pass 2 (per-commit `name_status`
    // collection) inline. Phase 1's stub `Cache::get_or_insert_with` calls
    // `compute()` unconditionally via `todo!()` — the real wiring lands in
    // Phase 3.
    let head_sha = git::head_oid(repo)?;
    let (replace_refs_hash, git_config_hash) = match session_hashes {
        Some(pair) => pair,
        None => compute_session_hashes(repo).unwrap_or(([0u8; 32], [0u8; 32])),
    };
    let empty_seed: HashSet<String> = HashSet::new();
    let seed_for_key = candidate_paths.unwrap_or(&empty_seed);
    let candidate_seed_hash = hash_sorted_paths(seed_for_key);
    let rename_budget = walker::rename_budget() as i64;

    let gw_key = GroupedWalkKey {
        anchor_sha: anchor_sha.to_string(),
        copy_detection,
        seed_hash: candidate_seed_hash,
        replace_refs_hash,
        git_config_hash,
        rename_budget,
        head_sha: head_sha.clone(),
    };

    let result = cache.get_or_insert_with_outcome(Kind::GroupedWalk, &gw_key, || {
        compute_grouped_walk(
            repo,
            anchor_sha,
            copy_detection,
            candidate_paths,
            &head_sha,
            replace_refs_hash,
            git_config_hash,
            candidate_seed_hash,
            rename_budget,
            warnings,
            skipped_counter,
            interesting_counter,
            pass1_ms_counter,
            rename_trail_hits_counter,
            rename_trail_misses_counter,
            cache,
        )
    });
    match result {
        Ok((walk, outcome)) => {
            match outcome {
                Outcome::L1Hit | Outcome::L2Hit => *grouped_walk_hits_counter += 1,
                Outcome::Miss => *grouped_walk_misses_counter += 1,
            }
            Ok(walk)
        }
        Err(e) => {
            *grouped_walk_misses_counter += 1;
            Err(e)
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn compute_grouped_walk(
    repo: &gix::Repository,
    anchor_sha: &str,
    copy_detection: CopyDetection,
    candidate_paths: Option<&HashSet<String>>,
    head_sha: &str,
    replace_refs_hash: [u8; 32],
    git_config_hash: [u8; 32],
    candidate_seed_hash: [u8; 32],
    rename_budget: i64,
    warnings: &mut Vec<String>,
    skipped_counter: &mut u64,
    interesting_counter: &mut u64,
    pass1_ms_counter: &mut u64,
    rename_trail_hits_counter: &mut u64,
    rename_trail_misses_counter: &mut u64,
    cache: &Cache,
) -> Result<GroupedWalk> {
    let _ = (replace_refs_hash, git_config_hash, candidate_seed_hash, rename_budget);

    // Walk anchor..HEAD oldest-first.
    let mut commits =
        git::rev_walk_excluding(repo, &[head_sha], &[anchor_sha], None).unwrap_or_default();
    commits.reverse();

    // Walk-order parent map for Pass 1's rename-detection baseline alignment.
    let mut walk_parents: HashMap<String, String> = HashMap::with_capacity(commits.len());
    {
        let mut prev = anchor_sha.to_string();
        for commit in &commits {
            walk_parents.insert(commit.clone(), prev.clone());
            prev = commit.clone();
        }
    }

    // Pass 1: rename-trail closure via the cache. The compute closure runs
    // `compute_rename_trail` on miss; cache hits skip Pass 1 entirely.
    let (closed_paths, interesting_set, pass1_fell_back) = match candidate_paths {
        Some(seed) => {
            let trail_key = RenameTrailKey {
                anchor_sha: anchor_sha.to_string(),
                head_sha: head_sha.to_string(),
                copy_detection,
                rename_budget,
                candidate_seed_hash,
                replace_refs_hash,
                git_config_hash,
            };
            // The cached payload carries `fell_back` so warm runs reconstruct
            // Pass 1's classification authority: a fell-back trail
            // (`AnyFileInRepo` short-circuit) cached `(seed, ∅, true)` must,
            // on hit, force every commit interesting just as the cold run did.
            let local_pass1_ms = std::cell::Cell::new(0u64);
            type RenameTrailPayload = (HashSet<String>, HashSet<String>, bool);
            type RenameTrailProbe = (RenameTrailPayload, Outcome);
            let probe: Result<RenameTrailProbe> = cache
                .get_or_insert_with_outcome(Kind::RenameTrail, &trail_key, || {
                    let t0 = std::time::Instant::now();
                    let (closed, interesting, fell_back) = compute_rename_trail(
                        repo, anchor_sha, seed, copy_detection, &walk_parents, warnings,
                    )?;
                    local_pass1_ms.set(t0.elapsed().as_millis() as u64);
                    Ok((closed, interesting, fell_back))
                });
            *pass1_ms_counter += local_pass1_ms.get();
            match probe {
                Ok(((closed, interesting, fell_back), outcome)) => {
                    match outcome {
                        Outcome::Miss => *rename_trail_misses_counter += 1,
                        Outcome::L1Hit | Outcome::L2Hit => *rename_trail_hits_counter += 1,
                    }
                    let cached_closed = if fell_back { None } else { Some(closed) };
                    (cached_closed, Some(interesting), fell_back)
                }
                Err(e) => {
                    eprintln!("rename_trail cache error (treating as miss): {e}");
                    *rename_trail_misses_counter += 1;
                    let t0 = std::time::Instant::now();
                    let (closed, interesting, fell_back) = compute_rename_trail(
                        repo, anchor_sha, seed, copy_detection, &walk_parents, warnings,
                    )?;
                    *pass1_ms_counter += t0.elapsed().as_millis() as u64;
                    let cached_closed = if fell_back { None } else { Some(closed) };
                    (cached_closed, Some(interesting), fell_back)
                }
            }
        }
        None => (None, None, false),
    };

    let mut deltas: Vec<CommitDelta> = Vec::with_capacity(commits.len());
    let mut parent = anchor_sha.to_string();
    let prior_warning_count = warnings.len();

    for commit in &commits {
        let interesting = match interesting_set.as_ref() {
            None => true,
            Some(_) if pass1_fell_back => true,
            Some(set) => set.contains(commit),
        };
        let entries = if interesting {
            *interesting_counter += 1;
            walker::name_status(repo, &parent, commit, copy_detection, warnings)?
        } else {
            *skipped_counter += 1;
            Vec::new()
        };
        deltas.push(CommitDelta {
            parent: parent.clone(),
            commit: commit.clone(),
            entries,
        });
        parent = commit.clone();
    }
    let renames_disabled = pass1_fell_back || warnings.len() > prior_warning_count;

    let walk = GroupedWalk {
        anchor_sha: anchor_sha.to_string(),
        head_sha: head_sha.to_string(),
        commits: deltas,
        renames_disabled,
        closed_paths,
    };
    Ok(walk)
}

/// Pass 1: discover every historical name any seed path has had in
/// `anchor_sha..HEAD` and the set of commits whose `--name-status` rows
/// touch any of those names.
///
/// Implementation note on pathspec: git applies `-- <pathspec>` *before*
/// rename detection, so `git log -M -- foo.rs` clips the `R foo.rs
/// bar.rs` row down to `D foo.rs` (the destination side falls outside
/// the pathspec). Running `-M` over the full range (no pathspec) is
/// correct but pays the rename-detection cost on every commit — too
/// expensive on long histories.
///
/// The hybrid used here keeps both halves cheap:
///
/// 1. **Cheap pass (pathspec, no rename detection) via `git log`.**
///    `git log --no-renames -- closed_paths` enumerates every commit
///    that touches any closed path. git's pathspec engine prunes whole
///    subtrees and skips rename detection entirely.
/// 2. **Per-commit rename detection in-process via gix.** For each
///    touched commit, [`walker::name_status`] is called against the
///    commit's *walk-order* parent — the same baseline Pass 2 uses, so
///    the rename rows Pass 1 surfaces are exactly the rows Pass 2's
///    [`walker::advance_with_entries`] will encounter. (On merge
///    commits the walk-order predecessor is whichever parent the gix
///    rev-walk emitted next; that may differ from `commit^1`, but it's
///    the only baseline whose entries Pass 2 actually consumes.) New
///    rename/copy pairs that intersect the trail grow `closed_paths`;
///    the loop iterates to fixed point.
/// 3. **Phase B for `AnyFileInCommit`.** Copies whose source was *not*
///    modified in the commit can't be detected from the cheap pass at
///    all; a single full-range `-C -C` `git log` scan completes the
///    trail. Skipped for `Off` and `SameCommit` (where the source is
///    always among the pass-1 touched commits).
///
/// `AnyFileInRepo` widens copy sources to any blob reachable from any
/// ref — outside the per-range scope a single `git log` invocation can
/// express. Falling back to "every commit interesting" preserves the
/// walker's whole-ref widening (`widen_copies_any_ref`) in Pass 2.
///
/// Returns `(closed_paths, interesting_commits, fell_back)`. On
/// `fell_back == true`, `interesting_commits` is empty and the caller
/// must treat every commit as interesting.
fn compute_rename_trail(
    repo: &gix::Repository,
    anchor_sha: &str,
    seed: &HashSet<String>,
    copy_detection: CopyDetection,
    walk_parents: &HashMap<String, String>,
    warnings: &mut Vec<String>,
) -> Result<(HashSet<String>, HashSet<String>, bool)> {
    let range = format!("{anchor_sha}..HEAD");
    let budget = walker::rename_budget();

    if matches!(copy_detection, CopyDetection::AnyFileInRepo) {
        return Ok((seed.clone(), HashSet::new(), true));
    }

    let mut closed: HashSet<String> = seed.clone();
    let mut interesting: HashSet<String> = HashSet::new();
    // Per-commit cache of `(from, to)` rename + copy pairs from gix
    // `name_status`, populated once per commit on first inspection.
    // Pairs are kept in a flat list so the fixed-point loop can revisit
    // them every iteration — both across commits AND within a single
    // commit's entry list (intra-commit rename chains like
    // `R foo→a; R a→b` only converge after both pairs are re-scanned in
    // the iteration where `closed` grew to include the prerequisite).
    let mut inspected: HashSet<String> = HashSet::new();
    let mut all_pairs: Vec<(String, String)> = Vec::new();

    loop {
        let paths: Vec<String> = closed.iter().cloned().collect();
        let (rows, _) = git::git_log_name_status(
            repo,
            &range,
            git::RenameDetect::None,
            budget,
            &paths,
        )?;
        // (--no-renames means "renames disabled" is meaningless here; the
        // budget warning fires only with -M / -C.)

        for row in rows {
            interesting.insert(row.commit.clone());
            if !inspected.insert(row.commit.clone()) {
                continue;
            }
            // Pathspec clipping can hide a rename/copy pair when only
            // one side matches `closed`. Per-commit rename detection
            // runs in-process via gix (no subprocess overhead) against
            // the commit's walk-order parent — the same baseline Pass 2
            // uses, so the rename rows Pass 1 surfaces are exactly the
            // ones Pass 2's `walker::advance_with_entries` will see.
            // The pairs join the flat `all_pairs` list so the
            // fixed-point loop below can grow `closed` regardless of
            // intra- or inter-commit chain ordering.
            let Some(parent) = walk_parents.get(&row.commit) else {
                // Touched commit not in the gix walk (set-agreement
                // gap between `git log` and `gix::rev_walk`); skip.
                continue;
            };
            let entries =
                walker::name_status(repo, parent, &row.commit, copy_detection, warnings)?;
            for e in &entries {
                if let NS::Renamed { from, to } | NS::Copied { from, to } = e {
                    all_pairs.push((from.clone(), to.clone()));
                }
            }
        }

        // Fixed-point closure across every pair seen so far.
        let mut grew = false;
        loop {
            let mut grew_inner = false;
            for (from, to) in &all_pairs {
                if closed.contains(from) || closed.contains(to) {
                    if closed.insert(from.clone()) {
                        grew_inner = true;
                    }
                    if closed.insert(to.clone()) {
                        grew_inner = true;
                    }
                }
            }
            if !grew_inner {
                break;
            }
            grew = true;
        }
        if !grew {
            break;
        }
    }

    // Phase B — only for AnyFileInCommit. SameCommit copies always have a
    // modified source side, which the cheap pass already captures; Off
    // skips copy detection entirely.
    if matches!(copy_detection, CopyDetection::AnyFileInCommit) {
        let (rows, disabled) = git::git_log_name_status(
            repo,
            &range,
            git::RenameDetect::CopiesHarder,
            budget,
            &[],
        )?;
        if disabled {
            warnings.push(format!(
                "warning: copy detection disabled for trail closure {}..HEAD; falling back to all-commits-interesting",
                short_sha(anchor_sha),
            ));
            return Ok((closed, HashSet::new(), true));
        }
        // Fixed-point loop in memory across the full-range C rows.
        loop {
            let mut grew = false;
            for row in &rows {
                let mut commit_relevant = interesting.contains(&row.commit);
                for (from, to) in &row.copies {
                    if closed.contains(from) || closed.contains(to) {
                        commit_relevant = true;
                        if closed.insert(from.clone()) {
                            grew = true;
                        }
                        if closed.insert(to.clone()) {
                            grew = true;
                        }
                    }
                }
                for (from, to) in &row.renames {
                    if closed.contains(from) || closed.contains(to) {
                        commit_relevant = true;
                        if closed.insert(from.clone()) {
                            grew = true;
                        }
                        if closed.insert(to.clone()) {
                            grew = true;
                        }
                    }
                }
                if commit_relevant {
                    interesting.insert(row.commit.clone());
                }
            }
            if !grew {
                break;
            }
        }
    }

    Ok((closed, interesting, false))
}

fn short_sha(sha: &str) -> &str {
    &sha[..sha.len().min(8)]
}

// ── Session-wide hash helpers ────────────────────────────────────────────────

pub(crate) fn compute_session_hashes(repo: &gix::Repository) -> Option<([u8; 32], [u8; 32])> {
    let replace = hash_replace_refs(repo).ok()?;
    let config = hash_git_config(repo).ok()?;
    Some((replace, config))
}

fn hash_sorted_paths(paths: &HashSet<String>) -> [u8; 32] {
    let mut sorted: Vec<&String> = paths.iter().collect();
    sorted.sort();
    let joined = sorted.iter().map(|s| s.as_str()).collect::<Vec<_>>().join("\n");
    let mut h = Sha256::new();
    h.update(joined.as_bytes());
    h.finalize().into()
}

fn hash_replace_refs(repo: &gix::Repository) -> Result<[u8; 32]> {
    let work_dir = repo.workdir().unwrap_or_else(|| std::path::Path::new("."));
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
    let work_dir = repo.workdir().unwrap_or_else(|| std::path::Path::new("."));
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


/// Shared replacement for `walker::resolve_at_head`. Consumes deltas from
/// the session's grouped walk instead of running its own rev_walk +
/// per-commit `name_status`. The hunk math (per-commit blob diff for the
/// tracked path) is still per-anchor — that's the work that genuinely
/// depends on the anchor's path.
pub(crate) fn resolve_at_head_shared(
    repo: &gix::Repository,
    session: &mut ResolveSession,
    r: &Anchor,
    copy_detection: CopyDetection,
    warnings: &mut Vec<String>,
) -> Result<Option<walker::Tracked>> {
    use crate::types::AnchorExtent;
    let (rstart, rend) = match r.extent {
        AnchorExtent::LineRange { start, end } => (start, end),
        AnchorExtent::WholeFile => (1, 1),
    };
    // Clone the walk data so we can release the mutable borrow on `session`
    // and then freely access `session.cache` during the hunk loop.
    let (head_sha, commits) = {
        let group = session.ensure_group(repo, &r.anchor_sha, copy_detection, warnings)?;
        (group.head_sha.clone(), group.commits.clone())
    };
    let mut loc = walker::Tracked {
        path: r.path.clone(),
        start: rstart,
        end: rend,
    };
    // Iterate shared per-commit deltas; only the hunk math is per-anchor.
    for delta in &commits {
        // Commits classified as non-interesting by the candidate-path filter
        // have empty entries and advance_with_entries would return Unchanged
        // immediately.  Skip the call entirely to avoid the overhead across
        // the (common warm-cache) case where most commits touch none of the
        // anchor's paths.
        if delta.entries.is_empty() {
            continue;
        }
        match walker::advance_with_entries(
            repo,
            &delta.parent,
            &delta.commit,
            &loc,
            &delta.entries,
            Some(&mut session.blob_oid_memo),
        )? {
            walker::Change::Unchanged => {}
            walker::Change::Deleted => return Ok(None),
            walker::Change::Updated(next) => loc = next,
        }
    }
    if git::path_blob_at(repo, &head_sha, &loc.path).is_err() {
        return Ok(None);
    }
    Ok(Some(loc))
}

/// Shared replacement for `whole_file::follow_path_to_head`. Consumes
/// per-commit rename information from the grouped walk; runs no rev_walk
/// of its own. Returns `Some(new_path)` if any rename was followed,
/// `None` if the path is unchanged.
pub(crate) fn follow_path_to_head_shared(
    repo: &gix::Repository,
    session: &mut ResolveSession,
    anchor_sha: &str,
    path: &str,
    copy_detection: CopyDetection,
    warnings: &mut Vec<String>,
) -> Option<String> {
    let group = session
        .ensure_group(repo, anchor_sha, copy_detection, warnings)
        .ok()?;
    let mut current = path.to_string();
    for delta in &group.commits {
        for e in &delta.entries {
            if let NS::Renamed { from, to } = e
                && from == &current
            {
                current = to.clone();
                break;
            }
        }
    }
    if current == path { None } else { Some(current) }
}

#[cfg(test)]
mod candidate_filter_tests {
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

    fn commit_file(dir: &std::path::Path, path: &str, content: &str, msg: &str) {
        let abs = dir.join(path);
        if let Some(p) = abs.parent() {
            std::fs::create_dir_all(p).unwrap();
        }
        std::fs::write(abs, content).unwrap();
        run_git(dir, &["add", "."]);
        run_git(dir, &["commit", "-m", msg]);
    }

    fn init_repo() -> tempfile::TempDir {
        let td = tempdir().unwrap();
        let dir = td.path();
        run_git(dir, &["init", "--initial-branch=main"]);
        run_git(dir, &["config", "user.email", "t@t"]);
        run_git(dir, &["config", "user.name", "t"]);
        run_git(dir, &["config", "commit.gpgsign", "false"]);
        td
    }

    /// (a) Commits that don't touch any candidate path are skipped:
    /// `interesting_commits` matches the count of commits that touch
    /// the candidate path; everything else lands in `skipped_commits`.
    #[test]
    fn skips_commits_that_dont_touch_candidate() {
        let td = init_repo();
        let dir = td.path();
        commit_file(dir, "tracked.txt", "v1\n", "init tracked");
        let anchor_sha = rev_parse(dir, "HEAD");
        // 5 unrelated commits.
        for i in 0..5 {
            commit_file(dir, &format!("other_{i}.txt"), "x\n", &format!("other {i}"));
        }
        // 2 commits that touch the candidate.
        commit_file(dir, "tracked.txt", "v2\n", "edit tracked");
        commit_file(dir, "tracked.txt", "v3\n", "edit tracked again");

        let repo = gix::open(dir).unwrap();
        let mut session = ResolveSession::new(&repo);
        let mut candidate = HashSet::new();
        candidate.insert("tracked.txt".to_string());
        let mut warnings = Vec::new();
        session
            .prepare_group(
                &repo,
                &anchor_sha,
                CopyDetection::Off,
                &candidate,
                &mut warnings,
            )
            .unwrap();
        assert_eq!(session.interesting_commits, 2, "two tracked edits");
        assert_eq!(session.skipped_commits, 5, "five unrelated commits");
    }

    /// (b) A commit that renames a candidate path is kept and the new
    /// name joins the candidate set so future commits on the new name
    /// also stay interesting.
    #[test]
    fn keeps_commits_that_rename_candidate() {
        let td = init_repo();
        let dir = td.path();
        commit_file(dir, "old.txt", "v1\n", "init");
        let anchor_sha = rev_parse(dir, "HEAD");
        // Unrelated.
        commit_file(dir, "noise.txt", "x\n", "noise");
        // Rename old.txt -> new.txt
        std::fs::rename(dir.join("old.txt"), dir.join("new.txt")).unwrap();
        run_git(dir, &["add", "-A"]);
        run_git(dir, &["commit", "-m", "rename"]);
        // Edit the new name.
        commit_file(dir, "new.txt", "v2\n", "edit new");
        // Unrelated trailing.
        commit_file(dir, "trail.txt", "y\n", "trail");

        let repo = gix::open(dir).unwrap();
        let mut session = ResolveSession::new(&repo);
        let mut candidate = HashSet::new();
        candidate.insert("old.txt".to_string());
        let mut warnings = Vec::new();
        session
            .prepare_group(
                &repo,
                &anchor_sha,
                CopyDetection::SameCommit,
                &candidate,
                &mut warnings,
            )
            .unwrap();
        // Rename + edit-after-rename are both interesting; two unrelated
        // skipped. (SameCommit copy detection treats an "added path" as
        // interesting too — but the rename + the trailing add will both
        // count as interesting under that policy.)
        assert!(
            session.interesting_commits >= 2,
            "rename and follow-up edit must be interesting; got {}",
            session.interesting_commits
        );
    }

    /// (c) AnyFileInCommit copy detection: a copy that lands on a path
    /// derived from a candidate is reachable via Pass 1's `-C` phase
    /// (the candidate becomes the source of the `C<score>` row), so the
    /// copy commit is marked interesting. Unrelated added-file commits
    /// in the same range are *not* widened in (Route A is precise where
    /// the prior cheap-pass widener was conservative).
    #[test]
    fn copy_detection_widens_to_added_paths() {
        let td = init_repo();
        let dir = td.path();
        let content: String = (1..=20).map(|i| format!("line_{i}\n")).collect();
        commit_file(dir, "src.txt", &content, "init");
        let anchor_sha = rev_parse(dir, "HEAD");
        // Unrelated noise.
        commit_file(dir, "noise.txt", "x\n", "noise");
        // Copy: add a file with the same content as src.txt.
        std::fs::write(dir.join("dst.txt"), &content).unwrap();
        run_git(dir, &["add", "-A"]);
        run_git(dir, &["commit", "-m", "copy src to dst"]);

        let repo = gix::open(dir).unwrap();
        let mut session = ResolveSession::new(&repo);
        let mut candidate = HashSet::new();
        candidate.insert("src.txt".to_string());
        let mut warnings = Vec::new();
        session
            .prepare_group(
                &repo,
                &anchor_sha,
                CopyDetection::AnyFileInCommit,
                &candidate,
                &mut warnings,
            )
            .unwrap();
        // Pass 1's `-C` phase pairs dst.txt with the candidate src.txt
        // and marks the copy commit interesting; the unrelated noise
        // commit (no rename/copy row touching the trail) is skipped.
        assert_eq!(
            session.interesting_commits, 1,
            "only the copy commit is interesting under Route A"
        );
        assert_eq!(
            session.skipped_commits, 1,
            "the unrelated added-file commit is skipped"
        );
    }

    /// Route A's central correctness fix: a rename chain `foo → bar →
    /// baz` where the *first* hop's `to` (`bar`) is not yet in the
    /// candidate set must still be discovered. Pass 1 closes the trail
    /// to `{foo, bar, baz}` before any per-commit interesting-or-not
    /// classification, so `follow_path_to_head_shared` sees both rename
    /// rows and the anchor reports `Moved` to `baz`, not `Orphaned`.
    #[test]
    fn rename_chain_intermediate_name_is_closed_in_pass1() {
        let td = init_repo();
        let dir = td.path();
        // Make the file content long enough that gix's default rename
        // similarity threshold matches across hops.
        let content: String = (1..=30).map(|i| format!("line_{i}\n")).collect();
        commit_file(dir, "foo.rs", &content, "init foo");
        let anchor_sha = rev_parse(dir, "HEAD");
        // Unrelated noise.
        commit_file(dir, "noise_a.txt", "x\n", "noise a");
        // Rename foo.rs → bar.rs.
        std::fs::rename(dir.join("foo.rs"), dir.join("bar.rs")).unwrap();
        run_git(dir, &["add", "-A"]);
        run_git(dir, &["commit", "-m", "rename foo to bar"]);
        // Unrelated noise.
        commit_file(dir, "noise_b.txt", "x\n", "noise b");
        // Rename bar.rs → baz.rs.
        std::fs::rename(dir.join("bar.rs"), dir.join("baz.rs")).unwrap();
        run_git(dir, &["add", "-A"]);
        run_git(dir, &["commit", "-m", "rename bar to baz"]);
        // Trailing noise.
        commit_file(dir, "noise_c.txt", "x\n", "noise c");

        let repo = gix::open(dir).unwrap();
        let mut session = ResolveSession::new(&repo);
        let mut candidate = HashSet::new();
        candidate.insert("foo.rs".to_string());
        let mut warnings = Vec::new();
        session
            .prepare_group(
                &repo,
                &anchor_sha,
                CopyDetection::Off,
                &candidate,
                &mut warnings,
            )
            .unwrap();

        // Both rename commits are interesting; the three noise commits
        // are skipped — Pass 1's iterated `-M` query closed the trail
        // to {foo.rs, bar.rs, baz.rs} before classification.
        assert_eq!(
            session.interesting_commits, 2,
            "both rename commits must be classified interesting"
        );
        assert_eq!(
            session.skipped_commits, 3,
            "three unrelated noise commits must be skipped"
        );

        // Anchor reports `Moved` to baz.rs, not `Orphaned`.
        let new_path = follow_path_to_head_shared(
            &repo,
            &mut session,
            &anchor_sha,
            "foo.rs",
            CopyDetection::Off,
            &mut warnings,
        );
        assert_eq!(new_path.as_deref(), Some("baz.rs"));
    }

    /// Merge-commit baseline alignment: a rename that lives on a
    /// side-branch (parent P2) reaches HEAD only after a non-fast-forward
    /// merge. Pass 1 must diff the merge commit against the *same*
    /// parent Pass 2's `advance_with_entries` will see; otherwise the
    /// rename row is invisible on one or both passes and the anchor
    /// reports `Orphaned`.
    #[test]
    fn merge_commit_rename_is_visible_to_both_passes() {
        let td = init_repo();
        let dir = td.path();
        let content: String = (1..=30).map(|i| format!("line_{i}\n")).collect();
        commit_file(dir, "foo.rs", &content, "init foo");
        let anchor_sha = rev_parse(dir, "HEAD");
        // Mainline: a noise commit that doesn't touch foo.rs.
        commit_file(dir, "main_noise.txt", "x\n", "main noise");
        let main_tip = rev_parse(dir, "HEAD");
        // Side branch from anchor: rename foo.rs → bar.rs.
        run_git(dir, &["checkout", "-q", "-b", "side", &anchor_sha]);
        std::fs::rename(dir.join("foo.rs"), dir.join("bar.rs")).unwrap();
        run_git(dir, &["add", "-A"]);
        run_git(dir, &["commit", "-m", "side: rename foo to bar"]);
        // Merge side into main with --no-ff so the merge commit is real.
        run_git(dir, &["checkout", "-q", "main"]);
        run_git(dir, &["reset", "-q", "--hard", &main_tip]);
        run_git(dir, &["merge", "--no-ff", "-m", "merge side", "side"]);

        let repo = gix::open(dir).unwrap();
        let mut session = ResolveSession::new(&repo);
        let mut candidate = HashSet::new();
        candidate.insert("foo.rs".to_string());
        let mut warnings = Vec::new();
        session
            .prepare_group(
                &repo,
                &anchor_sha,
                CopyDetection::Off,
                &candidate,
                &mut warnings,
            )
            .unwrap();
        // The trail must reach bar.rs regardless of which parent the
        // merge commit is diffed against.
        let new_path = follow_path_to_head_shared(
            &repo,
            &mut session,
            &anchor_sha,
            "foo.rs",
            CopyDetection::Off,
            &mut warnings,
        );
        assert_eq!(
            new_path.as_deref(),
            Some("bar.rs"),
            "side-branch rename foo.rs → bar.rs must be observed across the merge"
        );
    }

    /// (d) Two anchors share a walk via the same candidate-path union.
    #[test]
    fn union_of_paths_keeps_each_anchor_visible() {
        let td = init_repo();
        let dir = td.path();
        commit_file(dir, "a.txt", "v1\n", "init a");
        commit_file(dir, "b.txt", "v1\n", "init b");
        let anchor_sha = rev_parse(dir, "HEAD");
        // Commit touching only a.
        commit_file(dir, "a.txt", "v2\n", "edit a");
        // Commit touching only b.
        commit_file(dir, "b.txt", "v2\n", "edit b");
        // Unrelated.
        commit_file(dir, "c.txt", "v1\n", "init c");

        let repo = gix::open(dir).unwrap();
        let mut session = ResolveSession::new(&repo);
        let mut candidate = HashSet::new();
        candidate.insert("a.txt".to_string());
        candidate.insert("b.txt".to_string());
        let mut warnings = Vec::new();
        session
            .prepare_group(
                &repo,
                &anchor_sha,
                CopyDetection::Off,
                &candidate,
                &mut warnings,
            )
            .unwrap();
        // Off → no copy widening; only the two real edits are interesting.
        assert_eq!(session.interesting_commits, 2);
        assert_eq!(session.skipped_commits, 1);
    }
}

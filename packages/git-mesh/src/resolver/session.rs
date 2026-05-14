//! `ResolveSession` — engine-wide shared state for one `stale` run.
//!
//! The reverse-indexed HEAD walk (built by [`ResolveSession::build_reverse_walk`])
//! runs once and produces per-anchor commit deltas. Each anchor's classifier
//! consumes its slice of the walk output instead of running its own per-anchor
//! walk. The session is constructed once at the top of the `stale` CLI path and
//! threaded through `resolve_anchor_inner`. There is no caching across runs —
//! the session lives only for the duration of one engine call and is dropped
//! when it returns.

use crate::Result;
use crate::git;
use crate::resolver::bloom::CommitGraphBloom;
use crate::resolver::cache::Cache;
use crate::resolver::walker::{self, NS};
use crate::types::{Anchor, CopyDetection};
use std::collections::{HashMap, HashSet};

/// One per-commit slice of the shared walk: `(parent_sha, commit_sha,
/// name_status_entries)`. Produced by the reverse-indexed walk once per
/// commit that touches a tracked path. The hunk math
/// (`walker::advance_with_entries`) is still per-anchor — that's the work
/// that genuinely depends on the anchor's path.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct CommitDelta {
    pub(crate) parent: String,
    pub(crate) commit: String,
    pub(crate) entries: Vec<NS>,
}

/// Maps each tracked path to the set of anchors that depend on it,
/// enabling a single reverse-indexed walk to fan out per-commit
/// path-change results to every affected anchor.
#[derive(Debug, Clone)]
pub(crate) struct AnchorReverseIndex {
    /// Every (path, anchor_sha) pair that any mesh anchors against.
    /// Keyed by path so a per-commit "did this commit touch P?" answer
    /// can fan out to every (mesh, anchor_id) waiting on P.
    pub(crate) by_path: HashMap<Vec<u8>, Vec<AnchorRef>>,
    /// Union of all anchor_sha values — the walk's stop set.
    /// A commit is "interesting" iff it touches some path in by_path
    /// AND lies between HEAD and some anchor_sha still being resolved.
    pub(crate) anchor_shas: HashSet<gix::ObjectId>,
}

#[derive(Debug, Clone)]
pub(crate) struct AnchorRef {
    pub(crate) mesh_name: String,
    pub(crate) anchor_id: String,
    pub(crate) anchor_sha: gix::ObjectId,
}

impl AnchorReverseIndex {
    /// Build the reverse index from all meshes' anchors in a single pass.
    /// `meshes` is the list of (mesh_name, mesh) pairs being resolved.
    pub(crate) fn from_meshes(meshes: &[(String, crate::types::Mesh)]) -> Self {
        let mut by_path: HashMap<Vec<u8>, Vec<AnchorRef>> = HashMap::new();
        let mut anchor_shas: HashSet<gix::ObjectId> = HashSet::new();

        for (mesh_name, mesh) in meshes {
            for (anchor_id, anchor) in &mesh.anchors_v2 {
                let sha = match gix::ObjectId::from_hex(anchor.anchor_sha.as_bytes()) {
                    Ok(oid) => oid,
                    Err(_) => continue, // skip malformed SHAs
                };
                anchor_shas.insert(sha);
                by_path
                    .entry(anchor.path.as_bytes().to_vec())
                    .or_default()
                    .push(AnchorRef {
                        mesh_name: mesh_name.clone(),
                        anchor_id: anchor_id.clone(),
                        anchor_sha: sha,
                    });
            }
        }

        Self { by_path, anchor_shas }
    }
}

/// Output of the single reverse-indexed HEAD walk.
///
/// Produced by [`ResolveSession::build_reverse_walk`] and consumed by
/// `resolve_at_head_shared` / `follow_path_to_head_shared`.
pub(crate) struct ReverseWalkOutput {
    /// HEAD oid at walk time.
    pub(crate) head_sha: String,
    /// Per-anchor commit deltas (oldest-first), keyed by `(mesh_name, anchor_id)`.
    ///
    /// Each vec contains only the commits that touch that anchor's path,
    /// including commits where the path was renamed (the entries include
    /// the `Renamed` NS variant so downstream consumers can follow the trail).
    pub(crate) per_anchor_deltas: HashMap<(String, String), Vec<CommitDelta>>,
}

/// Engine-wide shared state: session-scoped caches and counters for one
/// `stale` run.
pub(crate) struct ResolveSession {
    pub(crate) reverse_walk_output: Option<ReverseWalkOutput>,
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
    /// Session-scoped blob OID memo: `(commit_sha, path) → blob_oid`.
    ///
    /// `path_blob_at` requires a tree traversal for every `(commit, path)`
    /// pair. Many anchors in a mesh share the same underlying files and
    /// commit history, so the same pair is requested repeatedly across
    /// anchors. This memo eliminates the redundant traversals within a
    /// single `stale` run; it does not persist across invocations.
    pub(crate) blob_oid_memo: HashMap<(String, String), Option<String>>,
    /// Counters: anchors classified by terminal status. Updated from the
    /// per-anchor loop in `engine::resolve_loaded_mesh_with_state` after each
    /// `resolve_anchor_inner` call.
    pub(crate) anchors_fresh: u64,
    pub(crate) anchors_moved: u64,
    pub(crate) anchors_changed: u64,
    pub(crate) anchors_orphaned: u64,
    pub(crate) anchors_merge_conflict: u64,
    pub(crate) anchors_unavailable: u64,
    /// Counter: anchors skipped entirely via [`can_skip_clean_head_pinned_mesh`]
    /// (the whole mesh was clean and pinned at HEAD). These anchors are not
    /// resolved individually, but they are counted toward `anchors_total`.
    pub(crate) anchors_skipped_clean_head: u64,
    /// Counter: anchors that returned via [`clean_head_fast_path`] (early
    /// return). The remainder went through the full layer-comparison path
    /// (`anchors_total - anchors_fast_path_hits - anchors_skipped_clean_head
    ///  == anchors_full_resolution`).
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
    /// Reverse-indexed walk: commits where Bloom said "definitely no tracked path changed".
    pub(crate) walk_bloom_skips: u64,
    /// Reverse-indexed walk: paths Bloom said "maybe" but tree-diff showed unchanged.
    pub(crate) walk_bloom_false_positives: u64,
    /// Reverse-indexed walk: commits that ran a tree-diff.
    pub(crate) walk_tree_diffs: u64,
    /// Reverse-indexed walk: total commits visited.
    pub(crate) walk_commits_visited: u64,
    /// Reverse-indexed walk: wall-clock ms to build the AnchorReverseIndex.
    pub(crate) reverse_index_build_ms: u64,
    /// Warnings accumulated during the reverse-indexed walk (rename budget
    /// notes, etc.). Forwarded to `EngineState::finish` for stderr output.
    pub(crate) warnings: Vec<String>,
}

impl ResolveSession {
    pub(crate) fn new(repo: &gix::Repository) -> Self {
        let cache = Cache::open(repo).unwrap_or_else(|_| {
            // Cache failures degrade silently to a disabled cache.
            Cache::open_disabled()
        });
        Self {
            reverse_walk_output: None,
            drift_locus_hits: 0,
            drift_locus_misses: 0,
            filter_attr_hits: 0,
            filter_attr_misses: 0,
            cache,
            known_head_ancestors: std::collections::HashMap::new(),
            blob_oid_memo: HashMap::new(),
            anchors_fresh: 0,
            anchors_moved: 0,
            anchors_changed: 0,
            anchors_orphaned: 0,
            anchors_merge_conflict: 0,
            anchors_unavailable: 0,
            anchors_skipped_clean_head: 0,
            anchors_fast_path_hits: 0,
            anchors_full_resolution: 0,
            per_anchor_us: Vec::new(),
            per_anchor_trace: None,
            walk_bloom_skips: 0,
            walk_bloom_false_positives: 0,
            walk_tree_diffs: 0,
            walk_commits_visited: 0,
            reverse_index_build_ms: 0,
            warnings: Vec::new(),
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
            + self.anchors_skipped_clean_head
    }

    /// Build the reverse-indexed walk: one pass from HEAD, Bloom-gated,
    /// that produces per-anchor commit deltas for every anchor in every mesh.
    ///
    /// The walk tracks per-anchor current paths through rename chains:
    /// when a commit renames an anchor's path from A to B, subsequent commits
    /// query the Bloom filter for B (the new name).
    ///
    /// Terminates when every `anchor_sha` in the reverse index has been observed
    /// (passed in the walk) or the walk reaches the root.
    ///
    /// The output is stored internally on `self.reverse_walk_output` so that
    /// consumers (`resolve_at_head_shared`, `follow_path_to_head_shared`) can
    /// read it without callers having to thread it through every signature.
    ///
    /// ## Fail-closed
    ///
    /// Returns `Err` if the repository does not have a commit-graph file with
    /// changed-path Bloom filters. No silent fallback — per `<fail-closed>`.
    pub(crate) fn build_reverse_walk(
        &mut self,
        repo: &gix::Repository,
        meshes: &[(String, crate::types::Mesh)],
    ) -> Result<()> {
        // 1. Build the reverse index.
        let t0 = std::time::Instant::now();
        let reverse_index = AnchorReverseIndex::from_meshes(meshes);
        self.reverse_index_build_ms = t0.elapsed().as_millis() as u64;

        // 2. Compute the most permissive copy_detection across all meshes.
        //    The walk produces a single name_status per commit; using the max
        //    ensures copies are detected when any mesh requires them, even
        //    though walker::advance_with_entries later filters per-anchor path.
        let max_copy = meshes
            .iter()
            .map(|(_, m)| m.config.copy_detection)
            .max()
            .unwrap_or(CopyDetection::Off);

        // 3. Open the Bloom filter (fail-closed — no silent fallback).
        let bloom = CommitGraphBloom::open(repo)
            .map_err(crate::Error::Git)?;

        // 3. Get HEAD oid.
        let head_sha = git::head_oid(repo)?;
        let head_oid = gix::ObjectId::from_hex(head_sha.as_bytes())
            .map_err(|e| crate::Error::Git(format!("parse HEAD: {e}")))?;

        // 4. Initialize per-anchor state.
        let mut per_anchor: Vec<AnchorWalkState> = Vec::new();
        for (mesh_name, mesh) in meshes {
            for (anchor_id, anchor) in &mesh.anchors_v2 {
                let sha = match gix::ObjectId::from_hex(anchor.anchor_sha.as_bytes()) {
                    Ok(s) => s,
                    Err(_) => continue,
                };
                per_anchor.push(AnchorWalkState {
                    mesh_name: mesh_name.clone(),
                    anchor_id: anchor_id.clone(),
                    anchor_sha: sha,
                    current_path: anchor.path.as_bytes().to_vec(),
                    deltas: Vec::new(),
                    anchor_passed: false,
                });
            }
        }

        // 5. Walk from HEAD reverse-chronologically.
        let mut stop_set: HashSet<gix::ObjectId> = reverse_index.anchor_shas;

        let walk = repo
            .rev_walk([head_oid])
            .sorting(gix::revision::walk::Sorting::BreadthFirst)
            .all()
            .map_err(|e| crate::Error::Git(format!("rev walk: {e}")))?;

        for info in walk {
            let info = info.map_err(|e| crate::Error::Git(format!("rev walk commit: {e}")))?;
            let commit_oid = info.id;

            // Check stop condition: remove this commit from the set of
            // anchor_shas not yet observed. Mark the matching anchor as
            // passed so we stop recording new deltas for it.
            if stop_set.remove(&commit_oid) {
                for state in &mut per_anchor {
                    if state.anchor_sha == commit_oid {
                        state.anchor_passed = true;
                    }
                }
            }
            if stop_set.is_empty() {
                break;
            }

            self.walk_commits_visited += 1;

            // Collect unique tracked paths across all active (non-passed)
            // anchors into a single set for Bloom querying.
            let mut bloom_check_paths: Vec<Vec<u8>> = Vec::new();
            let mut seen_paths: HashSet<Vec<u8>> = HashSet::new();
            for state in &per_anchor {
                if state.anchor_passed {
                    continue;
                }
                if seen_paths.insert(state.current_path.clone()) {
                    bloom_check_paths.push(state.current_path.clone());
                }
            }

            if bloom_check_paths.is_empty() {
                continue;
            }

            // Bloom filter gate: paths that might have changed.
            // When the commit is not in the commit-graph (no Bloom
            // position), we cannot Bloom-check and must assume all
            // tracked paths may have changed.  The Bloom filter is an
            // optimization, not a correctness gate — skipping commits
            // that might contain relevant changes would produce
            // incorrect results.
            let mut bloom_positives: Vec<Vec<u8>> = Vec::new();
            if let Some(commit_pos) = bloom.commit_position(&commit_oid) {
                for path in &bloom_check_paths {
                    if bloom.maybe_contains(commit_pos, path) {
                        bloom_positives.push(path.clone());
                    }
                }
                if bloom_positives.is_empty() {
                    self.walk_bloom_skips += 1;
                    continue;
                }
            } else {
                // Not in commit-graph: check all tracked paths.
                bloom_positives = bloom_check_paths;
            }

            // Bloom says "maybe" for at least one path — run tree-diff.
            self.walk_tree_diffs += 1;

            let commit_sha_str = commit_oid.to_string();
            let commit_obj = repo
                .find_commit(commit_oid)
                .map_err(|e| crate::Error::Git(format!("find commit {commit_oid}: {e}")))?;
            let parent_oid = match commit_obj.parent_ids().next() {
                Some(p) => p.detach(),
                None => {
                    // Root commit — nothing older to diff against.
                    continue;
                }
            };
            let parent_sha_str = parent_oid.to_string();

            // Run name_status with max copy detection across all meshes.
            // Different meshes may have different copy_detection settings; using
            // the most permissive ensures that copies are available for every
            // mesh even though walker::advance_with_entries later filters per
            // anchor. Warnings (rename budget notes, budget downgrade notes)
            // are accumulated on the session for stderr output via
            // EngineState::finish.
            let entries = walker::name_status(
                repo,
                &parent_sha_str,
                &commit_sha_str,
                max_copy,
                &mut self.warnings,
            )?;

            // Count false positives: paths Bloom said "maybe" but that
            // don't appear in the actual tree-diff result.
            let actual_paths: HashSet<Vec<u8>> = entries
                .iter()
                .flat_map(|e| {
                    let mut paths: Vec<Vec<u8>> = Vec::new();
                    match e {
                        NS::Added { path }
                        | NS::Modified { path }
                        | NS::Deleted { path } => {
                            paths.push(path.as_bytes().to_vec())
                        }
                        NS::Renamed { from, to } | NS::Copied { from, to } => {
                            paths.push(from.as_bytes().to_vec());
                            paths.push(to.as_bytes().to_vec());
                        }
                    }
                    paths
                })
                .collect();

            for bp in &bloom_positives {
                if !actual_paths.contains(bp) {
                    self.walk_bloom_false_positives += 1;
                }
            }

            // Fan out to anchors whose tracked path was affected.
            let delta = CommitDelta {
                parent: parent_sha_str,
                commit: commit_sha_str,
                entries: entries.clone(),
            };

            'anchor_loop: for state in &mut per_anchor {
                if state.anchor_passed {
                    continue;
                }

                // Check each entry against this anchor's current tracked path.
                for entry in &entries {
                    let affects = match entry {
                        NS::Added { path }
                        | NS::Modified { path }
                        | NS::Deleted { path } => {
                            path.as_bytes() == state.current_path.as_slice()
                        }
                        NS::Renamed { from, to: _ } | NS::Copied { from, to: _ } => {
                            from.as_bytes() == state.current_path.as_slice()
                        }
                    };

                    if affects {
                        // Update tracked path for renames.
                        if let NS::Renamed { from: _, to }
                        | NS::Copied { from: _, to } = entry
                        {
                            state.current_path = to.as_bytes().to_vec();
                        }

                        state.deltas.push(delta.clone());
                        continue 'anchor_loop;
                    }
                }
            }
        }

        // 6. Build the output. Reverse each anchor's deltas to oldest-first
        // order (the walk produced newest-first).
        let mut per_anchor_deltas: HashMap<(String, String), Vec<CommitDelta>> =
            HashMap::new();
        for state in per_anchor {
            let mut deltas = state.deltas;
            deltas.reverse();
            per_anchor_deltas.insert((state.mesh_name, state.anchor_id), deltas);
        }

        // Store in self so consumers (resolve_at_head_shared,
        // follow_path_to_head_shared) can read the output without
        // the caller having to thread it through every signature.
        self.reverse_walk_output = Some(ReverseWalkOutput {
            head_sha,
            per_anchor_deltas,
        });

        Ok(())
    }
}

/// Per-anchor walk state maintained during the reverse-indexed walk.
///
/// Tracks the current path (updated through renames) and whether the
/// anchor_sha has been passed in the walk.
struct AnchorWalkState {
    mesh_name: String,
    anchor_id: String,
    anchor_sha: gix::ObjectId,
    /// The path we are currently tracking for this anchor. Updated when
    /// a rename/copy entry matches.
    current_path: Vec<u8>,
    /// Accumulated commit deltas (newest-first during the walk; reversed
    /// to oldest-first in the output).
    deltas: Vec<CommitDelta>,
    /// Set to true once the walk passes this anchor's anchor_sha. After
    /// that point, no more deltas are recorded for this anchor.
    anchor_passed: bool,
}


/// Shared replacement for `walker::resolve_at_head`. Consumes deltas from
/// the session's reverse-indexed walk output instead of running its own
/// rev_walk + per-commit `name_status`. The hunk math (per-commit blob
/// diff for the tracked path) is still per-anchor — that's the work that
/// genuinely depends on the anchor's path.
pub(crate) fn resolve_at_head_shared(
    repo: &gix::Repository,
    session: &mut ResolveSession,
    r: &Anchor,
    mesh_name: &str,
    anchor_id: &str,
    _warnings: &mut Vec<String>,
) -> Result<Option<walker::Tracked>> {
    use crate::types::AnchorExtent;
    let (rstart, rend) = match r.extent {
        AnchorExtent::LineRange { start, end } => (start, end),
        AnchorExtent::WholeFile => (1, 1),
    };
    // Clone the walk data so we can release the borrow on
    // `session.reverse_walk_output` and then freely access
    // `session.blob_oid_memo` during the hunk loop.
    let (head_sha, deltas) = {
        let output = session.reverse_walk_output.as_ref()
            .ok_or_else(|| crate::Error::Git("reverse walk not built".into()))?;
        let head_sha = output.head_sha.clone();
        let deltas = output.per_anchor_deltas
            .get(&(mesh_name.to_string(), anchor_id.to_string()))
            .cloned()
            .unwrap_or_default();
        (head_sha, deltas)
    };
    let mut loc = walker::Tracked {
        path: r.path.clone(),
        start: rstart,
        end: rend,
    };
    // Iterate shared per-commit deltas; only the hunk math is per-anchor.
    for delta in &deltas {
        // Commits with empty entries have no affect on this anchor;
        // advance_with_entries would return Unchanged immediately.
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
/// per-commit rename information from the reverse-indexed walk output;
/// runs no rev_walk of its own. Returns `Some(new_path)` if any rename
/// was followed, `None` if the path is unchanged.
pub(crate) fn follow_path_to_head_shared(
    _repo: &gix::Repository,
    session: &mut ResolveSession,
    mesh_name: &str,
    anchor_id: &str,
    path: &str,
    _warnings: &mut Vec<String>,
) -> Option<String> {
    let output = session.reverse_walk_output.as_ref()?;
    let deltas = output.per_anchor_deltas
        .get(&(mesh_name.to_string(), anchor_id.to_string()))?;
    let mut current = path.to_string();
    for delta in deltas {
        for e in &delta.entries {
            if let NS::Renamed { from, to } | NS::Copied { from, to } = e
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
mod tests {
    use super::*;

    #[test]
    fn anchors_total_includes_skipped_clean_head() {
        let session = ResolveSession {
            reverse_walk_output: None,
            drift_locus_hits: 0,
            drift_locus_misses: 0,
            filter_attr_hits: 0,
            filter_attr_misses: 0,
            cache: crate::resolver::cache::Cache::open_disabled(),
            known_head_ancestors: std::collections::HashMap::new(),
            blob_oid_memo: HashMap::new(),
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
            walk_bloom_skips: 0,
            walk_bloom_false_positives: 0,
            walk_tree_diffs: 0,
            walk_commits_visited: 0,
            reverse_index_build_ms: 0,
            warnings: Vec::new(),
            anchors_skipped_clean_head: 50,
        };

        let total = session.anchors_total();

        // Decomposition identity per card: each anchor is either skipped
        // clean-head, resolved via a per-anchor fast-path, or goes through
        // full resolution.
        let decomposed = session.anchors_skipped_clean_head
            + session.anchors_fast_path_hits
            + session.anchors_full_resolution;
        assert_eq!(total, decomposed,
            "anchors-total must equal skipped-clean-head + fast-path-hits + full-resolution");
        assert_eq!(total, 50,
            "anchors-total must count anchors that were skipped clean-head");
    }

    #[test]
    fn decomposition_identity_mixed_buckets() {
        let session = ResolveSession {
            reverse_walk_output: None,
            drift_locus_hits: 0,
            drift_locus_misses: 0,
            filter_attr_hits: 0,
            filter_attr_misses: 0,
            cache: crate::resolver::cache::Cache::open_disabled(),
            known_head_ancestors: std::collections::HashMap::new(),
            blob_oid_memo: HashMap::new(),
            anchors_fresh: 0,
            anchors_moved: 3,
            anchors_changed: 2,
            anchors_orphaned: 0,
            anchors_merge_conflict: 0,
            anchors_unavailable: 1,
            anchors_fast_path_hits: 4,
            anchors_full_resolution: 2,
            per_anchor_us: Vec::new(),
            per_anchor_trace: None,
            walk_bloom_skips: 0,
            walk_bloom_false_positives: 0,
            walk_tree_diffs: 0,
            walk_commits_visited: 0,
            reverse_index_build_ms: 0,
            warnings: Vec::new(),
            anchors_skipped_clean_head: 40,
        };

        let total = session.anchors_total();

        // The status-bucket total should account for moved+changed+unavailable = 6.
        // But the decomposition identity is: total == skipped-clean-head + fast-path-hits + full-resolution.
        assert_eq!(
            total,
            session.anchors_fresh
                + session.anchors_moved
                + session.anchors_changed
                + session.anchors_orphaned
                + session.anchors_merge_conflict
                + session.anchors_unavailable
                + session.anchors_skipped_clean_head,
            "anchors-total must include skipped-clean-head alongside per-status buckets"
        );

        let decomposed = session.anchors_skipped_clean_head
            + session.anchors_fast_path_hits
            + session.anchors_full_resolution;
        assert_eq!(total, decomposed,
            "anchors-total == skipped-clean-head + fast-path-hits + full-resolution");
        assert_eq!(total, 46);
    }
}

//! HEAD-source drift-locus attribution. Walks `anchor..HEAD` forward
//! along the anchored path and returns the first commit that mutates
//! the anchored byte range (`ChangedAt`), the commit that removes or
//! renames the path (`OrphanedAt`), or marks the anchor as unreachable
//! from HEAD.
//!
//! Only meaningful when the engine attributes drift to the HEAD layer
//! (`AnchorResolved.source == Some(DriftSource::Head)`); for shallower
//! sources the caller already has a layer-local label and this walk is
//! skipped.

use crate::Result;
use crate::git;
use crate::resolver::cache::{DriftLocusCacheKey, DriftLocusCachedValue};
use crate::resolver::session::ResolveSession;
use crate::resolver::walker;
use crate::types::{AnchorExtent, AnchorResolved, DriftLocus, DriftSource};
use std::collections::HashMap;

/// Null-commit sentinel used in `DriftLocusCachedValue.answer_commit` when
/// the cached result is `Unreachable` (no commit to name).
const NULL_COMMIT: &str = "0000000000000000000000000000000000000000";

/// Forward-walk `anchor..HEAD` along the anchored path and return the
/// `DriftLocus` that explains the drift. Returns `None` when
/// `resolved.source != Some(Head)` or the walk finds no overlapping
/// commit; the caller falls back on the per-layer label.
///
/// `session` provides the sqlite cache, hit/miss counters, and the
/// per-session HEAD-ancestor memo. `copy_detection` is forwarded for the
/// cache key so rows keyed to one CD level are not reused for another.
pub(crate) fn drift_locus(
    repo: &gix::Repository,
    resolved: &AnchorResolved,
    session: &mut ResolveSession,
    copy_detection: crate::types::CopyDetection,
) -> Result<Option<DriftLocus>> {
    // The walk applies to (a) HEAD-attributed drift (Changed-in-HEAD) and
    // (b) Orphaned anchors, where the caller (`populate_drift_locus`) asks
    // us to describe the orphaning commit (rename or deletion) when the
    // anchor itself is still reachable from HEAD.
    if resolved.source != Some(DriftSource::Head)
        && !matches!(resolved.status, crate::types::AnchorStatus::Orphaned)
    {
        return Ok(None);
    }

    // Only cache line-range anchors — whole-file exits early below anyway.
    let (anchored_start, anchored_end) = match resolved.anchored.extent {
        AnchorExtent::LineRange { start, end } => (start, end),
        AnchorExtent::WholeFile => return Ok(None),
    };

    let blob_oid = resolved
        .anchored
        .blob
        .map(|o| o.to_string())
        .unwrap_or_default();
    let rename_budget = walker::rename_budget();

    let cache_key = if session.cache.is_enabled() {
        Some(DriftLocusCacheKey {
            anchor_sha: resolved.anchor_sha.clone(),
            path: resolved.anchored.path.to_string_lossy().into_owned(),
            blob_oid: blob_oid.clone(),
            range_start: anchored_start,
            range_end: anchored_end,
            copy_detection,
            rename_budget,
        })
    } else {
        None
    };

    // ── Tier 5 cache probe ───────────────────────────────────────────────────
    if let Some(ref key) = cache_key
        && let Some(cached) = session.cache.drift_locus_get(key)
    {
        // Validate answer_commit: is it still an ancestor of HEAD?
        let valid = validate_answer_commit(
            repo,
            &cached.answer_commit,
            &mut session.known_head_ancestors,
        );
        if valid {
            session.drift_locus_hits += 1;
            return Ok(decode_drift_locus(&cached));
        }
        // Ancestor check failed → treat as miss, recompute below.
        // Do NOT delete the row (no-in-band revocation).
    }
    session.drift_locus_misses += 1;

    // ── Miss path: full walk ─────────────────────────────────────────────────
    let result = drift_locus_walk(repo, resolved, anchored_start, anchored_end, &blob_oid, &mut session.known_head_ancestors)?;

    // Store result in cache.
    if let Some(ref key) = cache_key {
        let value = encode_drift_locus(result.as_ref());
        if let Err(e) = session.cache.with_write_txn(|txn| {
            session.cache.drift_locus_put(txn, key, &value)
        }) {
            eprintln!("drift_locus cache write error (ignored): {e}");
        }
    }

    Ok(result)
}

/// Perform the full forward walk without touching the cache.
fn drift_locus_walk(
    repo: &gix::Repository,
    resolved: &AnchorResolved,
    anchored_start: u32,
    anchored_end: u32,
    blob_oid: &str,
    known_head_ancestors: &mut std::collections::HashMap<
        gix::ObjectId,
        std::collections::HashSet<gix::ObjectId>,
    >,
) -> Result<Option<DriftLocus>> {
    let head_hex = git::head_oid(repo)?;
    let head_id = match repo.rev_parse_single(head_hex.as_str()) {
        Ok(id) => id.detach(),
        Err(_) => return Ok(None),
    };
    let anchor_id = match repo.rev_parse_single(resolved.anchor_sha.as_str()) {
        Ok(id) => id.detach(),
        Err(_) => return Ok(Some(DriftLocus::Unreachable)),
    };

    // Determine reachability and the anchor..HEAD walk in forward
    // (oldest-first) order. `rev_walk` is newest-first by default; collect
    // and reverse.
    let walk = match repo.rev_walk([head_id]).with_hidden([anchor_id]).all() {
        Ok(w) => w,
        Err(_) => return Ok(Some(DriftLocus::Unreachable)),
    };
    let mut commits: Vec<gix::ObjectId> = Vec::new();
    for info in walk {
        match info {
            Ok(i) => commits.push(i.id),
            Err(_) => return Ok(Some(DriftLocus::Unreachable)),
        }
    }
    if commits.is_empty() {
        // Anchor is at HEAD (or beyond): no commit on the path. The
        // caller should treat this as "no Head locus".
        return Ok(None);
    }
    // Every commit in this walk is an ancestor of HEAD by construction
    // (rev_walk visits `HEAD..anchor`, so all visited commits reach HEAD).
    // The memo is keyed by HEAD so entries from a stale HEAD do not leak
    // into a subsequent check against a different HEAD.
    let entry = known_head_ancestors.entry(head_id).or_default();
    for &id in &commits {
        entry.insert(id);
    }
    commits.reverse();

    let anchored_text = git::read_git_text(repo, blob_oid).unwrap_or_default();
    let anchored_lines: Vec<&str> = anchored_text.lines().collect();
    let a_lo = (anchored_start as usize).saturating_sub(1);
    let a_hi = (anchored_end as usize).min(anchored_lines.len());
    let anchored_slice: Vec<&str> = if a_lo <= a_hi {
        anchored_lines[a_lo..a_hi].to_vec()
    } else {
        Vec::new()
    };

    // Track the path across renames as we step forward.
    let current_path = resolved.anchored.path.to_string_lossy().into_owned();

    // In-process blob-text memo: one read per OID per drift_locus call.
    let mut blob_text_memo: HashMap<gix::ObjectId, String> = HashMap::new();

    for commit_id in commits {
        let commit = match repo.find_commit(commit_id) {
            Ok(c) => c,
            Err(_) => continue,
        };
        let new_tree = match commit.tree() {
            Ok(t) => t,
            Err(_) => continue,
        };
        let parents: Vec<_> = commit.parent_ids().map(|p| p.detach()).collect();
        let old_tree = match parents.first() {
            Some(pid) => match repo.find_commit(*pid) {
                Ok(parent) => parent.tree().unwrap_or_else(|_| repo.empty_tree()),
                Err(_) => repo.empty_tree(),
            },
            None => repo.empty_tree(),
        };

        let mut opts = gix::diff::Options::default();
        opts.track_rewrites(Some(Default::default()));
        let changes = match repo.diff_tree_to_tree(Some(&old_tree), Some(&new_tree), Some(opts)) {
            Ok(c) => c,
            Err(_) => continue,
        };

        let path_bytes = current_path.as_bytes();
        for change in changes.iter() {
            use gix::object::tree::diff::ChangeDetached;
            match change {
                ChangeDetached::Deletion { location, .. } => {
                    if location.as_slice() == path_bytes {
                        return Ok(Some(DriftLocus::OrphanedAt(commit_id)));
                    }
                }
                ChangeDetached::Rewrite {
                    source_location,
                    location,
                    ..
                } => {
                    if source_location.as_slice() == path_bytes {
                        // Rename of the anchored path: per §5, treat as
                        // orphan locus.
                        let _ = location;
                        return Ok(Some(DriftLocus::OrphanedAt(commit_id)));
                    }
                }
                ChangeDetached::Modification {
                    location,
                    previous_id,
                    id,
                    ..
                } => {
                    if location.as_slice() != path_bytes {
                        continue;
                    }
                    if range_overlaps_memo(
                        repo,
                        previous_id,
                        id,
                        &anchored_slice,
                        anchored_start,
                        anchored_end,
                        &mut blob_text_memo,
                    ) {
                        return Ok(Some(DriftLocus::ChangedAt(commit_id)));
                    }
                }
                ChangeDetached::Addition { location, id, .. } => {
                    // Addition can match when the path was previously
                    // removed and reintroduced. Compare the new blob's
                    // range against the anchored slice.
                    if location.as_slice() != path_bytes {
                        continue;
                    }
                    if range_overlaps_memo(
                        repo,
                        &gix::ObjectId::null(gix::hash::Kind::Sha1),
                        id,
                        &anchored_slice,
                        anchored_start,
                        anchored_end,
                        &mut blob_text_memo,
                    ) {
                        return Ok(Some(DriftLocus::ChangedAt(commit_id)));
                    }
                }
            }
        }
    }
    Ok(None)
}

// ── Cache encode / decode helpers ────────────────────────────────────────────

fn encode_drift_locus(locus: Option<&DriftLocus>) -> DriftLocusCachedValue {
    match locus {
        None => DriftLocusCachedValue {
            variant: 3, // None / no locus found
            answer_commit: NULL_COMMIT.to_string(),
        },
        Some(DriftLocus::Unreachable) => DriftLocusCachedValue {
            variant: 0,
            answer_commit: NULL_COMMIT.to_string(),
        },
        Some(DriftLocus::ChangedAt(id)) => DriftLocusCachedValue {
            variant: 1,
            answer_commit: id.to_string(),
        },
        Some(DriftLocus::OrphanedAt(id)) => DriftLocusCachedValue {
            variant: 2,
            answer_commit: id.to_string(),
        },
    }
}

/// Decode a cached value back to `Option<DriftLocus>`.
///
/// Returns `None` for variant 3 (the stored "no locus" sentinel), so that a
/// `None` result round-trips as `None` and not as `Some(Unreachable)`.
fn decode_drift_locus(cached: &DriftLocusCachedValue) -> Option<DriftLocus> {
    match cached.variant {
        1 => {
            let id = parse_oid(&cached.answer_commit);
            Some(DriftLocus::ChangedAt(id))
        }
        2 => {
            let id = parse_oid(&cached.answer_commit);
            Some(DriftLocus::OrphanedAt(id))
        }
        3 => None, // "no locus found" sentinel
        _ => Some(DriftLocus::Unreachable),
    }
}

fn parse_oid(hex: &str) -> gix::ObjectId {
    use std::str::FromStr;
    gix::ObjectId::from_str(hex).unwrap_or_else(|_| gix::ObjectId::null(gix::hash::Kind::Sha1))
}

/// Check whether `answer_commit_hex` is in the known-ancestors memo or can
/// be confirmed as an ancestor of HEAD via a gix merge-base call.
///
/// On confirmed ancestor, seeds the memo with the result.
/// Returns `true` if the commit is an ancestor of HEAD (or is HEAD itself).
/// Returns `false` for the null sentinel, unknown commits, or errors.
fn validate_answer_commit(
    repo: &gix::Repository,
    answer_commit_hex: &str,
    known_head_ancestors: &mut std::collections::HashMap<
        gix::ObjectId,
        std::collections::HashSet<gix::ObjectId>,
    >,
) -> bool {
    if answer_commit_hex == NULL_COMMIT {
        // Unreachable sentinel — always valid (no commit to check).
        return true;
    }
    use std::str::FromStr;
    let answer_id = match gix::ObjectId::from_str(answer_commit_hex) {
        Ok(id) => id,
        Err(_) => return false,
    };
    let head_hex = match git::head_oid(repo) {
        Ok(h) => h,
        Err(_) => return false,
    };
    let head_id = match gix::ObjectId::from_str(&head_hex) {
        Ok(id) => id,
        Err(_) => return false,
    };
    // Fast path: already in memo for *this* HEAD. Entries from a prior
    // HEAD are ignored — they may name commits that are not ancestors of
    // the current HEAD.
    if let Some(set) = known_head_ancestors.get(&head_id)
        && set.contains(&answer_id)
    {
        return true;
    }
    // Slow path: gix merge-base check.
    // merge_base(A, B) == A means A is an ancestor of B.
    match repo.merge_base(answer_id, head_id) {
        Ok(base) if base.detach() == answer_id => {
            known_head_ancestors
                .entry(head_id)
                .or_default()
                .insert(answer_id);
            true
        }
        _ => false,
    }
}

/// Return `true` if the anchored byte range differs between two blobs.
/// The null OID is treated as empty content (used for synthetic
/// addition-from-nothing comparisons).
///
/// `blob_text_memo` is an in-process memo for the current `drift_locus_walk`
/// call that eliminates repeated git-object reads for the same OID.
fn range_overlaps_memo(
    repo: &gix::Repository,
    previous_id: &gix::ObjectId,
    id: &gix::ObjectId,
    anchored_slice: &[&str],
    anchored_start: u32,
    anchored_end: u32,
    blob_text_memo: &mut HashMap<gix::ObjectId, String>,
) -> bool {
    let prev_text = if previous_id.is_null() {
        String::new()
    } else {
        blob_text_memo
            .entry(*previous_id)
            .or_insert_with(|| git::read_git_text(repo, &previous_id.to_string()).unwrap_or_default())
            .clone()
    };
    let new_text = if id.is_null() {
        String::new()
    } else {
        blob_text_memo
            .entry(*id)
            .or_insert_with(|| git::read_git_text(repo, &id.to_string()).unwrap_or_default())
            .clone()
    };
    let prev_lines: Vec<&str> = prev_text.lines().collect();
    let new_lines: Vec<&str> = new_text.lines().collect();
    let lo = (anchored_start as usize).saturating_sub(1);
    let hi = anchored_end as usize;
    let prev_slice: &[&str] = if lo <= prev_lines.len() {
        &prev_lines[lo..hi.min(prev_lines.len())]
    } else {
        &[]
    };
    let new_slice: &[&str] = if lo <= new_lines.len() {
        &new_lines[lo..hi.min(new_lines.len())]
    } else {
        &[]
    };
    // If the new blob's slice equals the anchored slice, this commit did
    // not mutate the anchored content (e.g. it edited a different range
    // of the file). If the previous slice equals the new slice, the
    // commit changed only lines outside the anchored range.
    let anchored = anchored_slice;
    let new_eq_anchored = new_slice == anchored;
    let prev_eq_new = prev_slice == new_slice;
    !(new_eq_anchored || prev_eq_new)
}

// ── Test helpers ─────────────────────────────────────────────────────────────

/// Thin test-only wrapper exposing `encode_drift_locus` for unit tests in
/// sibling modules that need to assert round-trip correctness.
#[cfg(test)]
pub(crate) fn encode_drift_locus_for_test(locus: Option<&crate::types::DriftLocus>) -> DriftLocusCachedValue {
    encode_drift_locus(locus)
}

/// Thin test-only wrapper exposing `decode_drift_locus` for unit tests in
/// sibling modules that need to assert round-trip correctness.
#[cfg(test)]
pub(crate) fn decode_drift_locus_for_test(cached: &DriftLocusCachedValue) -> Option<crate::types::DriftLocus> {
    decode_drift_locus(cached)
}

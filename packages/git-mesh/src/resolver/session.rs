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
//! "Sharing a single computation across consumers, not storing past
//! results."

use crate::Result;
use crate::git;
use crate::resolver::walker::{self, NS};
use crate::types::{Anchor, CopyDetection};
use std::collections::HashMap;

/// One per-commit slice of the shared walk: `(parent_sha, commit_sha,
/// name_status_entries)`. Entries are produced with rewrite tracking
/// enabled; consumers that want the cheap "no-rewrites" view derive it by
/// projecting `Rename`/`Copied` back to `Added` (the `to`) plus
/// `Deleted` (the `from`). Per phase 3.
pub(crate) struct CommitDelta {
    pub(crate) parent: String,
    pub(crate) commit: String,
    pub(crate) entries: Vec<NS>,
}

/// One grouped walk: the rev list (oldest-first) from `anchor_sha..HEAD`,
/// plus per-commit deltas. Computed exactly once per `(repo,
/// anchor_sha)`.
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
}

/// Engine-wide shared state: one entry per distinct anchor commit.
pub(crate) struct ResolveSession {
    walks: HashMap<(String, CopyDetection), GroupedWalk>,
    pub(crate) ensure_calls: u64,
    pub(crate) ensure_hits: u64,
}

impl ResolveSession {
    pub(crate) fn new() -> Self {
        Self {
            walks: HashMap::new(),
            ensure_calls: 0,
            ensure_hits: 0,
        }
    }

    pub(crate) fn walks_len(&self) -> usize {
        self.walks.len()
    }

    /// Ensure a grouped walk exists for `anchor_sha`. Idempotent. The
    /// `copy_detection` is used the first time a group is built; meshes
    /// share the same copy-detection knob across their anchors so this
    /// is unambiguous within one mesh, and walks are keyed by anchor
    /// commit so different meshes that share a anchor still get their
    /// own group only on first observation. (Greenfield: we don't try
    /// to merge mismatched copy-detection levels — the first wins
    /// because a single mesh-wide level is the authoritative source.)
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
            let walk = build_grouped_walk(repo, anchor_sha, copy_detection, warnings)?;
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

fn build_grouped_walk(
    repo: &gix::Repository,
    anchor_sha: &str,
    copy_detection: CopyDetection,
    warnings: &mut Vec<String>,
) -> Result<GroupedWalk> {
    let head_sha = git::head_oid(repo)?;
    let mut commits =
        git::rev_walk_excluding(repo, &[&head_sha], &[anchor_sha], None).unwrap_or_default();
    commits.reverse(); // oldest-first

    let mut deltas: Vec<CommitDelta> = Vec::with_capacity(commits.len());
    let mut parent = anchor_sha.to_string();
    let prior_warning_count = warnings.len();
    for commit in &commits {
        let entries = walker::name_status(repo, &parent, commit, copy_detection, warnings)?;
        deltas.push(CommitDelta {
            parent: parent.clone(),
            commit: commit.clone(),
            entries,
        });
        parent = commit.clone();
    }
    let renames_disabled = warnings.len() > prior_warning_count;

    Ok(GroupedWalk {
        anchor_sha: anchor_sha.to_string(),
        head_sha,
        commits: deltas,
        renames_disabled,
    })
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
    let group = session.ensure_group(repo, &r.anchor_sha, copy_detection, warnings)?;
    let head_sha = group.head_sha.clone();
    let mut loc = walker::Tracked {
        path: r.path.clone(),
        start: rstart,
        end: rend,
    };
    // Iterate shared per-commit deltas; only the hunk math is per-anchor.
    for delta in &group.commits {
        match walker::advance_with_entries(
            repo,
            &delta.parent,
            &delta.commit,
            &loc,
            &delta.entries,
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

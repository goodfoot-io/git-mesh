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
//! ## Candidate-path filtering
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
use crate::resolver::walker::{self, NS};
use crate::types::{Anchor, CopyDetection};
use std::collections::{HashMap, HashSet};
use std::str::FromStr;

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
    /// Counter: how many commits across all walks were skipped by the
    /// candidate-path filter (i.e. classified non-interesting).
    pub(crate) skipped_commits: u64,
    /// Counter: how many commits across all walks ran the full
    /// rewrite-aware `name_status`.
    pub(crate) interesting_commits: u64,
}

impl ResolveSession {
    pub(crate) fn new() -> Self {
        Self {
            walks: HashMap::new(),
            ensure_calls: 0,
            ensure_hits: 0,
            skipped_commits: 0,
            interesting_commits: 0,
        }
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

fn build_grouped_walk(
    repo: &gix::Repository,
    anchor_sha: &str,
    copy_detection: CopyDetection,
    candidate_paths: Option<&HashSet<String>>,
    warnings: &mut Vec<String>,
    skipped_counter: &mut u64,
    interesting_counter: &mut u64,
) -> Result<GroupedWalk> {
    let head_sha = git::head_oid(repo)?;
    let mut commits =
        git::rev_walk_excluding(repo, &[&head_sha], &[anchor_sha], None).unwrap_or_default();
    commits.reverse(); // oldest-first

    let mut deltas: Vec<CommitDelta> = Vec::with_capacity(commits.len());
    let mut parent = anchor_sha.to_string();
    let prior_warning_count = warnings.len();

    // SameCommit copy detection only pairs an added file with a parent-tree
    // path that was *also modified* in the same commit — so the source
    // already shows up in the cheap-pass changed-path list and the
    // intersection check catches it. AnyFileInCommit / AnyFileInRepo widen
    // copy sources to unmodified files, so any added path could pull in a
    // candidate as its copy source — for those modes we have to widen the
    // interesting-commit trigger to "any added path".
    let copies_widen_to_added = matches!(
        copy_detection,
        CopyDetection::AnyFileInCommit | CopyDetection::AnyFileInRepo
    );
    let mut tracked: Option<HashSet<String>> = candidate_paths.cloned();

    // Pre-compute the blob OID of each candidate path at every commit in
    // the walk (plus the anchor commit itself). Walking each candidate
    // path top-down once across the whole walk and reusing subtree
    // lookups via a per-tree_oid cache keeps the cheap-pass cost down to
    // a handful of ms per walk in practice — the overhead per *skipped*
    // commit is dominated by `repo.find_object` (small) rather than a
    // full tree traversal per candidate per commit.
    //
    // Only useful when copy detection isn't widening every added-path
    // commit into "interesting"; otherwise the cheap pass needs the
    // no-rewrites tree-diff anyway and these blob lookups would be wasted.
    let cheap_blobs: HashMap<(String, String), Option<String>> =
        if !copies_widen_to_added && let Some(set) = tracked.as_ref() {
            let mut all_commits: Vec<&str> = Vec::with_capacity(commits.len() + 1);
            all_commits.push(anchor_sha);
            for c in &commits {
                all_commits.push(c);
            }
            precompute_candidate_blobs(repo, &all_commits, set)
        } else {
            HashMap::new()
        };

    for commit in &commits {
        let interesting = match tracked.as_ref() {
            None => true, // no filter → keep every commit
            Some(set) if copies_widen_to_added => {
                // Copy detection can pull in candidates from unmodified
                // sources, so a commit with any added path is potentially
                // interesting. Need the no-rewrites tree-diff to know.
                let (paths, has_added) =
                    walker::changed_paths_no_rewrites(repo, &parent, commit)?;
                has_added || paths.iter().any(|p| set.contains(p))
            }
            Some(set) => {
                // SameCommit / Off: a commit can only affect a candidate
                // if it changes the candidate's blob OID at the candidate
                // path (deletion / modification / rename source). The
                // pre-computed `cheap_blobs` map answers each lookup in
                // O(1) for the seed candidate set; for paths added to
                // the set mid-walk (rename targets discovered by an
                // earlier interesting commit) we fall back to a live
                // `path_blob_at` lookup so renamed paths still get the
                // correct interesting-commit classification.
                let mut touches = false;
                for p in set {
                    let pkey = (parent.clone(), p.clone());
                    let ckey = (commit.clone(), p.clone());
                    let p_blob = match cheap_blobs.get(&pkey) {
                        Some(v) => v.clone(),
                        None => git::path_blob_at(repo, &parent, p).ok(),
                    };
                    let c_blob = match cheap_blobs.get(&ckey) {
                        Some(v) => v.clone(),
                        None => git::path_blob_at(repo, commit, p).ok(),
                    };
                    if p_blob != c_blob {
                        touches = true;
                        break;
                    }
                }
                touches
            }
        };
        let entries = if interesting {
            *interesting_counter += 1;
            let entries = walker::name_status(repo, &parent, commit, copy_detection, warnings)?;
            // Update candidate set with discovered renames/copies so future
            // commits that touch the new path are also marked interesting.
            if let Some(set) = tracked.as_mut() {
                for e in &entries {
                    match e {
                        NS::Renamed { from, to } | NS::Copied { from, to }
                            if set.contains(from) =>
                        {
                            set.insert(to.clone());
                        }
                        _ => {}
                    }
                }
            }
            entries
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
    let renames_disabled = warnings.len() > prior_warning_count;

    Ok(GroupedWalk {
        anchor_sha: anchor_sha.to_string(),
        head_sha,
        commits: deltas,
        renames_disabled,
    })
}

/// Pre-compute `(commit_oid, candidate_path) -> Option<blob_oid>` for
/// every commit in the walk in one pass. Two layers of memoization keep
/// this fast even when adjacent commits in `anchor_sha..HEAD` share most
/// of their tree:
///
/// - `commit_oid -> tree_oid` (cheap: just reads the commit header).
/// - `(tree_oid, path) -> Option<blob_oid>` (skipped when an earlier
///   commit with the same `tree_oid` already resolved this path).
///
/// Failures (bad OIDs, missing commits / trees) silently record `None`
/// so the caller's `parent != commit` blob comparison classifies the
/// commit as interesting (fail-closed: when in doubt, run the full
/// rewrite-aware pass).
fn precompute_candidate_blobs(
    repo: &gix::Repository,
    commits: &[&str],
    tracked: &HashSet<String>,
) -> HashMap<(String, String), Option<String>> {
    let mut tree_path_cache: HashMap<(String, String), Option<String>> = HashMap::new();
    let mut commit_to_tree: HashMap<String, Option<String>> = HashMap::new();
    let mut out: HashMap<(String, String), Option<String>> =
        HashMap::with_capacity(commits.len() * tracked.len());

    for commit in commits {
        let tree_oid = match commit_to_tree.get(*commit) {
            Some(v) => v.clone(),
            None => {
                let v = (|| -> Option<String> {
                    let oid = gix::ObjectId::from_str(commit).ok()?;
                    let commit_obj = repo.find_commit(oid).ok()?;
                    commit_obj.tree_id().ok().map(|id| id.detach().to_string())
                })();
                commit_to_tree.insert((*commit).to_string(), v.clone());
                v
            }
        };

        let Some(tree_oid) = tree_oid else {
            // Couldn't resolve the tree — record `None` for every
            // candidate so the comparison flags this commit as
            // interesting and the full rewrite pass takes over.
            for path in tracked {
                out.insert(((*commit).to_string(), path.clone()), None);
            }
            continue;
        };

        // Resolve any candidate paths not yet in `tree_path_cache` by
        // walking this tree once. Subsequent commits with the same
        // tree_oid hit the cache without any object I/O.
        let need_lookup: Vec<&String> = tracked
            .iter()
            .filter(|p| !tree_path_cache.contains_key(&(tree_oid.clone(), (*p).clone())))
            .collect();

        if !need_lookup.is_empty() {
            let tree = (|| -> Option<gix::Tree<'_>> {
                let id = gix::ObjectId::from_str(&tree_oid).ok()?;
                let obj = repo.find_object(id).ok()?;
                obj.peel_to_tree().ok()
            })();
            for path in need_lookup {
                let blob = match tree.as_ref() {
                    None => None,
                    Some(tree) => {
                        let mut tree = tree.clone();
                        tree.peel_to_entry_by_path(std::path::Path::new(path))
                            .ok()
                            .flatten()
                            .map(|e| e.object_id().to_string())
                    }
                };
                tree_path_cache.insert((tree_oid.clone(), path.clone()), blob);
            }
        }

        for path in tracked {
            let blob = tree_path_cache
                .get(&(tree_oid.clone(), path.clone()))
                .cloned()
                .unwrap_or(None);
            out.insert(((*commit).to_string(), path.clone()), blob);
        }
    }

    out
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
        let mut session = ResolveSession::new();
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
        let mut session = ResolveSession::new();
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
    /// makes its commit interesting (because we conservatively mark
    /// every commit with an added path as interesting when copy
    /// detection is on).
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
        let mut session = ResolveSession::new();
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
        // The "noise" commit added a file — copy detection forces it
        // interesting. The "copy" commit also added a file. So both are
        // interesting. (Over-inclusion is allowed; correctness > speed.)
        assert_eq!(
            session.interesting_commits, 2,
            "both add commits run rewrites under AnyFileInCommit"
        );
        assert_eq!(session.skipped_commits, 0);
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
        let mut session = ResolveSession::new();
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

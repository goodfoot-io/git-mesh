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
use crate::resolver::walker::{self, NS};
use crate::types::{Anchor, CopyDetection};
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
}

impl ResolveSession {
    pub(crate) fn new() -> Self {
        Self {
            walks: HashMap::new(),
            ensure_calls: 0,
            ensure_hits: 0,
            skipped_commits: 0,
            interesting_commits: 0,
            pass1_ms: 0,
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
            &mut self.pass1_ms,
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
) -> Result<GroupedWalk> {
    let head_sha = git::head_oid(repo)?;
    let mut commits =
        git::rev_walk_excluding(repo, &[&head_sha], &[anchor_sha], None).unwrap_or_default();
    commits.reverse(); // oldest-first

    // Walk-order parent map: each commit's "parent" for rename detection
    // is the commit immediately preceding it in the reversed `rev_walk`
    // output, with the anchor as the parent of the first commit.
    // `compute_rename_trail` consumes this same map so Pass 1's per-commit
    // gix `name_status` always diffs against the same baseline that Pass
    // 2's `build_grouped_walk` loop uses — important on merge commits,
    // where first-parent and walk-order can disagree.
    let mut walk_parents: HashMap<String, String> = HashMap::with_capacity(commits.len());
    {
        let mut prev = anchor_sha.to_string();
        for commit in &commits {
            walk_parents.insert(commit.clone(), prev.clone());
            prev = commit.clone();
        }
    }

    // Pass 1: rename-trail closure. Caller-supplied seed + iterative
    // `git log --name-status` queries until the path set stops growing.
    // When no seed is provided (single-anchor / test fallback) the gate
    // is bypassed and every commit is treated as interesting.
    let (closed_paths, interesting_set, pass1_fell_back) = match candidate_paths {
        Some(seed) => {
            let t0 = std::time::Instant::now();
            let (closed, interesting, fell_back) =
                compute_rename_trail(repo, anchor_sha, seed, copy_detection, &walk_parents, warnings)?;
            *pass1_ms_counter += t0.elapsed().as_millis() as u64;
            // On fallback, `closed` is just the seed (or a partial set) —
            // not the authoritative trail closure the field's contract
            // promises. Drop it so future cache consumers (Route C) can
            // distinguish "trail closed" from "trail unknown" without
            // mistaking a seed-only result for a real closure.
            let cached_closed = if fell_back { None } else { Some(closed) };
            (cached_closed, Some(interesting), fell_back)
        }
        None => (None, None, false),
    };

    let mut deltas: Vec<CommitDelta> = Vec::with_capacity(commits.len());
    let mut parent = anchor_sha.to_string();
    let prior_warning_count = warnings.len();

    for commit in &commits {
        let interesting = match interesting_set.as_ref() {
            // No seed → no filter → keep every commit.
            None => true,
            // Pass 1 hit the rename budget → fall back to "every commit
            // interesting" rather than silently shrink the trail.
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

    Ok(GroupedWalk {
        anchor_sha: anchor_sha.to_string(),
        head_sha,
        commits: deltas,
        renames_disabled,
        closed_paths,
    })
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
        let mut session = ResolveSession::new();
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
        let mut session = ResolveSession::new();
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

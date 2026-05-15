//! Phase 1: `PathTimeline` — path-history abstraction for committed HEAD
//! projection.
//!
//! Re-routes per-anchor delta replay through a structured, allocation-light
//! representation keyed by `(path, head_blob_oid, copy_detection, anchor_sha)`.
//! The current timeline is built from one anchor's reverse-walk delta slice, so
//! the key includes anchor identity to keep replay windows separate.
//!
//! Phase 1 is a parity-preserving structural refactor: `project_by_hunk_replay`
//! produces the same `Tracked` location as the previous per-anchor delta
//! replay in `session::resolve_at_head_shared`.
//!
//! Phase 2 will replace per-anchor replay with a composed line map. Phase 1
//! does *not* attempt that — see `three-phase-plan.md`.

use crate::git;
use crate::perf;
use crate::resolver::linemap::LineMap;
use crate::resolver::session::CommitDelta;
use crate::resolver::walker::{Tracked, apply_hunks_to_range, compute_hunks};
use crate::types::CopyDetection;
use crate::{Error, Result};
use std::collections::HashMap;
use std::str::FromStr;
use std::sync::{Arc, OnceLock};

/// One diff hunk: `(old_start, old_count, new_start, new_count)`.
pub(crate) type Hunk = (u32, u32, u32, u32);

/// Cache key for a `PathTimeline`.
///
/// Identifies one anchor-scoped path history. `build_timeline` consumes a
/// single anchor's delta slice, so two anchors with the same path and HEAD blob
/// still need distinct entries when their `anchor_sha` values differ.
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub(crate) struct PathTimelineKey {
    pub(crate) path: Arc<[u8]>,
    pub(crate) head_blob_oid: Option<gix::ObjectId>,
    pub(crate) copy_detection: CopyDetection,
    pub(crate) anchor_sha: String,
}

/// One step in a `PathTimeline`. Pre-computes the hunks against the parent
/// blob so projection is hunk replay only.
#[derive(Clone, Debug)]
pub(crate) struct PathDelta {
    pub(crate) parent: gix::ObjectId,
    pub(crate) commit: gix::ObjectId,
    pub(crate) from_path: Arc<[u8]>,
    pub(crate) to_path: Arc<[u8]>,
    pub(crate) old_blob: Option<gix::ObjectId>,
    pub(crate) new_blob: Option<gix::ObjectId>,
    pub(crate) hunks: Arc<[Hunk]>,
    /// `true` when the path is deleted at this commit and never reintroduced
    /// by a rename pair in the same commit. Projection returns `None` on
    /// the first `deleted` delta.
    pub(crate) deleted: bool,
    /// Old-side line count (the parent blob's line count). Used by the
    /// Phase 2 `LineMap` builder.
    pub(crate) old_line_count: u32,
    /// New-side line count (the commit blob's line count). Used by the
    /// Phase 2 `LineMap` builder.
    pub(crate) new_line_count: u32,
}

/// Path history for one `(start_path, head_blob_oid, copy_detection)` triple.
#[derive(Clone, Debug)]
pub(crate) struct PathTimeline {
    pub(crate) start_path: Arc<[u8]>,
    pub(crate) final_path: Option<Arc<[u8]>>,
    pub(crate) deltas: Vec<PathDelta>,
    /// Composed `LineMap` across all non-deleted deltas. Built lazily
    /// the first time `project_by_linemap` is invoked.
    pub(crate) composed_linemap: OnceLock<LineMap>,
}

impl PathTimeline {
    /// Project `(start, end)` from `start_path` through the timeline to
    /// the final path. Returns `None` when the path is deleted along the
    /// way (parity with `walker::Change::Deleted`).
    /// Phase 2: project via the composed `LineMap`. Falls back to the
    /// Phase 1 `project_by_hunk_replay` when the linemap result
    /// disagrees with the hunk-replay result; mismatches are recorded
    /// as `linemap.project-fallbacks`.
    ///
    /// Returns `None` only when the path is deleted along the way (or
    /// the fallback decides so).
    pub(crate) fn project_by_linemap(&self, start: u32, end: u32) -> Option<Tracked> {
        // Any deleted delta terminates projection.
        if self.deltas.iter().any(|d| d.deleted) {
            // Match Phase 1 `Deleted` semantics.
            return self.project_by_hunk_replay(start, end);
        }

        let composed = self.composed_linemap.get_or_init(|| {
            let mut acc = LineMap::empty();
            for d in &self.deltas {
                if d.deleted {
                    continue;
                }
                let m = LineMap::from_hunks(&d.hunks, d.old_line_count, d.new_line_count);
                acc = LineMap::compose(&acc, &m);
            }
            acc
        });

        let (lm_s, lm_e) = composed.project_range(start, end)?;

        // Path always tracks `final_path` (the linemap doesn't model
        // path identity — only line coordinates).
        let final_path_arc = match &self.final_path {
            Some(p) => Arc::clone(p),
            None => return self.project_by_hunk_replay(start, end),
        };

        // Cross-check against hunk replay. On mismatch, fall back and
        // record a counter — keeps Phase 1 parity airtight.
        let replay = self.project_by_hunk_replay(start, end);
        match &replay {
            Some(t) => {
                if t.start != lm_s || t.end != lm_e {
                    crate::resolver::linemap::record_fallback();
                    return replay;
                }
            }
            None => {
                crate::resolver::linemap::record_fallback();
                return None;
            }
        }

        Some(Tracked {
            path: bytes_to_string(&final_path_arc),
            start: lm_s,
            end: lm_e,
        })
    }

    pub(crate) fn project_by_hunk_replay(&self, start: u32, end: u32) -> Option<Tracked> {
        let _span = perf::span("timeline.project-range");
        let t0 = std::time::Instant::now();
        let mut loc = Tracked {
            path: bytes_to_string(&self.start_path),
            start,
            end,
        };
        for delta in &self.deltas {
            if delta.deleted {
                record_project_us(t0);
                return None;
            }
            let (s, e) = apply_hunks_to_range(&delta.hunks, loc.start, loc.end);
            loc = Tracked {
                path: bytes_to_string(&delta.to_path),
                start: s,
                end: e,
            };
        }
        record_project_us(t0);
        Some(loc)
    }
}

fn bytes_to_string(bytes: &Arc<[u8]>) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn record_project_us(t0: std::time::Instant) {
    PROJECT_RANGE_US.fetch_add(
        t0.elapsed().as_micros() as u64,
        std::sync::atomic::Ordering::Relaxed,
    );
}

// ── Counters ────────────────────────────────────────────────────────────────
//
// Phase 1 counters live as process-global atomics (mirroring `perf.rs`),
// because `PathTimeline::build_from_deltas` does not have the resolver
// session at hand for every call site that may want to use it. They are
// emitted alongside the existing resolver counters from
// `engine::stale_meshes`.

use std::sync::atomic::{AtomicU64, Ordering};

static PATHS_BUILT: AtomicU64 = AtomicU64::new(0);
static DELTAS_BUILT: AtomicU64 = AtomicU64::new(0);
static PROJECT_RANGE_US: AtomicU64 = AtomicU64::new(0);
static PATH_BYTES_SHARED: AtomicU64 = AtomicU64::new(0);

pub(crate) fn reset_counters() {
    PATHS_BUILT.store(0, Ordering::Relaxed);
    DELTAS_BUILT.store(0, Ordering::Relaxed);
    PROJECT_RANGE_US.store(0, Ordering::Relaxed);
    PATH_BYTES_SHARED.store(0, Ordering::Relaxed);
}

pub(crate) fn paths_built() -> u64 {
    PATHS_BUILT.load(Ordering::Relaxed)
}
pub(crate) fn deltas_built() -> u64 {
    DELTAS_BUILT.load(Ordering::Relaxed)
}
pub(crate) fn project_range_us() -> u64 {
    PROJECT_RANGE_US.load(Ordering::Relaxed)
}
pub(crate) fn path_bytes_shared() -> u64 {
    PATH_BYTES_SHARED.load(Ordering::Relaxed)
}

/// Emit Phase 1 perf counters. Called from `engine::stale_meshes` after
/// resolution finishes.
pub(crate) fn emit_counters() {
    perf::counter("timeline.paths-built", paths_built());
    perf::counter("timeline.deltas-built", deltas_built());
    perf::counter("timeline.project-range-us", project_range_us());
    perf::counter("timeline.path-bytes-shared", path_bytes_shared());
}

// ── Construction ────────────────────────────────────────────────────────────

/// Path-interner: when the same path appears in multiple deltas (and
/// timelines), share one `Arc<[u8]>` allocation. Tracked as
/// `timeline.path-bytes-shared`.
pub(crate) struct PathInterner {
    map: HashMap<Vec<u8>, Arc<[u8]>>,
}

impl PathInterner {
    pub(crate) fn new() -> Self {
        Self {
            map: HashMap::new(),
        }
    }

    pub(crate) fn intern(&mut self, bytes: &[u8]) -> Arc<[u8]> {
        if let Some(existing) = self.map.get(bytes) {
            PATH_BYTES_SHARED.fetch_add(bytes.len() as u64, Ordering::Relaxed);
            return Arc::clone(existing);
        }
        let arc: Arc<[u8]> = Arc::from(bytes);
        self.map.insert(bytes.to_vec(), Arc::clone(&arc));
        arc
    }
}

/// Build a `PathTimeline` from a per-anchor `Vec<Arc<CommitDelta>>` (the
/// output of the reverse-indexed walk, oldest-first).
///
/// Parity with `session::resolve_at_head_shared`: at each commit, we
/// inspect the name-status entries against the *current* tracked path
/// (which may have been renamed by an earlier delta). If the path is
/// modified or moved by this commit, we emit one `PathDelta` with hunks
/// computed against the parent blob. On a pure deletion (no rename pair
/// in the same commit), we emit a `deleted` delta and stop.
pub(crate) fn build_timeline(
    repo: &gix::Repository,
    start_path: &[u8],
    deltas: &[Arc<CommitDelta>],
    head_blob_oid: Option<gix::ObjectId>,
    copy_detection: CopyDetection,
    interner: &mut PathInterner,
    blob_oid_memo: &mut HashMap<(String, String), Option<String>>,
) -> Result<PathTimeline> {
    let _span = perf::span("timeline.build");
    let start_path_arc = interner.intern(start_path);
    let mut current_path: Arc<[u8]> = Arc::clone(&start_path_arc);
    let mut out: Vec<PathDelta> = Vec::new();
    let mut deleted_terminal = false;

    for delta in deltas {
        // Replicate `walker::advance_with_entries` semantics, but produce a
        // `PathDelta` instead of an in-place `Tracked` update.
        use crate::resolver::walker::NS;
        let cur_path_str = String::from_utf8_lossy(&current_path).into_owned();
        let mut next_path: Option<String> = None;
        let mut deleted = false;
        let mut modified = false;
        for e in &delta.entries {
            match e {
                NS::Added { path } | NS::Modified { path } => {
                    if path == &cur_path_str {
                        modified = true;
                        next_path = Some(cur_path_str.clone());
                    }
                }
                NS::Deleted { path } => {
                    if path == &cur_path_str {
                        deleted = true;
                    }
                }
                NS::Renamed { from, to } => {
                    if from == &cur_path_str {
                        next_path = Some(to.clone());
                        modified = true;
                        deleted = false;
                    }
                }
                NS::Copied { from, to } => {
                    if from == &cur_path_str {
                        next_path = Some(to.clone());
                        modified = true;
                    }
                }
            }
        }

        if deleted && next_path.is_none() {
            // Pure deletion: emit a terminal `deleted` delta.
            let parent_oid = parse_oid(&delta.parent)?;
            let commit_oid = parse_oid(&delta.commit)?;
            let from_arc = Arc::clone(&current_path);
            let to_arc = Arc::clone(&current_path);
            out.push(PathDelta {
                parent: parent_oid,
                commit: commit_oid,
                from_path: from_arc,
                to_path: to_arc,
                old_blob: None,
                new_blob: None,
                hunks: Arc::from(Vec::<Hunk>::new()),
                deleted: true,
                old_line_count: 0,
                new_line_count: 0,
            });
            DELTAS_BUILT.fetch_add(1, Ordering::Relaxed);
            deleted_terminal = true;
            break;
        }

        if !modified {
            continue;
        }

        let new_path_str = next_path.unwrap_or_else(|| cur_path_str.clone());
        let new_path_arc = interner.intern(new_path_str.as_bytes());

        let parent_sha = &delta.parent;
        let commit_sha = &delta.commit;

        let old_blob_oid =
            blob_oid_at(repo, parent_sha, &cur_path_str, Some(blob_oid_memo));
        let new_blob_oid =
            blob_oid_at(repo, commit_sha, &new_path_str, Some(blob_oid_memo));

        let old_text = old_blob_oid
            .as_deref()
            .and_then(|b| git::read_git_text(repo, b).ok())
            .unwrap_or_default();
        let new_text = new_blob_oid
            .as_deref()
            .and_then(|b| git::read_git_text(repo, b).ok())
            .unwrap_or_default();
        let old_line_count = old_text.lines().count() as u32;
        let new_line_count = new_text.lines().count() as u32;
        let hunks_vec = compute_hunks(&old_text, &new_text);
        let hunks: Arc<[Hunk]> = Arc::from(hunks_vec);

        let parent_oid = parse_oid(parent_sha)?;
        let commit_oid = parse_oid(commit_sha)?;
        let old_blob_id = old_blob_oid.as_deref().and_then(|s| parse_oid(s).ok());
        let new_blob_id = new_blob_oid.as_deref().and_then(|s| parse_oid(s).ok());

        out.push(PathDelta {
            parent: parent_oid,
            commit: commit_oid,
            from_path: Arc::clone(&current_path),
            to_path: Arc::clone(&new_path_arc),
            old_blob: old_blob_id,
            new_blob: new_blob_id,
            hunks,
            deleted: false,
            old_line_count,
            new_line_count,
        });
        DELTAS_BUILT.fetch_add(1, Ordering::Relaxed);

        current_path = new_path_arc;
    }

    PATHS_BUILT.fetch_add(1, Ordering::Relaxed);

    let final_path = if deleted_terminal {
        None
    } else {
        Some(Arc::clone(&current_path))
    };

    // `head_blob_oid` and `copy_detection` are part of the cache key, not
    // the timeline content itself. Stored on the key by the caller.
    let _ = head_blob_oid;
    let _ = copy_detection;

    Ok(PathTimeline {
        start_path: start_path_arc,
        final_path,
        deltas: out,
        composed_linemap: OnceLock::new(),
    })
}

fn parse_oid(s: &str) -> Result<gix::ObjectId> {
    gix::ObjectId::from_str(s).map_err(|e| Error::Git(format!("parse oid `{s}`: {e}")))
}

fn blob_oid_at(
    repo: &gix::Repository,
    commit: &str,
    path: &str,
    memo: Option<&mut HashMap<(String, String), Option<String>>>,
) -> Option<String> {
    let key = (commit.to_string(), path.to_string());
    if let Some(m) = memo {
        if let Some(cached) = m.get(&key) {
            return cached.clone();
        }
        let oid = git::path_blob_at(repo, commit, path).ok();
        m.insert(key, oid.clone());
        oid
    } else {
        git::path_blob_at(repo, commit, path).ok()
    }
}

// ── Tests: parity vs current replay ────────────────────────────────────────
//
// These tests build small repositories with known histories and verify
// that `PathTimeline::project_by_hunk_replay` returns exactly the same
// `Tracked` as the pre-Phase-1 path: per-anchor calls into
// `walker::advance_with_entries`.

#[cfg(test)]
mod parity_tests {
    use super::*;
    use crate::resolver::session::CommitDelta;
    use crate::resolver::walker::{self, Change, Tracked};
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

    fn init_repo(dir: &std::path::Path) {
        run_git(dir, &["init", "--initial-branch=main"]);
        run_git(dir, &["config", "user.email", "t@t"]);
        run_git(dir, &["config", "user.name", "t"]);
        run_git(dir, &["config", "commit.gpgsign", "false"]);
    }

    fn rev_parse(dir: &std::path::Path, rev: &str) -> String {
        String::from_utf8(
            Command::new("git")
                .current_dir(dir)
                .args(["rev-parse", rev])
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap()
        .trim()
        .to_string()
    }

    /// Build the per-anchor deltas the same way the reverse-indexed walk
    /// does: enumerate parent..HEAD commits, run `name_status` for each.
    fn collect_deltas(
        repo: &gix::Repository,
        from_sha: &str,
        to_sha: &str,
        copy_detection: CopyDetection,
    ) -> Vec<Arc<CommitDelta>> {
        let from_oid = gix::ObjectId::from_str(from_sha).unwrap();
        let to_oid = gix::ObjectId::from_str(to_sha).unwrap();
        let walk = repo
            .rev_walk([to_oid])
            .sorting(gix::revision::walk::Sorting::BreadthFirst)
            .all()
            .unwrap();
        let mut commits: Vec<gix::ObjectId> = Vec::new();
        for info in walk {
            let info = info.unwrap();
            if info.id == from_oid {
                break;
            }
            commits.push(info.id);
        }
        commits.reverse();
        let mut out = Vec::new();
        let mut warnings = Vec::new();
        for c in commits {
            let commit_obj = repo.find_commit(c).unwrap();
            let parent = match commit_obj.parent_ids().next() {
                Some(p) => p.detach(),
                None => continue,
            };
            let entries = walker::name_status(
                repo,
                &parent.to_string(),
                &c.to_string(),
                copy_detection,
                &mut warnings,
            )
            .unwrap();
            out.push(Arc::new(CommitDelta {
                parent: parent.to_string(),
                commit: c.to_string(),
                entries,
            }));
        }
        out
    }

    /// Replay deltas the old way (per-anchor advance_with_entries) so we
    /// have a ground truth to compare against.
    fn replay_old(
        repo: &gix::Repository,
        deltas: &[Arc<CommitDelta>],
        start_path: &str,
        start: u32,
        end: u32,
    ) -> Option<Tracked> {
        let mut loc = Tracked {
            path: start_path.to_string(),
            start,
            end,
        };
        for d in deltas {
            match walker::advance_with_entries(repo, &d.parent, &d.commit, &loc, &d.entries, None)
                .unwrap()
            {
                Change::Unchanged => {}
                Change::Deleted => return None,
                Change::Updated(t) => loc = t,
            }
        }
        Some(loc)
    }

    fn project_new(
        repo: &gix::Repository,
        deltas: &[Arc<CommitDelta>],
        start_path: &str,
        start: u32,
        end: u32,
        copy_detection: CopyDetection,
    ) -> Option<Tracked> {
        let mut interner = PathInterner::new();
        let mut memo = HashMap::new();
        let tl = build_timeline(
            repo,
            start_path.as_bytes(),
            deltas,
            None,
            copy_detection,
            &mut interner,
            &mut memo,
        )
        .unwrap();
        tl.project_by_hunk_replay(start, end)
    }

    fn write_lines(p: &std::path::Path, lines: &[&str]) {
        let mut s = String::new();
        for l in lines {
            s.push_str(l);
            s.push('\n');
        }
        std::fs::write(p, s).unwrap();
    }

    fn assert_parity(
        repo: &gix::Repository,
        deltas: &[Arc<CommitDelta>],
        start_path: &str,
        start: u32,
        end: u32,
        cd: CopyDetection,
    ) {
        let old = replay_old(repo, deltas, start_path, start, end);
        let new = project_new(repo, deltas, start_path, start, end, cd);
        match (&old, &new) {
            (None, None) => {}
            (Some(a), Some(b)) => {
                assert_eq!(a.path, b.path, "path parity");
                assert_eq!(a.start, b.start, "start parity");
                assert_eq!(a.end, b.end, "end parity");
            }
            other => panic!("parity mismatch: old={:?} new={:?}", other.0, other.1),
        }
    }

    #[test]
    fn parity_insertion_before_range() {
        let td = tempdir().unwrap();
        let dir = td.path();
        init_repo(dir);
        // Initial: 10 lines, anchor on lines 5..7.
        let initial: Vec<String> = (1..=10).map(|i| format!("L{i}")).collect();
        let initial_refs: Vec<&str> = initial.iter().map(|s| s.as_str()).collect();
        write_lines(&dir.join("f.txt"), &initial_refs);
        run_git(dir, &["add", "."]);
        run_git(dir, &["commit", "-m", "init"]);
        let anchor_sha = rev_parse(dir, "HEAD");

        // Insert 3 lines at top.
        let mut next: Vec<String> = vec!["X".into(), "Y".into(), "Z".into()];
        next.extend(initial.iter().cloned());
        let next_refs: Vec<&str> = next.iter().map(|s| s.as_str()).collect();
        write_lines(&dir.join("f.txt"), &next_refs);
        run_git(dir, &["add", "."]);
        run_git(dir, &["commit", "-m", "insert top"]);
        let head_sha = rev_parse(dir, "HEAD");

        let repo = gix::open(dir).unwrap();
        let deltas = collect_deltas(&repo, &anchor_sha, &head_sha, CopyDetection::Off);
        assert_parity(&repo, &deltas, "f.txt", 5, 7, CopyDetection::Off);
    }

    #[test]
    fn parity_deletion_before_range() {
        let td = tempdir().unwrap();
        let dir = td.path();
        init_repo(dir);
        let initial: Vec<String> = (1..=10).map(|i| format!("L{i}")).collect();
        let initial_refs: Vec<&str> = initial.iter().map(|s| s.as_str()).collect();
        write_lines(&dir.join("f.txt"), &initial_refs);
        run_git(dir, &["add", "."]);
        run_git(dir, &["commit", "-m", "init"]);
        let anchor_sha = rev_parse(dir, "HEAD");

        // Delete lines 1..=2.
        let next: Vec<String> = initial.iter().skip(2).cloned().collect();
        let next_refs: Vec<&str> = next.iter().map(|s| s.as_str()).collect();
        write_lines(&dir.join("f.txt"), &next_refs);
        run_git(dir, &["add", "."]);
        run_git(dir, &["commit", "-m", "delete top"]);
        let head_sha = rev_parse(dir, "HEAD");

        let repo = gix::open(dir).unwrap();
        let deltas = collect_deltas(&repo, &anchor_sha, &head_sha, CopyDetection::Off);
        assert_parity(&repo, &deltas, "f.txt", 6, 8, CopyDetection::Off);
    }

    #[test]
    fn parity_deletion_inside_range() {
        let td = tempdir().unwrap();
        let dir = td.path();
        init_repo(dir);
        let initial: Vec<String> = (1..=10).map(|i| format!("L{i}")).collect();
        let initial_refs: Vec<&str> = initial.iter().map(|s| s.as_str()).collect();
        write_lines(&dir.join("f.txt"), &initial_refs);
        run_git(dir, &["add", "."]);
        run_git(dir, &["commit", "-m", "init"]);
        let anchor_sha = rev_parse(dir, "HEAD");

        // Delete line 6 (inside range 5..7).
        let next: Vec<String> = initial
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != 5)
            .map(|(_, s)| s.clone())
            .collect();
        let next_refs: Vec<&str> = next.iter().map(|s| s.as_str()).collect();
        write_lines(&dir.join("f.txt"), &next_refs);
        run_git(dir, &["add", "."]);
        run_git(dir, &["commit", "-m", "delete inside"]);
        let head_sha = rev_parse(dir, "HEAD");

        let repo = gix::open(dir).unwrap();
        let deltas = collect_deltas(&repo, &anchor_sha, &head_sha, CopyDetection::Off);
        assert_parity(&repo, &deltas, "f.txt", 5, 7, CopyDetection::Off);
    }

    #[test]
    fn parity_replacement_overlapping_boundary() {
        let td = tempdir().unwrap();
        let dir = td.path();
        init_repo(dir);
        let initial: Vec<String> = (1..=10).map(|i| format!("L{i}")).collect();
        let initial_refs: Vec<&str> = initial.iter().map(|s| s.as_str()).collect();
        write_lines(&dir.join("f.txt"), &initial_refs);
        run_git(dir, &["add", "."]);
        run_git(dir, &["commit", "-m", "init"]);
        let anchor_sha = rev_parse(dir, "HEAD");

        // Replace lines 4..=6 with two new lines (overlapping range 5..7).
        let mut next: Vec<String> = initial[..3].to_vec();
        next.push("A".into());
        next.push("B".into());
        next.extend(initial[6..].iter().cloned());
        let next_refs: Vec<&str> = next.iter().map(|s| s.as_str()).collect();
        write_lines(&dir.join("f.txt"), &next_refs);
        run_git(dir, &["add", "."]);
        run_git(dir, &["commit", "-m", "replace overlap"]);
        let head_sha = rev_parse(dir, "HEAD");

        let repo = gix::open(dir).unwrap();
        let deltas = collect_deltas(&repo, &anchor_sha, &head_sha, CopyDetection::Off);
        assert_parity(&repo, &deltas, "f.txt", 5, 7, CopyDetection::Off);
    }

    #[test]
    fn parity_rename() {
        let td = tempdir().unwrap();
        let dir = td.path();
        init_repo(dir);
        let initial: Vec<String> = (1..=10).map(|i| format!("L{i}")).collect();
        let initial_refs: Vec<&str> = initial.iter().map(|s| s.as_str()).collect();
        write_lines(&dir.join("a.txt"), &initial_refs);
        run_git(dir, &["add", "."]);
        run_git(dir, &["commit", "-m", "init"]);
        let anchor_sha = rev_parse(dir, "HEAD");

        run_git(dir, &["mv", "a.txt", "b.txt"]);
        run_git(dir, &["commit", "-m", "rename"]);
        let head_sha = rev_parse(dir, "HEAD");

        let repo = gix::open(dir).unwrap();
        let deltas = collect_deltas(&repo, &anchor_sha, &head_sha, CopyDetection::SameCommit);
        assert_parity(&repo, &deltas, "a.txt", 5, 7, CopyDetection::SameCommit);
    }

    #[test]
    fn parity_copy() {
        let td = tempdir().unwrap();
        let dir = td.path();
        init_repo(dir);
        let content: Vec<String> = (1..=20).map(|i| format!("content_line_{i}")).collect();
        let content_refs: Vec<&str> = content.iter().map(|s| s.as_str()).collect();
        write_lines(&dir.join("a.ts"), &content_refs);
        run_git(dir, &["add", "."]);
        run_git(dir, &["commit", "-m", "init"]);
        let anchor_sha = rev_parse(dir, "HEAD");

        // Copy a.ts -> b.ts; a.ts itself is unchanged.
        write_lines(&dir.join("b.ts"), &content_refs);
        run_git(dir, &["add", "."]);
        run_git(dir, &["commit", "-m", "copy"]);
        let head_sha = rev_parse(dir, "HEAD");

        let repo = gix::open(dir).unwrap();
        let deltas = collect_deltas(&repo, &anchor_sha, &head_sha, CopyDetection::AnyFileInCommit);
        // Old replay: a.ts is unchanged; new replay: copy entry triggers
        // a modification because `from == cur_path`. Both should agree.
        assert_parity(&repo, &deltas, "a.ts", 5, 7, CopyDetection::AnyFileInCommit);
    }

    #[test]
    fn parity_path_deletion() {
        let td = tempdir().unwrap();
        let dir = td.path();
        init_repo(dir);
        let initial: Vec<String> = (1..=10).map(|i| format!("L{i}")).collect();
        let initial_refs: Vec<&str> = initial.iter().map(|s| s.as_str()).collect();
        write_lines(&dir.join("f.txt"), &initial_refs);
        run_git(dir, &["add", "."]);
        run_git(dir, &["commit", "-m", "init"]);
        let anchor_sha = rev_parse(dir, "HEAD");

        std::fs::remove_file(dir.join("f.txt")).unwrap();
        run_git(dir, &["add", "-A"]);
        run_git(dir, &["commit", "-m", "delete file"]);
        let head_sha = rev_parse(dir, "HEAD");

        let repo = gix::open(dir).unwrap();
        let deltas = collect_deltas(&repo, &anchor_sha, &head_sha, CopyDetection::Off);
        assert_parity(&repo, &deltas, "f.txt", 5, 7, CopyDetection::Off);
    }
}

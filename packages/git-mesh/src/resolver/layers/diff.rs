//! Per-layer index/worktree diffs into `LayerDiffs`.
//!
//! Slice 8 of the gix migration replaces the previous
//! `git diff-{index,files} -U0 -M --full-index` subprocess with two gix
//! pipelines:
//!
//! * Index layer: `repo.tree_index_status(HEAD^{tree}, index, ...)` for
//!   structural changes (added / removed / modified / renamed paths) and
//!   `gix_diff::blob` (`imara-diff` under the hood) for the `-U0` hunks
//!   we need to map line ranges through.
//! * Worktree layer: `repo.index_worktree_status(...)` for which tracked
//!   files differ from the index, plus the same blob/file pairwise
//!   `imara-diff` pass for hunks. Pure adds (untracked files) are
//!   intentionally not emitted because the previous `git diff-files`
//!   subprocess didn't surface them either.
//!
//! Rename detection is configured via `gix_diff::Rewrites` with a 50%
//! similarity threshold, matching git's `-M` default. When the change
//! count exceeds `GIT_MESH_RENAME_BUDGET` we re-run with rewrites
//! disabled, mirroring the previous fall-back warning.

use super::super::walker::rename_budget;
use crate::git;
use crate::{Error, Result};
use std::collections::{HashMap, HashSet};

use gix::bstr::ByteSlice;

/// Per-run, per-layer cache of structured layer diffs.
pub(crate) struct LayerDiffs {
    pub(crate) map: HashMap<String, DiffEntry>,
    pub(crate) renamed_from: HashMap<String, String>,
    pub(crate) rename_detection_disabled: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct DiffEntry {
    pub(crate) new_path: String,
    pub(crate) old_path: String,
    pub(crate) hunks: Vec<(u32, u32, u32, u32)>,
    pub(crate) new_blob: Option<String>,
    pub(crate) deleted: bool,
    pub(crate) intent_to_add: bool,
}

/// HEAD^{tree} → worktree-index diff (the `git diff --cached` layer).
pub(crate) fn read_index_layer(
    repo: &gix::Repository,
    warnings: &mut Vec<String>,
) -> Result<LayerDiffs> {
    let entries = collect_tree_index_changes(repo, /*track_renames:*/ true)?;
    let budget = rename_budget();
    if entries.len() > budget {
        warnings.push(format!(
            "warning: rename detection disabled (--no-renames); {} > GIT_MESH_RENAME_BUDGET={}",
            entries.len(),
            budget
        ));
        let entries = collect_tree_index_changes(repo, /*track_renames:*/ false)?;
        return Ok(into_layer(entries, true));
    }
    Ok(into_layer(entries, false))
}

/// Index → worktree diff (the `git diff` (no `--cached`) layer).
pub(crate) fn read_worktree_layer(
    repo: &gix::Repository,
    warnings: &mut Vec<String>,
) -> Result<LayerDiffs> {
    let entries = collect_index_worktree_changes(repo, /*track_renames:*/ true)?;
    let budget = rename_budget();
    if entries.len() > budget {
        warnings.push(format!(
            "warning: rename detection disabled (--no-renames); {} > GIT_MESH_RENAME_BUDGET={}",
            entries.len(),
            budget
        ));
        let entries = collect_index_worktree_changes(repo, /*track_renames:*/ false)?;
        return Ok(into_layer(entries, true));
    }
    Ok(into_layer(entries, false))
}

fn into_layer(entries: Vec<DiffEntry>, rename_detection_disabled: bool) -> LayerDiffs {
    let mut map = HashMap::new();
    let mut renamed_from = HashMap::new();
    for e in entries {
        if e.old_path != e.new_path {
            renamed_from.insert(e.old_path.clone(), e.new_path.clone());
            map.insert(e.old_path.clone(), e.clone());
        }
        map.insert(e.new_path.clone(), e);
    }
    LayerDiffs {
        map,
        renamed_from,
        rename_detection_disabled,
    }
}

// ---------------------------------------------------------------------------
// Index layer (HEAD^{tree} → worktree index).
// ---------------------------------------------------------------------------

fn collect_tree_index_changes(
    repo: &gix::Repository,
    track_renames: bool,
) -> Result<Vec<DiffEntry>> {
    let head_tree = repo
        .head_tree_id_or_empty()
        .map_err(|e| Error::Git(format!("resolve HEAD tree: {e}")))?;
    let worktree_index = repo
        .index_or_load_from_head_or_empty()
        .map_err(|e| Error::Git(format!("load index: {e}")))?;

    let renames = if track_renames {
        gix::status::tree_index::TrackRenames::Given(default_rewrites())
    } else {
        gix::status::tree_index::TrackRenames::Disabled
    };

    let mut raw: Vec<RawIndexChange> = Vec::new();
    repo.tree_index_status::<std::convert::Infallible>(
        &head_tree,
        &worktree_index,
        None,
        renames,
        |change, _tree_idx, wt_idx| {
            use gix::diff::index::ChangeRef;
            match change {
                ChangeRef::Addition {
                    location,
                    index,
                    id,
                    ..
                } => {
                    let path = bstr_to_string(&location);
                    let entry = &wt_idx.entries()[index];
                    let intent = entry
                        .flags
                        .contains(gix::index::entry::Flags::INTENT_TO_ADD);
                    raw.push(RawIndexChange::Added {
                        path,
                        new_blob: id.to_hex().to_string(),
                        intent_to_add: intent,
                    });
                }
                ChangeRef::Deletion { location, .. } => {
                    raw.push(RawIndexChange::Deleted {
                        path: bstr_to_string(&location),
                    });
                }
                ChangeRef::Modification {
                    location,
                    previous_id,
                    id,
                    ..
                } => {
                    raw.push(RawIndexChange::Modified {
                        path: bstr_to_string(&location),
                        old_blob: previous_id.to_hex().to_string(),
                        new_blob: id.to_hex().to_string(),
                    });
                }
                ChangeRef::Rewrite {
                    source_location,
                    location,
                    source_id,
                    id,
                    ..
                } => {
                    raw.push(RawIndexChange::Rewrite {
                        old_path: bstr_to_string(&source_location),
                        new_path: bstr_to_string(&location),
                        old_blob: source_id.to_hex().to_string(),
                        new_blob: id.to_hex().to_string(),
                    });
                }
            }
            Ok(std::ops::ControlFlow::Continue(()))
        },
    )
    .map_err(|e| Error::Git(format!("tree-index diff: {e}")))?;

    let mut out = Vec::with_capacity(raw.len());
    for change in raw {
        out.push(materialize_index_entry(repo, change)?);
    }
    Ok(out)
}

enum RawIndexChange {
    Added {
        path: String,
        new_blob: String,
        intent_to_add: bool,
    },
    Deleted {
        path: String,
    },
    Modified {
        path: String,
        old_blob: String,
        new_blob: String,
    },
    Rewrite {
        old_path: String,
        new_path: String,
        old_blob: String,
        new_blob: String,
    },
}

fn materialize_index_entry(repo: &gix::Repository, change: RawIndexChange) -> Result<DiffEntry> {
    match change {
        RawIndexChange::Added {
            path,
            new_blob,
            intent_to_add,
        } => Ok(DiffEntry {
            new_path: path.clone(),
            old_path: path,
            hunks: Vec::new(),
            new_blob: if intent_to_add { None } else { Some(new_blob) },
            deleted: false,
            intent_to_add,
        }),
        RawIndexChange::Deleted { path } => Ok(DiffEntry {
            new_path: path.clone(),
            old_path: path,
            hunks: Vec::new(),
            new_blob: None,
            deleted: true,
            intent_to_add: false,
        }),
        RawIndexChange::Modified {
            path,
            old_blob,
            new_blob,
        } => {
            let hunks = compute_blob_hunks(repo, &old_blob, &new_blob)?;
            Ok(DiffEntry {
                new_path: path.clone(),
                old_path: path,
                hunks,
                new_blob: Some(new_blob),
                deleted: false,
                intent_to_add: false,
            })
        }
        RawIndexChange::Rewrite {
            old_path,
            new_path,
            old_blob,
            new_blob,
        } => {
            let hunks = if old_blob == new_blob {
                Vec::new()
            } else {
                compute_blob_hunks(repo, &old_blob, &new_blob)?
            };
            Ok(DiffEntry {
                new_path,
                old_path,
                hunks,
                new_blob: Some(new_blob),
                deleted: false,
                intent_to_add: false,
            })
        }
    }
}

// ---------------------------------------------------------------------------
// Worktree layer (index → worktree files).
// ---------------------------------------------------------------------------

fn collect_index_worktree_changes(
    repo: &gix::Repository,
    track_renames: bool,
) -> Result<Vec<DiffEntry>> {
    let index = repo
        .index_or_load_from_head_or_empty()
        .map_err(|e| Error::Git(format!("load index: {e}")))?;
    let workdir = git::work_dir(repo)?.to_path_buf();

    // First pass: per-tracked-file modification + deletion check, hashing
    // the on-disk file (or noting it as missing). This mirrors what
    // `git diff-files` reports for tracked entries; it intentionally
    // doesn't surface untracked files (matching prior behavior).
    //
    // Bytes are normalized through the clean (to-git) filter pipeline
    // before hashing so that `filter=lfs`, `text`, and other core
    // filters don't spuriously diverge from the index blob (e.g. LFS
    // stores a pointer in the index while the worktree keeps the
    // smudged content). The cached bytes are reused downstream by the
    // hunk computation so we don't re-read and re-filter.
    struct WorktreeChange {
        path: String,
        old_blob: gix::ObjectId,
        new_blob: Option<gix::ObjectId>, // None ⇒ deleted on disk
        new_bytes: Option<Vec<u8>>,      // Some iff new_blob.is_some()
    }
    let mut changes: Vec<WorktreeChange> = Vec::new();
    let mut seen_paths: HashSet<String> = HashSet::new();
    for entry in index.entries() {
        if entry.stage() != gix::index::entry::Stage::Unconflicted {
            continue;
        }
        if !is_blob_mode(entry.mode) {
            continue;
        }
        let rel = entry.path(&index).to_str_lossy().into_owned();
        seen_paths.insert(rel.clone());
        let abs = workdir.join(&rel);
        let bytes = match read_worktree_cleaned(repo, &abs, &rel) {
            Ok(Some(b)) => b,
            Ok(None) => {
                changes.push(WorktreeChange {
                    path: rel,
                    old_blob: entry.id,
                    new_blob: None,
                    new_bytes: None,
                });
                continue;
            }
            Err(e) => return Err(Error::Git(format!("read `{rel}`: {e}"))),
        };
        let new_id = git::hash_blob(&bytes)?;
        if new_id != entry.id {
            changes.push(WorktreeChange {
                path: rel,
                old_blob: entry.id,
                new_blob: Some(new_id),
                new_bytes: Some(bytes),
            });
        }
    }

    // Optional rename pairing between deletions and *modified* tracked
    // adds. Pure untracked-file additions aren't enumerated here, which
    // matches the previous `git diff-files` behavior. A rename therefore
    // collapses a `Deleted` + `Modified(new)` pair when both sides were
    // already in the index — this only happens when the user staged a
    // `git mv` then continued editing, which the old subprocess pipeline
    // also wouldn't pair without dirwalk support.
    let mut entries = Vec::with_capacity(changes.len());
    if track_renames {
        // Best-effort exact-match pairing: when a deleted path's blob
        // equals a modified path's *current* worktree blob, treat the
        // pair as a rename. This is strictly weaker than git's `-M50`
        // similarity heuristic but covers the rename cases the engine
        // exercises (whole-file rename + zero edits).
        let mut deletions: Vec<usize> = Vec::new();
        let mut additions: Vec<usize> = Vec::new();
        for (idx, ch) in changes.iter().enumerate() {
            match ch.new_blob {
                None => deletions.push(idx),
                Some(_) => additions.push(idx),
            }
        }
        let mut paired_dels: HashSet<usize> = HashSet::new();
        let mut paired_adds: HashSet<usize> = HashSet::new();
        for &di in &deletions {
            for &ai in &additions {
                if paired_adds.contains(&ai) {
                    continue;
                }
                if changes[di].old_blob == changes[ai].new_blob.unwrap() {
                    paired_dels.insert(di);
                    paired_adds.insert(ai);
                    let new_blob_hex = changes[ai].new_blob.unwrap().to_hex().to_string();
                    entries.push(DiffEntry {
                        new_path: changes[ai].path.clone(),
                        old_path: changes[di].path.clone(),
                        hunks: Vec::new(),
                        // Worktree-layer entries don't carry a blob OID
                        // in the prior parser (it set `new_blob = None`
                        // because `worktree=true` skipped index parsing).
                        new_blob: Some(new_blob_hex),
                        deleted: false,
                        intent_to_add: false,
                    });
                    break;
                }
            }
        }
        for (idx, ch) in changes.into_iter().enumerate() {
            if paired_dels.contains(&idx) || paired_adds.contains(&idx) {
                continue;
            }
            entries.push(worktree_change_to_entry(
                repo,
                ch.path,
                ch.old_blob,
                ch.new_blob,
                ch.new_bytes,
            )?);
        }
    } else {
        for ch in changes {
            entries.push(worktree_change_to_entry(
                repo,
                ch.path,
                ch.old_blob,
                ch.new_blob,
                ch.new_bytes,
            )?);
        }
    }

    Ok(entries)
}

fn worktree_change_to_entry(
    repo: &gix::Repository,
    path: String,
    old_blob: gix::ObjectId,
    new_blob: Option<gix::ObjectId>,
    new_bytes: Option<Vec<u8>>,
) -> Result<DiffEntry> {
    match new_blob {
        None => Ok(DiffEntry {
            new_path: path.clone(),
            old_path: path,
            hunks: Vec::new(),
            new_blob: None,
            deleted: true,
            intent_to_add: false,
        }),
        Some(new_id) => {
            let new_bytes = new_bytes.unwrap_or_default();
            let old_bytes =
                git::read_blob_bytes(repo, &old_blob.to_hex().to_string()).unwrap_or_default();
            let hunks = compute_hunks_from_bytes(&old_bytes, &new_bytes);
            // Worktree layer historically doesn't fill `new_blob` (the
            // old text parser only populated it for the index layer
            // because `worktree=true` skipped `index ` line parsing).
            let _ = new_id;
            Ok(DiffEntry {
                new_path: path.clone(),
                old_path: path,
                hunks,
                new_blob: None,
                deleted: false,
                intent_to_add: false,
            })
        }
    }
}

// ---------------------------------------------------------------------------
// Hunk computation via imara-diff at -U0 (no context).
// ---------------------------------------------------------------------------

/// Read a worktree file applying the clean (to-git) filter pipeline so
/// its bytes hash-match the index blob under core filters like
/// `filter=lfs` or `text`. Returns `Ok(None)` when the file is missing.
/// Symlinks return the link target as bytes (git's blob form for a
/// symlink entry). Non-`ErrorKind::NotFound` IO errors surface.
fn read_worktree_cleaned(
    repo: &gix::Repository,
    abs: &std::path::Path,
    rel: &str,
) -> std::io::Result<Option<Vec<u8>>> {
    let md = match std::fs::symlink_metadata(abs) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    if md.file_type().is_symlink() {
        let target = std::fs::read_link(abs)?;
        return Ok(Some(target.to_string_lossy().into_owned().into_bytes()));
    }
    let file = std::fs::File::open(abs)?;
    let Ok((mut pipeline, index)) = repo.filter_pipeline(None) else {
        let mut buf = Vec::new();
        let mut f = std::fs::File::open(abs)?;
        use std::io::Read;
        f.read_to_end(&mut buf)?;
        return Ok(Some(buf));
    };
    let outcome = pipeline.convert_to_git(file, std::path::Path::new(rel), &index);
    let Ok(outcome) = outcome else {
        let mut buf = Vec::new();
        let mut f = std::fs::File::open(abs)?;
        use std::io::Read;
        f.read_to_end(&mut buf)?;
        return Ok(Some(buf));
    };
    use gix::filter::plumbing::pipeline::convert::ToGitOutcome;
    use std::io::Read;
    let mut out = Vec::new();
    match outcome {
        ToGitOutcome::Unchanged(mut r) => {
            r.read_to_end(&mut out)?;
        }
        ToGitOutcome::Buffer(buf) => out.extend_from_slice(buf),
        ToGitOutcome::Process(mut r) => {
            r.read_to_end(&mut out)?;
        }
    }
    Ok(Some(out))
}

fn compute_blob_hunks(
    repo: &gix::Repository,
    old_blob_hex: &str,
    new_blob_hex: &str,
) -> Result<Vec<(u32, u32, u32, u32)>> {
    let old_bytes = git::read_blob_bytes(repo, old_blob_hex).unwrap_or_default();
    let new_bytes = git::read_blob_bytes(repo, new_blob_hex).unwrap_or_default();
    Ok(compute_hunks_from_bytes(&old_bytes, &new_bytes))
}

fn compute_hunks_from_bytes(old_bytes: &[u8], new_bytes: &[u8]) -> Vec<(u32, u32, u32, u32)> {
    use gix::diff::blob::sources::byte_lines;
    use gix::diff::blob::{Algorithm, diff, intern::InternedInput};

    let input = InternedInput::new(byte_lines(old_bytes), byte_lines(new_bytes));
    let mut hunks: Vec<(u32, u32, u32, u32)> = Vec::new();
    diff(
        Algorithm::Histogram,
        &input,
        |before: std::ops::Range<u32>, after: std::ops::Range<u32>| {
            // Convert 0-based imara token ranges into git's 1-based
            // unified-hunk header semantics (`@@ -os,oc +ns,nc @@`):
            //   * for an empty (oc==0) before-anchor the start is the
            //     line *before* the insertion (so 0 when inserting at
            //     line 1, matching git's `-0,0` for prepended content
            //     and `-N,0` for inserts after line N).
            //   * the after-anchor follows symmetrically.
            let oc = before.end - before.start;
            let nc = after.end - after.start;
            let os = if oc == 0 {
                before.start
            } else {
                before.start + 1
            };
            let ns = if nc == 0 {
                after.start
            } else {
                after.start + 1
            };
            hunks.push((os, oc, ns, nc));
        },
    );
    hunks
}

// ---------------------------------------------------------------------------
// Helpers shared by the engine layers.
// ---------------------------------------------------------------------------

fn bstr_to_string(b: &gix::bstr::BStr) -> String {
    b.to_str_lossy().into_owned()
}

fn is_blob_mode(mode: gix::index::entry::Mode) -> bool {
    use gix::index::entry::Mode as M;
    mode == M::FILE || mode == M::FILE_EXECUTABLE || mode == M::SYMLINK
}

fn default_rewrites() -> gix::diff::Rewrites {
    // Match git's `-M` default (50% similarity, no copy detection) via
    // gix's published defaults rather than open-coding the fields.
    gix::diff::Rewrites::default()
}

pub(crate) fn read_conflicted_paths(repo: &gix::Repository) -> Result<HashSet<String>> {
    let entries = git::index_entries(repo)?;
    let mut set = HashSet::new();
    for entry in entries {
        if entry.stage != gix::index::entry::Stage::Unconflicted {
            set.insert(entry.path);
        }
    }
    Ok(set)
}

pub(crate) fn read_index_trailer(repo: &gix::Repository) -> Result<[u8; 20]> {
    let index_path = git::git_dir(repo).join("index");
    let bytes = std::fs::read(&index_path)?;
    if bytes.len() < 20 {
        return Err(Error::Git("index too short for trailer".into()));
    }
    let mut out = [0u8; 20];
    out.copy_from_slice(&bytes[bytes.len() - 20..]);
    Ok(out)
}

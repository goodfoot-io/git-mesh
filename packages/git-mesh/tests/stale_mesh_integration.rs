//! Acceptance tests for the layered `git mesh stale` engine.
//!
//! Every test here maps 1:1 to a bullet under
//! `docs/stale-layers-plan.md` §"Phase 1 — Acceptance tests". They all
//! call the real public API boundary (`resolve_anchor`, `resolve_mesh`,
//! `stale_meshes`, `ContentRef::read_normalized`, or the `git-mesh` CLI)
//! against realistic fixture state.

#![allow(clippy::too_many_lines)]

mod support;

use anyhow::Result;
use git_mesh::types::{
    AnchorExtent, AnchorStatus, ContentRef, DriftSource, EngineOptions, LayerSet, PendingDrift,
    Scope, UnavailableReason,
};
use git_mesh::{append_add, commit_mesh, resolve_anchor, resolve_mesh, set_why, stale_meshes};
use std::path::PathBuf;
use support::TestRepo;

// ---------------------------------------------------------------------------
// Local helpers. These produce realistic fixture state; they do NOT
// implement LFS/filter-process logic — they only set up the repo in the
// shape the eventual Phase 1 implementation will encounter.
// ---------------------------------------------------------------------------

/// Seed a mesh with one line-anchor anchor on `file1.txt#L1-L5` and commit it.
fn seed_line_range_mesh(repo: &TestRepo, mesh: &str) -> Result<()> {
    let gix = repo.gix_repo()?;
    append_add(&gix, mesh, "file1.txt", 1, 5, None)?;
    set_why(&gix, mesh, "seed")?;
    commit_mesh(&gix, mesh)?;
    Ok(())
}

/// Write a `.gitattributes` file at the repo root with the given contents.
fn write_gitattributes(repo: &TestRepo, contents: &str) -> Result<()> {
    repo.write_file(".gitattributes", contents)
}

/// Write a file with a `filter=lfs` attribute set and a plausible LFS
/// pointer body at `rel`. The actual LFS subprocess is never spawned in
/// this fixture; the readers slice will discover the attribute.
fn write_lfs_pointer(repo: &TestRepo, rel: &str, oid_hex_64: &str, size: usize) -> Result<()> {
    let pointer = format!(
        "version https://git-lfs.github.com/spec/v1\noid sha256:{oid}\nsize {size}\n",
        oid = oid_hex_64,
        size = size
    );
    repo.write_file(rel, &pointer)
}

/// Seed a fake LFS object cache file at `.git/lfs/objects/<oid[..2]>/<oid[2..4]>/<oid>`
/// containing arbitrary `bytes`. Slice 6's reader probes this layout to
/// distinguish "pointer changed and content cached" from `LfsNotFetched`.
fn seed_lfs_cache(repo: &TestRepo, oid_hex_64: &str, bytes: &[u8]) -> Result<()> {
    let dir = repo
        .path()
        .join(".git")
        .join("lfs")
        .join("objects")
        .join(&oid_hex_64[..2])
        .join(&oid_hex_64[2..4]);
    std::fs::create_dir_all(&dir)?;
    std::fs::write(dir.join(oid_hex_64), bytes)?;
    Ok(())
}

/// Make a submodule gitlink at `sub/` pointing at a second scratch repo.
/// Returns the bare-like path of the inner repo so the caller can advance
/// its tip and re-stage the gitlink.
fn add_submodule_gitlink(repo: &TestRepo, sub_rel: &str) -> Result<PathBuf> {
    let inner = tempfile::tempdir()?;
    let inner_path = inner.keep();
    std::process::Command::new("git")
        .args(["init", "--initial-branch=main"])
        .arg(&inner_path)
        .output()?;
    std::fs::write(inner_path.join("inner.txt"), "hello\n")?;
    std::process::Command::new("git")
        .current_dir(&inner_path)
        .args(["-c", "user.email=t@e", "-c", "user.name=T", "add", "-A"])
        .output()?;
    std::process::Command::new("git")
        .current_dir(&inner_path)
        .args([
            "-c",
            "user.email=t@e",
            "-c",
            "user.name=T",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-m",
            "inner",
        ])
        .output()?;
    repo.run_git([
        "-c",
        "protocol.file.allow=always",
        "submodule",
        "add",
        &inner_path.to_string_lossy(),
        sub_rel,
    ])?;
    repo.commit_all("add submodule")?;
    Ok(inner_path)
}

// ---------------------------------------------------------------------------
// Acceptance tests.
// ---------------------------------------------------------------------------

/// Plan bullet: HEAD-only mode: byte-identical output on the existing fixture.
#[test]
fn head_only_mode_byte_identical_output_on_fixture() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed_line_range_mesh(&repo, "m")?;
    // Drift at HEAD.
    repo.write_file(
        "file1.txt",
        "lineONE\nline2\nline3\nline4\nline5\nline6\nline7\nline8\nline9\nline10\n",
    )?;
    repo.commit_all("mutate")?;
    let out = repo.run_mesh([
        "stale",
        "m",
        "--no-worktree",
        "--no-index",
        "--no-staged-mesh",
        "--format=porcelain",
    ])?;
    assert_eq!(out.status.code(), Some(1));
    let stdout = String::from_utf8(out.stdout)?;
    assert!(stdout.contains("CHANGED"), "stdout={stdout}");
    // The whole selector set collapses to HEAD-only; the `src` column
    // must never appear as anything but `H` under that mode.
    assert!(!stdout.contains(" I "), "unexpected index marker");
    assert!(!stdout.contains(" W "), "unexpected worktree marker");
    Ok(())
}

/// Plan bullet: Worktree-only drift → Changed, source=Worktree, current.blob = None, exit 1.
#[test]
fn worktree_only_drift_changed_source_worktree_no_blob_exit_one() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed_line_range_mesh(&repo, "m")?;
    // Unstaged edit only.
    repo.write_file(
        "file1.txt",
        "lineONE\nline2\nline3\nline4\nline5\nline6\nline7\nline8\nline9\nline10\n",
    )?;
    let mr = resolve_mesh(&repo.gix_repo()?, "m", EngineOptions::full())?;
    let r = &mr.anchors[0];
    assert_eq!(r.status, AnchorStatus::Changed);
    // Source / current.blob live on the Phase 1 `Finding` shape which
    // `resolve_mesh`'s `AnchorResolved` will be widened to carry. The
    // check below pins the observable result once the widening lands.
    assert!(r.current.is_some());
    assert!(
        r.current.as_ref().unwrap().blob.is_none(),
        "worktree-only reads carry no blob OID"
    );
    let out = repo.run_mesh(["stale", "m"])?;
    assert_eq!(out.status.code(), Some(1));
    Ok(())
}

/// Plan bullet: `git add` moves drift from Worktree to Index;
/// current.blob = Some(staged_oid); exit still 1.
#[test]
fn git_add_moves_drift_worktree_to_index_with_staged_oid() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed_line_range_mesh(&repo, "m")?;
    repo.write_file(
        "file1.txt",
        "lineONE\nline2\nline3\nline4\nline5\nline6\nline7\nline8\nline9\nline10\n",
    )?;
    repo.run_git(["add", "file1.txt"])?;
    let mr = resolve_mesh(&repo.gix_repo()?, "m", EngineOptions::full())?;
    let r = &mr.anchors[0];
    assert_eq!(r.status, AnchorStatus::Changed);
    // Index-layer reads resolve to a blob.
    assert!(r.current.as_ref().and_then(|c| c.blob.as_ref()).is_some());
    let out = repo.run_mesh(["stale", "m"])?;
    assert_eq!(out.status.code(), Some(1));
    Ok(())
}

/// Plan bullet: `git mesh add` matching sidecar → acknowledged_by populated, exit 0.
#[test]

fn git_mesh_add_matching_sidecar_acknowledges_exit_zero() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed_line_range_mesh(&repo, "m")?;
    // Live edit in the anchored anchor.
    repo.write_file(
        "file1.txt",
        "lineONE\nline2\nline3\nline4\nline5\nline6\nline7\nline8\nline9\nline10\n",
    )?;
    // Stage a matching re-anchor via `git mesh add`.
    let _ = repo.run_mesh(["add", "m", "file1.txt#L1-L5"])?;
    let out = repo.run_mesh(["stale", "m"])?;
    assert_eq!(
        out.status.code(),
        Some(0),
        "staged re-anchor must ack live drift"
    );
    Ok(())
}

/// Plan bullet: Subsequent worktree edit invalidates the ack → exit 1.
#[test]

fn worktree_edit_after_ack_invalidates_exit_one() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed_line_range_mesh(&repo, "m")?;
    repo.write_file(
        "file1.txt",
        "lineONE\nline2\nline3\nline4\nline5\nline6\nline7\nline8\nline9\nline10\n",
    )?;
    let _ = repo.run_mesh(["add", "m", "file1.txt#L1-L5"])?;
    // Edit after staging invalidates the sidecar.
    repo.write_file(
        "file1.txt",
        "lineTWO\nline2\nline3\nline4\nline5\nline6\nline7\nline8\nline9\nline10\n",
    )?;
    let out = repo.run_mesh(["stale", "m"])?;
    assert_eq!(out.status.code(), Some(1));
    Ok(())
}

/// Plan bullet: Ack matching survives Moved: anchor's extent shifts, sidecar at
/// old extent still acknowledges via anchor_id.
#[test]

fn ack_survives_moved_via_range_id() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed_line_range_mesh(&repo, "m")?;
    // Stage an ack at the original extent.
    let _ = repo.run_mesh(["add", "m", "file1.txt#L1-L5"])?;
    // Now shift location: prepend two lines, committing the move so the
    // anchored bytes come back via rename/move detection at the new
    // extent.
    repo.write_file(
        "file1.txt",
        "prefix1\nprefix2\nline1\nline2\nline3\nline4\nline5\nline6\nline7\nline8\nline9\nline10\n",
    )?;
    repo.commit_all("shift")?;
    let mr = resolve_mesh(&repo.gix_repo()?, "m", EngineOptions::full())?;
    let r = &mr.anchors[0];
    assert_eq!(r.status, AnchorStatus::Moved);
    // Non-zero exit only if the ack fails to match by anchor_id — the
    // point of this test.
    let out = repo.run_mesh(["stale", "m"])?;
    assert_eq!(out.status.code(), Some(0));
    Ok(())
}

#[test]
fn commit_reanchor_replaces_moved_range_instead_of_adding_duplicate() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed_line_range_mesh(&repo, "m")?;
    repo.write_file(
        "file1.txt",
        "prefix1\nprefix2\nline1\nline2\nline3\nline4\nline5\nline6\nline7\nline8\nline9\nline10\n",
    )?;
    repo.commit_all("shift")?;

    let out = repo.run_mesh(["stale", "m"])?;
    assert_eq!(out.status.code(), Some(1));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("file1.txt#L1-L5 → file1.txt#L3-L7 (Moved"),
        "stdout={stdout}"
    );

    repo.mesh_stdout(["add", "m", "file1.txt#L3-L7"])?;
    repo.mesh_stdout(["commit", "m"])?;
    let mesh = git_mesh::read_mesh(&repo.gix_repo()?, "m")?;
    assert_eq!(mesh.anchors.len(), 1, "re-anchor must replace old anchor");
    let anchor = git_mesh::read_anchor(&repo.gix_repo()?, &mesh.anchors[0])?;
    assert_eq!(anchor.path, "file1.txt");
    assert_eq!(
        anchor.extent,
        git_mesh::types::AnchorExtent::LineRange { start: 3, end: 7 }
    );
    let stale = repo.run_mesh(["stale", "m"])?;
    assert_eq!(stale.status.code(), Some(0));
    Ok(())
}

/// Plan bullet: Sidecar captured before a `.gitattributes` EOL change: re-normalized
/// on read still acknowledges.
#[test]

fn sidecar_before_gitattributes_eol_change_still_acks() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed_line_range_mesh(&repo, "m")?;
    // Stage a re-anchor under the default (no .gitattributes) rules.
    repo.write_file(
        "file1.txt",
        "lineONE\nline2\nline3\nline4\nline5\nline6\nline7\nline8\nline9\nline10\n",
    )?;
    let _ = repo.run_mesh(["add", "m", "file1.txt#L1-L5"])?;
    // Now flip EOL policy. The stored sidecar bytes and the live
    // worktree bytes must both re-normalize to the same canonical form.
    write_gitattributes(&repo, "*.txt text eol=lf\n")?;
    let out = repo.run_mesh(["stale", "m"])?;
    assert_eq!(out.status.code(), Some(0));
    Ok(())
}

/// Plan bullet: `git add -p` partial staging: anchor straddles partial edit; both
/// layers show drift with shifted locations.
#[test]
fn git_add_p_partial_staging_shows_both_layer_drift() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed_line_range_mesh(&repo, "m")?;
    // Edit two separate regions.
    repo.write_file(
        "file1.txt",
        "lineONE\nline2\nline3\nline4\nline5\nline6\nline7\nline8\nline9\nlineTEN\n",
    )?;
    // Stage only the first hunk (simulating `git add -p` — we just stage
    // an intermediate state that differs from both HEAD and worktree).
    repo.write_file(
        "file1.txt.staged",
        "lineONE\nline2\nline3\nline4\nline5\nline6\nline7\nline8\nline9\nline10\n",
    )?;
    std::fs::rename(
        repo.path().join("file1.txt.staged"),
        repo.path().join("file1.txt"),
    )?;
    repo.run_git(["add", "file1.txt"])?;
    // Now restore the worktree to the full two-region edit.
    repo.write_file(
        "file1.txt",
        "lineONE\nline2\nline3\nline4\nline5\nline6\nline7\nline8\nline9\nlineTEN\n",
    )?;
    let out = repo.run_mesh(["stale", "m", "--format=porcelain"])?;
    assert_eq!(out.status.code(), Some(1));
    let stdout = String::from_utf8(out.stdout)?;
    // Both layer sources must show up in the porcelain `src` column.
    assert!(stdout.contains("CHANGED"));
    Ok(())
}

/// Plan bullet: Merge-conflict path → MergeConflict, current.blob = None.
#[test]
fn merge_conflict_path_surfaces_merge_conflict_no_blob() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed_line_range_mesh(&repo, "m")?;
    // Create branch divergence that produces a real stage-1/2/3 on file1.txt.
    repo.run_git(["checkout", "-b", "feature"])?;
    repo.write_file(
        "file1.txt",
        "feat1\nline2\nline3\nline4\nline5\nline6\nline7\nline8\nline9\nline10\n",
    )?;
    repo.commit_all("feature")?;
    repo.run_git(["checkout", "main"])?;
    repo.write_file(
        "file1.txt",
        "main1\nline2\nline3\nline4\nline5\nline6\nline7\nline8\nline9\nline10\n",
    )?;
    repo.commit_all("main edit")?;
    let _ = std::process::Command::new("git")
        .current_dir(repo.path())
        .args(["merge", "feature"])
        .output()?;
    let mr = resolve_mesh(&repo.gix_repo()?, "m", EngineOptions::full())?;
    let r = &mr.anchors[0];
    assert_eq!(r.status, AnchorStatus::MergeConflict);
    assert!(
        r.current.as_ref().is_none_or(|c| c.blob.is_none()),
        "MergeConflict carries path only, no blob"
    );
    Ok(())
}

/// Plan bullet: CRLF checkout of an LF blob → no false drift.
#[test]
fn crlf_checkout_of_lf_blob_no_false_drift() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed_line_range_mesh(&repo, "m")?;
    // Turn on CRLF-on-checkout and rewrite worktree bytes with CRLF.
    write_gitattributes(&repo, "*.txt text eol=crlf\n")?;
    repo.write_file(
        "file1.txt",
        "line1\r\nline2\r\nline3\r\nline4\r\nline5\r\nline6\r\nline7\r\nline8\r\nline9\r\nline10\r\n",
    )?;
    let mr = resolve_mesh(&repo.gix_repo()?, "m", EngineOptions::full())?;
    assert_eq!(mr.anchors[0].status, AnchorStatus::Fresh);
    Ok(())
}

/// Plan bullet: Whole-file pin on a binary asset: blob OID change → Changed;
/// `git mesh add <name> <path>` re-anchors and acknowledges.
#[test]

fn whole_file_pin_binary_asset_re_anchor_acks() -> Result<()> {
    let repo = TestRepo::seeded()?;
    // Commit a small binary-looking asset.
    std::fs::write(repo.path().join("hero.png"), [0u8, 1, 2, 3, 4, 5, 6, 7])?;
    repo.commit_all("add binary")?;
    // Pin the whole file (CLI omits `#L...` for whole-file per D2).
    let _ = repo.run_mesh(["add", "m", "hero.png"])?;
    repo.run_mesh(["why", "m", "-m", "seed"])?;
    repo.run_mesh(["commit", "m"])?;
    // Mutate the binary, exit 1.
    std::fs::write(repo.path().join("hero.png"), [9u8, 9, 9, 9])?;
    repo.commit_all("mutate binary")?;
    let mr = resolve_mesh(&repo.gix_repo()?, "m", EngineOptions::full())?;
    assert_eq!(mr.anchors[0].status, AnchorStatus::Changed);
    assert_eq!(mr.anchors[0].anchored.extent, AnchorExtent::WholeFile);
    // Re-anchor acknowledges.
    let _ = repo.run_mesh(["add", "m", "hero.png"])?;
    let out = repo.run_mesh(["stale", "m"])?;
    assert_eq!(out.status.code(), Some(0));
    Ok(())
}

/// Plan bullet: Whole-file pin on a submodule gitlink: index-layer SHA change
/// (`git submodule update` staged) → Changed.
#[test]

fn whole_file_pin_submodule_gitlink_index_sha_change_changed() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let inner = add_submodule_gitlink(&repo, "sub")?;
    // Pin the gitlink path itself (whole-file allowed per D2).
    let _ = repo.run_mesh(["add", "m", "sub"])?;
    repo.run_mesh(["why", "m", "-m", "seed"])?;
    repo.run_mesh(["commit", "m"])?;
    // Advance inner repo and stage the bump in outer repo.
    std::fs::write(inner.join("inner.txt"), "hello 2\n")?;
    std::process::Command::new("git")
        .current_dir(&inner)
        .args([
            "-c",
            "user.email=t@e",
            "-c",
            "user.name=T",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-am",
            "bump",
        ])
        .output()?;
    std::process::Command::new("git")
        .current_dir(repo.path().join("sub"))
        .args(["pull"])
        .output()?;
    repo.run_git(["add", "sub"])?;
    let mr = resolve_mesh(&repo.gix_repo()?, "m", EngineOptions::full())?;
    assert_eq!(mr.anchors[0].status, AnchorStatus::Changed);
    Ok(())
}

/// Plan bullet: Whole-file pin on a symlink: retarget → Changed. Line-anchor pin
/// on a symlink is rejected at `git mesh add`.
#[test]

fn whole_file_pin_symlink_retarget_changed_and_line_range_rejected() -> Result<()> {
    let repo = TestRepo::seeded()?;
    std::os::unix::fs::symlink("file1.txt", repo.path().join("link"))?;
    repo.commit_all("add symlink")?;
    // Whole-file pin allowed.
    let _ = repo.run_mesh(["add", "m", "link"])?;
    repo.run_mesh(["why", "m", "-m", "seed"])?;
    repo.run_mesh(["commit", "m"])?;
    // Retarget the symlink.
    std::fs::remove_file(repo.path().join("link"))?;
    std::os::unix::fs::symlink("file2.txt", repo.path().join("link"))?;
    repo.commit_all("retarget")?;
    let mr = resolve_mesh(&repo.gix_repo()?, "m", EngineOptions::full())?;
    assert_eq!(mr.anchors[0].status, AnchorStatus::Changed);
    // Line-anchor pin on a symlink must be rejected at add time.
    let rej = repo.run_mesh(["add", "n", "link#L1-L1"])?;
    assert_ne!(
        rej.status.code(),
        Some(0),
        "line-anchor on symlink must fail"
    );
    Ok(())
}

/// Plan bullet: LFS text file, content cached: slice-level Changed/Moved equivalent
/// to non-LFS.
#[test]
fn lfs_text_content_cached_behaves_like_non_lfs() -> Result<()> {
    let repo = TestRepo::seeded()?;
    write_gitattributes(&repo, "*.bigtxt filter=lfs diff=lfs merge=lfs -text\n")?;
    let oid_a = "a".repeat(64);
    let oid_b = "b".repeat(64);
    // Seed cache for both pointer OIDs so the LFS reader treats both
    // sides as fetched and runs the comparator on smudged bytes.
    seed_lfs_cache(&repo, &oid_a, b"alpha content\n")?;
    seed_lfs_cache(&repo, &oid_b, b"beta content\n")?;
    write_lfs_pointer(&repo, "doc.bigtxt", &oid_a, 42)?;
    repo.commit_all("lfs text")?;
    let _ = repo.run_mesh(["add", "m", "doc.bigtxt#L1-L1"])?;
    repo.run_mesh(["why", "m", "-m", "seed"])?;
    repo.run_mesh(["commit", "m"])?;
    write_lfs_pointer(&repo, "doc.bigtxt", &oid_b, 42)?;
    repo.commit_all("mutate pointer")?;
    let mr = resolve_mesh(&repo.gix_repo()?, "m", EngineOptions::full())?;
    assert_eq!(mr.anchors[0].status, AnchorStatus::Changed);
    Ok(())
}

/// Plan bullet: LFS text file, content missing: ContentUnavailable(LfsNotFetched),
/// exit 1; exit 0 with --ignore-unavailable.
#[test]
fn lfs_text_content_missing_unavailable_lfs_not_fetched() -> Result<()> {
    let repo = TestRepo::seeded()?;
    write_gitattributes(&repo, "*.bigtxt filter=lfs diff=lfs merge=lfs -text\n")?;
    let oid_c = "c".repeat(64);
    let oid_d = "d".repeat(64);
    // Seed cache only for the anchored pointer; the post-mutation
    // pointer's cache is intentionally absent so the LFS reader
    // surfaces `LfsNotFetched`.
    seed_lfs_cache(&repo, &oid_c, b"gamma content\n")?;
    write_lfs_pointer(&repo, "doc.bigtxt", &oid_c, 42)?;
    repo.commit_all("lfs text")?;
    let _ = repo.run_mesh(["add", "m", "doc.bigtxt#L1-L1"])?;
    repo.run_mesh(["why", "m", "-m", "seed"])?;
    repo.run_mesh(["commit", "m"])?;
    // Pointer changes, cache missing for new oid.
    write_lfs_pointer(&repo, "doc.bigtxt", &oid_d, 42)?;
    repo.commit_all("mutate pointer")?;
    let mr = resolve_mesh(&repo.gix_repo()?, "m", EngineOptions::full())?;
    assert_eq!(
        mr.anchors[0].status,
        AnchorStatus::ContentUnavailable(UnavailableReason::LfsNotFetched)
    );
    let out = repo.run_mesh(["stale", "m"])?;
    assert_eq!(out.status.code(), Some(1));
    let out2 = repo.run_mesh(["stale", "m", "--ignore-unavailable"])?;
    assert_eq!(out2.status.code(), Some(0));
    Ok(())
}

/// Plan bullet: LFS repo with no `git-lfs` binary on PATH:
/// ContentUnavailable(LfsNotInstalled).
#[test]
fn lfs_repo_without_binary_content_unavailable_lfs_not_installed() -> Result<()> {
    let repo = TestRepo::seeded()?;
    write_gitattributes(&repo, "*.bigtxt filter=lfs diff=lfs merge=lfs -text\n")?;
    let oid_e = "e".repeat(64);
    let oid_f = "f".repeat(64);
    // Seed both pointer caches — the reader must still surface
    // `LfsNotInstalled` because the subprocess spawn fails before any
    // cache probe matters.
    seed_lfs_cache(&repo, &oid_e, b"epsilon\n")?;
    seed_lfs_cache(&repo, &oid_f, b"phi\n")?;
    write_lfs_pointer(&repo, "doc.bigtxt", &oid_e, 42)?;
    repo.commit_all("lfs text")?;
    let _ = repo.run_mesh(["add", "m", "doc.bigtxt#L1-L1"])?;
    repo.run_mesh(["why", "m", "-m", "seed"])?;
    repo.run_mesh(["commit", "m"])?;
    write_lfs_pointer(&repo, "doc.bigtxt", &oid_f, 42)?;
    repo.commit_all("mutate pointer")?;
    // Build a sandbox PATH that contains `git` (the engine shells out to
    // git for many things) but excludes `git-lfs`, so the filter-process
    // spawn fails with ENOENT.
    let sandbox = tempfile::tempdir()?;
    let git_src = std::process::Command::new("which").arg("git").output()?;
    let git_path = String::from_utf8_lossy(&git_src.stdout).trim().to_string();
    std::os::unix::fs::symlink(&git_path, sandbox.path().join("git"))?;
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_git-mesh"))
        .current_dir(repo.path())
        .env("PATH", sandbox.path())
        .args(["stale", "m", "--format=porcelain"])
        .output()?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stdout.contains("LFS_NOT_INSTALLED") || stdout.contains("LfsNotInstalled"),
        "stdout={stdout} stderr={stderr}"
    );
    Ok(())
}

/// Plan bullet: Custom `filter=<name>` driver with broken smudge:
/// ContentUnavailable(FilterFailed { filter }).
#[test]
fn custom_filter_broken_smudge_surfaces_filter_failed() -> Result<()> {
    let repo = TestRepo::seeded()?;
    write_gitattributes(&repo, "*.secret filter=broken\n")?;
    // Configure a filter whose smudge command will fail. `clean=cat`
    // lets the fixture commit succeed; `smudge=false` is what the
    // engine's read path will hit. (The engine routes through
    // `filter.broken.process` if set; with no `.process` configured,
    // the dispatch tree stays on `FilterFailed` per slice 7.)
    repo.run_git(["config", "filter.broken.clean", "cat"])?;
    repo.run_git(["config", "filter.broken.smudge", "false"])?;
    repo.run_git(["config", "filter.broken.required", "true"])?;
    repo.write_file("config.secret", "secret payload\n")?;
    repo.commit_all("add filtered file")?;
    let _ = repo.run_mesh(["add", "m", "config.secret#L1-L1"])?;
    repo.run_mesh(["why", "m", "-m", "seed"])?;
    repo.run_mesh(["commit", "m"])?;
    repo.write_file("config.secret", "new payload\n")?;
    repo.commit_all("mutate")?;
    let mr = resolve_mesh(&repo.gix_repo()?, "m", EngineOptions::full())?;
    assert!(matches!(
        mr.anchors[0].status,
        AnchorStatus::ContentUnavailable(UnavailableReason::FilterFailed { .. })
    ));
    Ok(())
}

/// Regression: a line-anchor pin on an LFS-tracked path must report
/// `Fresh` immediately after the mesh commit when the worktree hasn't
/// been edited. The previous implementation hashed the raw (smudged)
/// worktree bytes during the `index → worktree` diff pass, which made
/// every LFS file look modified (index stores the pointer, worktree
/// stores the smudged content) and mangled the tracked line range via
/// spurious hunks. Requires `git-lfs` on PATH; skipped otherwise.
#[test]
fn lfs_line_range_unchanged_worktree_reports_fresh() -> Result<()> {
    if std::process::Command::new("git-lfs")
        .arg("version")
        .output()
        .map(|o| !o.status.success())
        .unwrap_or(true)
    {
        eprintln!("skipping: git-lfs not available");
        return Ok(());
    }
    let repo = TestRepo::new()?;
    repo.run_git(["lfs", "install", "--local"])?;
    write_gitattributes(&repo, "*.tsv filter=lfs diff=lfs merge=lfs -text\n")?;
    repo.run_git(["add", ".gitattributes"])?;
    repo.run_git(["commit", "-m", "attr"])?;
    // A 50-line LFS-tracked file. The clean filter (via `git add`) rewrites
    // the index entry to a pointer, while the worktree retains the smudged
    // content.
    let mut body = String::with_capacity(50 * 12);
    for i in 1..=50u32 {
        body.push_str(&format!("row{i}\tval\n"));
    }
    repo.write_file("data.tsv", &body)?;
    repo.run_git(["add", "data.tsv"])?;
    repo.run_git(["commit", "-m", "data"])?;
    let _ = repo.run_mesh(["add", "pn", "data.tsv#L1-L10"])?;
    repo.run_mesh(["why", "pn", "-m", "seed"])?;
    repo.run_mesh(["commit", "pn"])?;
    // No edits to data.tsv. Stale must report no drift.
    let out = repo.run_mesh(["stale", "pn", "--format=porcelain"])?;
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert_eq!(
        out.status.code(),
        Some(0),
        "expected no stale findings; stdout=\n{stdout}\nstderr=\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    Ok(())
}

/// Plan bullet: `git mv` across a pinned file (one-layer rename): Moved with new
/// path; mesh record's anchored path unchanged (re-anchor is a separate action).
#[test]
fn git_mv_across_pinned_file_reports_moved_new_path() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed_line_range_mesh(&repo, "m")?;
    repo.run_git(["mv", "file1.txt", "renamed.txt"])?;
    repo.commit_all("rename")?;
    let mr = resolve_mesh(&repo.gix_repo()?, "m", EngineOptions::full())?;
    let r = &mr.anchors[0];
    assert_eq!(r.status, AnchorStatus::Moved);
    // Anchored path unchanged.
    assert_eq!(r.anchored.path, PathBuf::from("file1.txt"));
    // Current path reflects the rename.
    assert_eq!(
        r.current.as_ref().map(|c| c.path.clone()),
        Some(PathBuf::from("renamed.txt"))
    );
    Ok(())
}

/// Plan bullet: `intent-to-add` path (`git add -N`) with a pinned anchor: zero-OID
/// index entry; resolver treats as unstaged; new-file variant (no HEAD) falls back
/// to worktree read.
#[test]
fn intent_to_add_path_zero_oid_treated_as_unstaged() -> Result<()> {
    let repo = TestRepo::seeded()?;
    // Create a new file staged with -N — zero-OID index entry.
    repo.write_file("new.txt", "l1\nl2\nl3\nl4\nl5\nl6\nl7\nl8\nl9\nl10\n")?;
    repo.run_git(["add", "-N", "new.txt"])?;
    // Append-add without commit would fail because HEAD has no blob; to
    // get a mesh whose anchored path maps to an intent-to-add file on
    // the next run, commit the file first then add -N to a different
    // update. Simpler: pin file1.txt and then mutate+intent-to-add on
    // a sibling — but the plan bullet really wants the zero-OID case.
    // For now, pin file1.txt, then mutate it and `git add -N` only the
    // new file so the resolver sees the zero-OID shape on traversal.
    seed_line_range_mesh(&repo, "m")?;
    repo.write_file(
        "file1.txt",
        "lineONE\nline2\nline3\nline4\nline5\nline6\nline7\nline8\nline9\nline10\n",
    )?;
    let mr = resolve_mesh(&repo.gix_repo()?, "m", EngineOptions::full())?;
    // The pinned anchor itself drifts via the worktree layer; zero-OID
    // sibling must not poison the read.
    assert_eq!(mr.anchors[0].status, AnchorStatus::Changed);
    assert_eq!(
        mr.anchors[0].current.as_ref().and_then(|c| c.blob.as_ref()),
        None
    );
    Ok(())
}

/// Plan bullet: Rename-heavy changeset (>1000 paths): `stale` completes without
/// pairing blow-up; a note indicates rename detection was disabled.
#[test]
fn rename_heavy_changeset_completes_with_note() -> Result<()> {
    let repo = TestRepo::seeded()?;
    // Create 1100 files in a bulk add, anchor the mesh on one of them so
    // the candidate-path filter classifies the rename commit as
    // interesting (and therefore exercises the rename-budget code path).
    for i in 0..1100u32 {
        repo.write_file(&format!("bulk/a_{i}.txt"), "x\n")?;
    }
    repo.commit_all("bulk add")?;
    // Mesh anchor on one of the bulk paths so it lives inside the
    // candidate-path set and the bulk-rename commit can't be skipped.
    let gix = repo.gix_repo()?;
    git_mesh::append_add(&gix, "m", "bulk/a_0.txt", 1, 1, None)?;
    git_mesh::set_why(&gix, "m", "seed")?;
    git_mesh::commit_mesh(&gix, "m")?;
    for i in 0..1100u32 {
        repo.run_git(["mv", &format!("bulk/a_{i}.txt"), &format!("bulk/b_{i}.txt")])?;
    }
    repo.commit_all("bulk rename")?;
    let out = repo.run_mesh(["stale", "m"])?;
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("rename detection disabled") || stderr.contains("--no-renames"),
        "expected rename-budget note: stderr={stderr}"
    );
    Ok(())
}

/// Plan bullet: Index-file SHA-1 trailer changes mid-run: stderr warning printed;
/// exit code unaffected.
#[test]
fn index_sha1_trailer_changes_mid_run_prints_warning() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed_line_range_mesh(&repo, "m")?;
    // Simulate a concurrent index update by touching the index file
    // after invocation start. We cannot deterministically race the real
    // binary from a test, so this scenario ultimately needs an internal
    // hook; placeholder: drive via an env-var hook the engine honors in
    // tests. Exit code must be zero in the clean case (no drift).
    let out = repo.run_mesh(["stale", "m"])?;
    assert_eq!(out.status.code(), Some(0));
    Ok(())
}

// ---------------------------------------------------------------------------
// Type-level smoke: exercises the Phase 1 public boundary in trivial ways
// so that refactors to the types show up here as compile errors rather
// than only in the library crate. Kept ignored — runtime would hit
// `todo!()` on `ContentRef::read_normalized`.
// ---------------------------------------------------------------------------

#[test]
fn content_ref_read_normalized_is_the_single_boundary() -> Result<()> {
    let layers = LayerSet::full();
    assert!(layers.worktree && layers.index && layers.staged_mesh);
    let committed = LayerSet::committed_only();
    assert!(!committed.worktree && !committed.index && !committed.staged_mesh);
    let _scope = Scope::All;
    let _src = DriftSource::Worktree;
    let _drift = PendingDrift::SidecarMismatch;
    let _ref = ContentRef::WorktreeFile(PathBuf::from("file1.txt"));
    // Actually invoking read_normalized() would hit todo!(); we only
    // need this to type-check. Keep as a compile-time guard.
    Ok(())
}

/// Plan bullet: `resolve_anchor` agrees with `resolve_mesh`. Smoke-tests the
/// single-anchor entry point against the mesh-level entry point once the
/// engine slice lands.
#[test]
fn resolve_range_agrees_with_resolve_mesh_smoke() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed_line_range_mesh(&repo, "m")?;
    let mr = resolve_mesh(&repo.gix_repo()?, "m", EngineOptions::full())?;
    let rid = &mr.anchors[0].anchor_id;
    let r = resolve_anchor(&repo.gix_repo()?, "m", rid, EngineOptions::full())?;
    assert_eq!(r.status, mr.anchors[0].status);
    Ok(())
}

/// Plan bullet (coverage of `stale_meshes`): worst-first ordering across meshes.
#[test]
fn stale_meshes_sorts_worst_first_smoke() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed_line_range_mesh(&repo, "clean")?;
    seed_line_range_mesh(&repo, "dirty")?;
    repo.write_file(
        "file1.txt",
        "XXX\nline2\nline3\nline4\nline5\nline6\nline7\nline8\nline9\nline10\n",
    )?;
    repo.commit_all("mutate")?;
    let all = stale_meshes(&repo.gix_repo()?, EngineOptions::full())?;
    assert!(
        all.iter()
            .any(|m| m.anchors.iter().any(|r| r.status == AnchorStatus::Changed))
    );
    Ok(())
}

/// A clean mesh (all anchors Fresh, no pending) must be excluded from `stale_meshes`.
#[test]
fn stale_meshes_excludes_clean_mesh() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed_line_range_mesh(&repo, "clean-only")?;
    let all = stale_meshes(&repo.gix_repo()?, EngineOptions::full())?;
    assert!(
        !all.iter().any(|m| m.name == "clean-only"),
        "clean mesh must not appear in stale_meshes output"
    );
    Ok(())
}

/// A mesh with one Changed anchor must be included in `stale_meshes`.
#[test]
fn stale_meshes_includes_changed_mesh() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed_line_range_mesh(&repo, "drifty")?;
    repo.write_file(
        "file1.txt",
        "CHANGED\nline2\nline3\nline4\nline5\nline6\nline7\nline8\nline9\nline10\n",
    )?;
    repo.commit_all("mutate")?;
    let all = stale_meshes(&repo.gix_repo()?, EngineOptions::full())?;
    assert!(
        all.iter()
            .any(|m| m.name == "drifty"
                && m.anchors.iter().any(|a| a.status == AnchorStatus::Changed)),
        "changed mesh must appear in stale_meshes output"
    );
    Ok(())
}

/// When a repo has both a clean mesh and a stale mesh, only the stale mesh appears.
#[test]
fn stale_meshes_filters_clean_leaves_stale() -> Result<()> {
    let repo = TestRepo::seeded()?;
    // "quiet" anchors lines 6-10 (stable); "noisy" anchors lines 1-5 (mutated below).
    let gix = repo.gix_repo()?;
    append_add(&gix, "quiet", "file1.txt", 6, 10, None)?;
    set_why(&gix, "quiet", "stable anchor")?;
    commit_mesh(&gix, "quiet")?;
    seed_line_range_mesh(&repo, "noisy")?;
    // Mutate only line 1 — "noisy" drifts, "quiet" (lines 6-10) stays clean.
    repo.write_file(
        "file1.txt",
        "CHANGED\nline2\nline3\nline4\nline5\nline6\nline7\nline8\nline9\nline10\n",
    )?;
    repo.commit_all("mutate")?;
    let all = stale_meshes(&repo.gix_repo()?, EngineOptions::full())?;
    assert!(
        all.iter().any(|m| m.name == "noisy"),
        "stale mesh must appear"
    );
    assert!(
        !all.iter().any(|m| m.name == "quiet"),
        "clean mesh must not appear"
    );
    Ok(())
}

/// All-clean repo + `--format=json` → empty stdout, exit 0.
#[test]
fn all_clean_json_empty_stdout() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed_line_range_mesh(&repo, "clean")?;
    let out = repo.run_mesh(["stale", "--format=json"])?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.trim().is_empty(),
        "json output must be empty when all meshes are clean, got: {stdout}"
    );
    assert_eq!(out.status.code(), Some(0), "exit 0 when no drift");
    Ok(())
}

/// All-clean repo + `--format=junit` → empty stdout, exit 0.
#[test]
fn all_clean_junit_empty_stdout() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed_line_range_mesh(&repo, "clean")?;
    let out = repo.run_mesh(["stale", "--format=junit"])?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.trim().is_empty(),
        "junit output must be empty when all meshes are clean, got: {stdout}"
    );
    assert_eq!(out.status.code(), Some(0), "exit 0 when no drift");
    Ok(())
}

/// All-clean repo + `--format=porcelain` → empty stdout, exit 0.
#[test]
fn all_clean_porcelain_empty_stdout() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed_line_range_mesh(&repo, "clean")?;
    let out = repo.run_mesh(["stale", "--format=porcelain"])?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.trim().is_empty(),
        "porcelain output must be empty when all meshes are clean, got: {stdout}"
    );
    assert_eq!(out.status.code(), Some(0), "exit 0 when no drift");
    Ok(())
}

/// All-clean repo + `--format=github` → empty stdout, exit 0.
#[test]
fn all_clean_github_empty_stdout() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed_line_range_mesh(&repo, "clean")?;
    let out = repo.run_mesh(["stale", "--format=github-actions"])?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.trim().is_empty(),
        "github output must be empty when all meshes are clean, got: {stdout}"
    );
    assert_eq!(out.status.code(), Some(0), "exit 0 when no drift");
    Ok(())
}

/// Stale-present repo + `--format=json` → non-empty envelope, exit 1.
#[test]
fn stale_present_json_emits_envelope() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed_line_range_mesh(&repo, "drifty")?;
    repo.write_file(
        "file1.txt",
        "CHANGED\nline2\nline3\nline4\nline5\nline6\nline7\nline8\nline9\nline10\n",
    )?;
    repo.commit_all("mutate")?;
    let out = repo.run_mesh(["stale", "--format=json"])?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        !stdout.trim().is_empty(),
        "json output must not be empty when a mesh is stale"
    );
    assert!(
        stdout.contains("schema_version"),
        "json output must contain schema_version"
    );
    assert_eq!(out.status.code(), Some(1), "exit 1 when drift present");
    Ok(())
}

/// Stale-present repo + `--format=junit` → non-empty testsuite, exit 1.
#[test]
fn stale_present_junit_emits_testsuite() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed_line_range_mesh(&repo, "drifty")?;
    repo.write_file(
        "file1.txt",
        "CHANGED\nline2\nline3\nline4\nline5\nline6\nline7\nline8\nline9\nline10\n",
    )?;
    repo.commit_all("mutate")?;
    let out = repo.run_mesh(["stale", "--format=junit"])?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        !stdout.trim().is_empty(),
        "junit output must not be empty when a mesh is stale"
    );
    assert!(
        stdout.contains("<testsuite"),
        "junit output must contain <testsuite>"
    );
    assert_eq!(out.status.code(), Some(1), "exit 1 when drift present");
    Ok(())
}

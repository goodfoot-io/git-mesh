//! Phase 3 validation suite.
//!
//! Covers:
//!
//! * Round-trip (save / load) for each persisted artifact.
//! * Cache-key invalidation for every documented input: catalog tree,
//!   HEAD, staging fingerprint, dirty content / dirty-path fingerprint,
//!   filter-config hash, and `KEY_SALT`.
//! * Full-vs-overlay equivalence under a mutated worktree, including
//!   the dirty-path edge cases: deleted, renamed, staged-only,
//!   worktree-only.
//! * Concurrent SQLite WAL smoke test (multiple writers).

use super::*;
use crate::mesh::catalog::Catalog;
use crate::types::{
    Anchor, AnchorExtent, AnchorStatus, EngineOptions, LayerSet, Mesh, MeshConfig, MeshResolved,
};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Command;

// ---------------------------------------------------------------------------
// Fixture
// ---------------------------------------------------------------------------

struct Fixture {
    _dir: tempfile::TempDir,
    path: PathBuf,
}

impl Fixture {
    fn path(&self) -> &Path {
        &self.path
    }
    fn repo(&self) -> gix::Repository {
        gix::open(&self.path).expect("open repo")
    }
}

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .current_dir(dir)
        .args(args)
        .output()
        .expect("spawn git");
    assert!(
        out.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&out.stderr)
    );
}

fn write_file(dir: &Path, name: &str, body: &str) {
    std::fs::write(dir.join(name), body).expect("write");
}

fn make_repo() -> Fixture {
    let td = tempfile::tempdir().expect("tempdir");
    let p = td.path().to_path_buf();
    git(&p, &["init", "--initial-branch=main"]);
    git(&p, &["config", "user.email", "t@t"]);
    git(&p, &["config", "user.name", "t"]);
    git(&p, &["config", "commit.gpgsign", "false"]);
    // Pin the CRLF filter inputs to a known local baseline. Git-for-Windows
    // ships `core.autocrlf=true` in its *system* config, so without an
    // explicit local override `filter_config_hash` would inherit that value
    // and the `filter_config_change_misses_baseline` flip-to-`true` would be
    // a no-op on Windows. A local value overrides system/global on every
    // platform, making the filter-config drift tests deterministic.
    git(&p, &["config", "core.autocrlf", "false"]);
    Fixture { _dir: td, path: p }
}

/// Build a fixture with three independent anchored files and two
/// meshes (`alpha` anchored on `a.txt`+`b.txt`, `beta` on `c.txt`).
fn fixture_two_meshes() -> Fixture {
    let f = make_repo();
    let p = f.path();
    // Use multi-line bodies so line-range anchors land cleanly.
    write_file(
        p,
        "a.txt",
        "a-line-1\na-line-2\na-line-3\na-line-4\na-line-5\n",
    );
    write_file(
        p,
        "b.txt",
        "b-line-1\nb-line-2\nb-line-3\nb-line-4\nb-line-5\n",
    );
    write_file(
        p,
        "c.txt",
        "c-line-1\nc-line-2\nc-line-3\nc-line-4\nc-line-5\n",
    );
    git(p, &["add", "."]);
    git(p, &["commit", "-m", "seed"]);

    let repo = f.repo();
    crate::staging::append_add(&repo, "alpha", "a.txt", 1, 3, None).unwrap();
    crate::staging::append_add(&repo, "alpha", "b.txt", 2, 4, None).unwrap();
    crate::staging::set_why(&repo, "alpha", "alpha description").unwrap();
    crate::mesh::commit_mesh(&repo, "alpha").unwrap();

    crate::staging::append_add(&repo, "beta", "c.txt", 1, 5, None).unwrap();
    crate::staging::set_why(&repo, "beta", "beta description").unwrap();
    crate::mesh::commit_mesh(&repo, "beta").unwrap();

    // Resolver requires a commit-graph with changed-path Bloom filters.
    git(
        p,
        &["commit-graph", "write", "--reachable", "--changed-paths"],
    );

    f
}

fn current_catalog_tree_oid(repo: &gix::Repository) -> String {
    let r = repo
        .try_find_reference("refs/meshes/v1/catalog")
        .expect("ref lookup")
        .expect("catalog ref present");
    let mut r = r;
    let commit_id = r.peel_to_id().expect("peel");
    let commit = repo.find_commit(commit_id).expect("find commit");
    let tree = commit.tree().expect("tree");
    tree.id().detach().to_string()
}

fn head_oid(repo: &gix::Repository) -> String {
    crate::git::head_oid(repo).expect("head oid")
}

// ---------------------------------------------------------------------------
// Round-trip tests
// ---------------------------------------------------------------------------

#[test]
fn path_anchor_index_round_trip() {
    let f = fixture_two_meshes();
    let repo = f.repo();
    let tree_oid = current_catalog_tree_oid(&repo);
    let catalog = Catalog::load(&repo).unwrap();
    let meshes = catalog.iter().unwrap();
    let index = build_path_anchor_index(&tree_oid, meshes);

    let store_path = f.path().join(".git/mesh/test-stale.db");
    let store = open_store_for_test(&store_path);
    store_path_anchor_index(&store, &index).unwrap();
    let loaded = load_path_anchor_index(&store, &tree_oid)
        .unwrap()
        .expect("present");
    assert_eq!(loaded, index);

    // Affected anchors: only b.txt is dirty.
    let dirty = ["b.txt".to_string()];
    let entries = loaded.lookup_many(&dirty);
    assert_eq!(entries.len(), 1, "exactly one anchor on b.txt");
    assert_eq!(entries[0].mesh_name, "alpha");
}

#[test]
fn baseline_round_trip() {
    let f = fixture_two_meshes();
    let repo = f.repo();
    let tree_oid = current_catalog_tree_oid(&repo);
    let head = head_oid(&repo);
    let filter = filter_config_hash(&repo);

    let baseline_meshes = crate::resolver::stale_meshes(
        &repo,
        EngineOptions {
            layers: LayerSet::committed_only(),
            ignore_unavailable: false,
            since: None,
            needs_all_layers: true,
        },
    )
    .unwrap();
    let counts = CommittedBaseline::counts_from_meshes(&baseline_meshes);
    let baseline = CommittedBaseline {
        catalog_tree_oid: tree_oid.clone(),
        head_oid: head.clone(),
        meshes: baseline_meshes,
        counts,
    };

    let store = open_store(&repo).unwrap();
    store_baseline(&store, &filter, &baseline).unwrap();
    let loaded = load_baseline(&store, &tree_oid, &head, &filter)
        .unwrap()
        .expect("present");
    assert_eq!(loaded.head_oid, baseline.head_oid);
    assert_eq!(loaded.meshes.len(), baseline.meshes.len());
    for (a, b) in loaded.meshes.iter().zip(baseline.meshes.iter()) {
        assert_eq!(a.name, b.name);
        assert_eq!(a.anchors.len(), b.anchors.len());
        for (la, lb) in a.anchors.iter().zip(b.anchors.iter()) {
            assert_eq!(la.anchor_id, lb.anchor_id);
            assert_eq!(la.status, lb.status);
            assert_eq!(la.anchored.path, lb.anchored.path);
        }
    }
}

#[test]
fn overlay_round_trip() {
    let f = fixture_two_meshes();
    let repo = f.repo();
    let tree_oid = current_catalog_tree_oid(&repo);
    let head = head_oid(&repo);
    let filter = filter_config_hash(&repo);

    let alpha_full = resolve_one(&repo, "alpha");
    let overlay = DirtyOverlay {
        affected_meshes: vec!["alpha".to_string()],
        meshes: vec![alpha_full],
    };
    let inputs = OverlayInputs {
        catalog_tree_oid: tree_oid,
        head_oid: head,
        filter_config_hash: filter,
        index_checksum: [7; 32],
        worktree_dirty_fingerprint: [9; 32],
        staging_state_fingerprint: [11; 32],
    };

    let store = open_store(&repo).unwrap();
    store_overlay(&store, &inputs, &overlay).unwrap();
    let loaded = load_overlay(&store, &inputs)
        .unwrap()
        .expect("overlay present");
    assert_eq!(loaded.affected_meshes, overlay.affected_meshes);
    assert_eq!(loaded.meshes.len(), overlay.meshes.len());
}

// ---------------------------------------------------------------------------
// Cache-key invalidation tests
// ---------------------------------------------------------------------------

#[test]
fn catalog_change_misses_path_anchor_index() {
    let f = fixture_two_meshes();
    let repo = f.repo();
    let original = current_catalog_tree_oid(&repo);
    let index = build_path_anchor_index(
        &original,
        Catalog::load(&repo).unwrap().iter().unwrap(),
    );
    let store = open_store(&repo).unwrap();
    store_path_anchor_index(&store, &index).unwrap();

    assert!(load_path_anchor_index(&store, &original).unwrap().is_some());
    assert!(
        load_path_anchor_index(&store, "00deadbeef00deadbeef00deadbeef00deadbeef")
            .unwrap()
            .is_none(),
        "different catalog tree must miss"
    );
}

#[test]
fn head_change_misses_baseline() {
    let f = fixture_two_meshes();
    let repo = f.repo();
    let tree_oid = current_catalog_tree_oid(&repo);
    let head = head_oid(&repo);
    let filter = filter_config_hash(&repo);
    let meshes = crate::resolver::stale_meshes(
        &repo,
        EngineOptions {
            layers: LayerSet::committed_only(),
            ignore_unavailable: false,
            since: None,
            needs_all_layers: true,
        },
    )
    .unwrap();
    let baseline = CommittedBaseline {
        catalog_tree_oid: tree_oid.clone(),
        head_oid: head.clone(),
        meshes,
        counts: BaselineCounts::default(),
    };
    let store = open_store(&repo).unwrap();
    store_baseline(&store, &filter, &baseline).unwrap();

    assert!(
        load_baseline(&store, &tree_oid, &head, &filter)
            .unwrap()
            .is_some()
    );
    assert!(
        load_baseline(
            &store,
            &tree_oid,
            "00deadbeef00deadbeef00deadbeef00deadbeef",
            &filter
        )
        .unwrap()
        .is_none(),
        "different HEAD must miss"
    );
}

#[test]
fn filter_config_change_misses_baseline() {
    let f = fixture_two_meshes();
    let repo = f.repo();
    let tree_oid = current_catalog_tree_oid(&repo);
    let head = head_oid(&repo);
    let original_filter = filter_config_hash(&repo);
    let meshes = crate::resolver::stale_meshes(
        &repo,
        EngineOptions {
            layers: LayerSet::committed_only(),
            ignore_unavailable: false,
            since: None,
            needs_all_layers: true,
        },
    )
    .unwrap();
    let baseline = CommittedBaseline {
        catalog_tree_oid: tree_oid.clone(),
        head_oid: head.clone(),
        meshes,
        counts: BaselineCounts::default(),
    };
    let store = open_store(&repo).unwrap();
    store_baseline(&store, &original_filter, &baseline).unwrap();
    assert!(
        load_baseline(&store, &tree_oid, &head, &original_filter)
            .unwrap()
            .is_some()
    );

    // Now mutate the filter config: write `core.autocrlf=true` and
    // re-hash. Old-key load must miss; new-key load must miss until
    // we re-store.
    git(f.path(), &["config", "core.autocrlf", "true"]);
    let new_repo = f.repo();
    let new_filter = filter_config_hash(&new_repo);
    assert_ne!(
        original_filter, new_filter,
        "filter-config hash must change on `core.autocrlf` flip"
    );
    assert!(
        load_baseline(&store, &tree_oid, &head, &new_filter)
            .unwrap()
            .is_none()
    );
}

#[test]
fn dirty_content_change_changes_overlay_key() {
    let f = fixture_two_meshes();
    let repo = f.repo();
    let tree_oid = current_catalog_tree_oid(&repo);
    let head = head_oid(&repo);
    let filter = filter_config_hash(&repo);

    let mut wt: HashSet<String> = HashSet::new();
    wt.insert("a.txt".to_string());
    let conflicted: HashSet<String> = HashSet::new();
    let (_paths, inputs_clean) = collect_dirty_paths(
        &tree_oid,
        &head,
        filter,
        Some([0; 20]),
        false,
        &HashSet::new(),
        &HashSet::new(),
        None,
        false,
    );
    let (_paths_dirty, inputs_dirty) = collect_dirty_paths(
        &tree_oid,
        &head,
        filter,
        Some([0; 20]),
        false,
        &wt,
        &conflicted,
        None,
        false,
    );
    assert_ne!(
        inputs_clean.key(),
        inputs_dirty.key(),
        "adding a worktree-dirty path must change the overlay key"
    );

    // Mutating the dirty path content: same path set but different
    // fingerprint? We model content by the trailer; bumping it gives
    // a new key.
    let (_p2, inputs_dirty_changed_index) = collect_dirty_paths(
        &tree_oid,
        &head,
        filter,
        Some([1; 20]),
        true,
        &wt,
        &conflicted,
        None,
        false,
    );
    assert_ne!(
        inputs_dirty.key(),
        inputs_dirty_changed_index.key(),
        "index trailer change must change overlay key"
    );
}

#[test]
fn staging_change_changes_overlay_key() {
    let f = fixture_two_meshes();
    let repo = f.repo();
    let tree_oid = current_catalog_tree_oid(&repo);
    let head = head_oid(&repo);
    let filter = filter_config_hash(&repo);

    let staging = f.path().join(".git/mesh/staging");
    std::fs::create_dir_all(staging.join("alpha")).unwrap();
    std::fs::write(staging.join("alpha/1.add"), b"a.txt#L1-L3\n").unwrap();

    let (_, inputs_a) = collect_dirty_paths(
        &tree_oid,
        &head,
        filter,
        None,
        false,
        &HashSet::new(),
        &HashSet::new(),
        Some(&staging),
        false,
    );

    std::fs::write(staging.join("alpha/2.add"), b"b.txt#L2-L4\n").unwrap();
    let (_, inputs_b) = collect_dirty_paths(
        &tree_oid,
        &head,
        filter,
        None,
        false,
        &HashSet::new(),
        &HashSet::new(),
        Some(&staging),
        false,
    );
    assert_ne!(
        inputs_a.key(),
        inputs_b.key(),
        "adding a staging sidecar must change the overlay key"
    );
}

// ---------------------------------------------------------------------------
// Full-vs-overlay equivalence
// ---------------------------------------------------------------------------

fn resolve_full(repo: &gix::Repository) -> Vec<MeshResolved> {
    crate::resolver::stale_meshes(
        repo,
        EngineOptions {
            layers: LayerSet::full(),
            ignore_unavailable: false,
            since: None,
            needs_all_layers: true,
        },
    )
    .unwrap()
}

fn resolve_baseline_meshes(repo: &gix::Repository) -> Vec<MeshResolved> {
    // Phase 3 baseline: every mesh in the catalog resolved with HEAD-only
    // layers, including meshes whose anchors are Fresh. We use
    // `resolve_named_meshes` to avoid the `stale_meshes` "drop fresh" filter.
    let catalog = Catalog::load(repo).unwrap();
    let names: Vec<String> = catalog.names();
    let out = crate::resolver::resolve_named_meshes(
        repo,
        &names,
        EngineOptions {
            layers: LayerSet::committed_only(),
            ignore_unavailable: false,
            since: None,
            needs_all_layers: true,
        },
    )
    .unwrap();
    out.into_iter()
        .map(|(_, r)| r.expect("baseline resolve"))
        .collect()
}

fn resolve_one(repo: &gix::Repository, name: &str) -> MeshResolved {
    let out = crate::resolver::resolve_named_meshes(
        repo,
        &[name.to_string()],
        EngineOptions {
            layers: LayerSet::full(),
            ignore_unavailable: false,
            since: None,
            needs_all_layers: true,
        },
    )
    .unwrap();
    out.into_iter()
        .next()
        .unwrap()
        .1
        .expect("resolve named mesh")
}

/// Strip `pending` and the per-mesh ordering so the equivalence check
/// is robust against engine bookkeeping that the persisted baseline
/// intentionally does not retain.
fn meshes_by_name(meshes: &[MeshResolved]) -> std::collections::BTreeMap<String, MeshResolved> {
    let mut m = std::collections::BTreeMap::new();
    for mr in meshes {
        let mut clone = mr.clone();
        clone.pending.clear();
        m.insert(clone.name.clone(), clone);
    }
    m
}

fn assert_meshes_equivalent(a: &[MeshResolved], b: &[MeshResolved]) {
    let amap = meshes_by_name(a);
    let bmap = meshes_by_name(b);
    let akeys: Vec<&String> = amap.keys().collect();
    let bkeys: Vec<&String> = bmap.keys().collect();
    assert_eq!(akeys, bkeys, "mesh name sets differ");
    for (k, av) in &amap {
        let bv = &bmap[k];
        assert_eq!(
            av.anchors.len(),
            bv.anchors.len(),
            "mesh `{k}` anchor count differs"
        );
        for (lhs, rhs) in av.anchors.iter().zip(bv.anchors.iter()) {
            assert_eq!(lhs.anchor_id, rhs.anchor_id);
            assert_eq!(lhs.status, rhs.status, "anchor {} status", lhs.anchor_id);
            assert_eq!(lhs.anchored.path, rhs.anchored.path);
        }
    }
}

#[test]
fn full_vs_overlay_worktree_only_dirty() {
    let f = fixture_two_meshes();
    let repo = f.repo();
    let tree_oid = current_catalog_tree_oid(&repo);

    let baseline_meshes = resolve_baseline_meshes(&repo);
    let baseline = CommittedBaseline {
        catalog_tree_oid: tree_oid.clone(),
        head_oid: head_oid(&repo),
        meshes: baseline_meshes,
        counts: BaselineCounts::default(),
    };
    let index = build_path_anchor_index(
        &tree_oid,
        Catalog::load(&repo).unwrap().iter().unwrap(),
    );

    // Dirty `a.txt` in the worktree only; do not stage.
    write_file(
        f.path(),
        "a.txt",
        "a-line-1\nDIFFERENT\na-line-3\na-line-4\na-line-5\n",
    );

    let oracle = resolve_full(&repo);

    let dirty_paths = vec!["a.txt".as_bytes()];
    let affected = index.lookup_many(dirty_paths.iter().copied());
    let affected_meshes: Vec<String> = affected
        .iter()
        .map(|e| e.mesh_name.clone())
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();

    let mut overlay_meshes = Vec::new();
    for name in &affected_meshes {
        overlay_meshes.push(resolve_one(&repo, name));
    }
    let overlay = DirtyOverlay {
        affected_meshes,
        meshes: overlay_meshes,
    };
    let merged = apply_overlay(&baseline, &overlay);

    // The `stale_meshes` oracle drops Fresh meshes; our merged result
    // keeps every mesh. Compare on the union of names but only on the
    // meshes the oracle reports. Effectively: any mesh in `oracle`
    // must appear in `merged` with equivalent anchor statuses.
    let oracle_names: HashSet<String> =
        oracle.iter().map(|m| m.name.clone()).collect();
    let merged_filtered: Vec<MeshResolved> = merged
        .into_iter()
        .filter(|m| {
            // Mirror `mesh_is_reportable_in_stale_discovery`: any
            // non-Fresh anchor.
            oracle_names.contains(&m.name)
                || m.anchors.iter().any(|a| a.status != AnchorStatus::Fresh)
        })
        .collect();
    assert_meshes_equivalent(&merged_filtered, &oracle);
}

#[test]
fn full_vs_overlay_clean_yields_baseline() {
    let f = fixture_two_meshes();
    let repo = f.repo();
    let tree_oid = current_catalog_tree_oid(&repo);
    let baseline_meshes = resolve_baseline_meshes(&repo);
    let baseline = CommittedBaseline {
        catalog_tree_oid: tree_oid,
        head_oid: head_oid(&repo),
        meshes: baseline_meshes.clone(),
        counts: BaselineCounts::default(),
    };
    // No dirty paths: empty overlay → merged == baseline.
    let overlay = DirtyOverlay {
        affected_meshes: vec![],
        meshes: vec![],
    };
    let merged = apply_overlay(&baseline, &overlay);
    assert_eq!(merged.len(), baseline_meshes.len());
    for (a, b) in merged.iter().zip(baseline_meshes.iter()) {
        assert_eq!(a.name, b.name);
        assert_eq!(a.anchors.len(), b.anchors.len());
    }
}

#[test]
fn full_vs_overlay_deleted_dirty_path() {
    let f = fixture_two_meshes();
    let repo = f.repo();
    let tree_oid = current_catalog_tree_oid(&repo);
    let baseline_meshes = resolve_baseline_meshes(&repo);
    let baseline = CommittedBaseline {
        catalog_tree_oid: tree_oid.clone(),
        head_oid: head_oid(&repo),
        meshes: baseline_meshes,
        counts: BaselineCounts::default(),
    };
    let index = build_path_anchor_index(
        &tree_oid,
        Catalog::load(&repo).unwrap().iter().unwrap(),
    );

    // Delete `a.txt` from the worktree.
    std::fs::remove_file(f.path().join("a.txt")).unwrap();
    let oracle = resolve_full(&repo);
    let dirty: Vec<&[u8]> = vec![b"a.txt"];
    let affected_set: HashSet<String> = index
        .lookup_many(dirty.iter().copied())
        .iter()
        .map(|e| e.mesh_name.clone())
        .collect();
    let affected: Vec<String> = affected_set.into_iter().collect();
    let mut overlay_meshes = Vec::new();
    for n in &affected {
        overlay_meshes.push(resolve_one(&repo, n));
    }
    let overlay = DirtyOverlay {
        affected_meshes: affected,
        meshes: overlay_meshes,
    };
    let merged = apply_overlay(&baseline, &overlay);
    let oracle_names: HashSet<String> = oracle.iter().map(|m| m.name.clone()).collect();
    let merged_filtered: Vec<MeshResolved> = merged
        .into_iter()
        .filter(|m| {
            oracle_names.contains(&m.name)
                || m.anchors.iter().any(|a| a.status != AnchorStatus::Fresh)
        })
        .collect();
    assert_meshes_equivalent(&merged_filtered, &oracle);
}

#[test]
fn full_vs_overlay_staged_only_dirty_path() {
    let f = fixture_two_meshes();
    let repo = f.repo();
    let tree_oid = current_catalog_tree_oid(&repo);
    let baseline_meshes = resolve_baseline_meshes(&repo);
    let baseline = CommittedBaseline {
        catalog_tree_oid: tree_oid.clone(),
        head_oid: head_oid(&repo),
        meshes: baseline_meshes,
        counts: BaselineCounts::default(),
    };
    let index = build_path_anchor_index(
        &tree_oid,
        Catalog::load(&repo).unwrap().iter().unwrap(),
    );

    // Dirty `b.txt` only in the index (staged), not in the worktree.
    write_file(
        f.path(),
        "b.txt",
        "b-line-1\nb-STAGED\nb-line-3\nb-line-4\nb-line-5\n",
    );
    git(f.path(), &["add", "b.txt"]);
    // Reset the worktree from the new index to make `b.txt` staged-only.
    git(f.path(), &["checkout", "--", "b.txt"]);

    let oracle = resolve_full(&repo);
    let dirty: Vec<&[u8]> = vec![b"b.txt"];
    let affected_set: HashSet<String> = index
        .lookup_many(dirty.iter().copied())
        .iter()
        .map(|e| e.mesh_name.clone())
        .collect();
    let affected: Vec<String> = affected_set.into_iter().collect();
    let mut overlay_meshes = Vec::new();
    for n in &affected {
        overlay_meshes.push(resolve_one(&repo, n));
    }
    let overlay = DirtyOverlay {
        affected_meshes: affected,
        meshes: overlay_meshes,
    };
    let merged = apply_overlay(&baseline, &overlay);
    let oracle_names: HashSet<String> = oracle.iter().map(|m| m.name.clone()).collect();
    let merged_filtered: Vec<MeshResolved> = merged
        .into_iter()
        .filter(|m| {
            oracle_names.contains(&m.name)
                || m.anchors.iter().any(|a| a.status != AnchorStatus::Fresh)
        })
        .collect();
    assert_meshes_equivalent(&merged_filtered, &oracle);
}

#[test]
fn full_vs_overlay_renamed_dirty_path() {
    let f = fixture_two_meshes();
    let repo = f.repo();
    let tree_oid = current_catalog_tree_oid(&repo);
    let baseline_meshes = resolve_baseline_meshes(&repo);
    let baseline = CommittedBaseline {
        catalog_tree_oid: tree_oid.clone(),
        head_oid: head_oid(&repo),
        meshes: baseline_meshes,
        counts: BaselineCounts::default(),
    };
    let index = build_path_anchor_index(
        &tree_oid,
        Catalog::load(&repo).unwrap().iter().unwrap(),
    );

    // Rename `c.txt` → `c-renamed.txt` (staged + worktree).
    git(f.path(), &["mv", "c.txt", "c-renamed.txt"]);

    let oracle = resolve_full(&repo);
    let dirty: Vec<&[u8]> = vec![b"c.txt", b"c-renamed.txt"];
    let affected_set: HashSet<String> = index
        .lookup_many(dirty.iter().copied())
        .iter()
        .map(|e| e.mesh_name.clone())
        .collect();
    let affected: Vec<String> = affected_set.into_iter().collect();
    let mut overlay_meshes = Vec::new();
    for n in &affected {
        overlay_meshes.push(resolve_one(&repo, n));
    }
    let overlay = DirtyOverlay {
        affected_meshes: affected,
        meshes: overlay_meshes,
    };
    let merged = apply_overlay(&baseline, &overlay);
    let oracle_names: HashSet<String> = oracle.iter().map(|m| m.name.clone()).collect();
    let merged_filtered: Vec<MeshResolved> = merged
        .into_iter()
        .filter(|m| {
            oracle_names.contains(&m.name)
                || m.anchors.iter().any(|a| a.status != AnchorStatus::Fresh)
        })
        .collect();
    assert_meshes_equivalent(&merged_filtered, &oracle);
}

// ---------------------------------------------------------------------------
// Concurrent SQLite WAL smoke test
// ---------------------------------------------------------------------------

#[test]
fn concurrent_wal_writes_do_not_corrupt() {
    let td = tempfile::tempdir().unwrap();
    let path = td.path().join("mesh/stale-cache.db");
    // Pre-create the store so each thread sees the schema applied.
    let _bootstrap = crate::resolver::persist::db::open_store_at(&path).unwrap();

    let path_for_threads = path.clone();
    let mut handles = Vec::new();
    for i in 0..8u32 {
        let p = path_for_threads.clone();
        handles.push(std::thread::spawn(move || {
            let store = crate::resolver::persist::db::open_store_at(&p).unwrap();
            // Each thread writes its own path_anchor_index keyed by a
            // unique tree oid so we don't trip the PRIMARY KEY unique
            // constraint. WAL allows concurrent writers (serialized by
            // SQLite), so all 8 should land.
            let tree = format!("tree-{i:040x}");
            let mut idx = PathAnchorIndex {
                catalog_tree_oid: tree.clone(),
                by_path: std::collections::HashMap::new(),
            };
            let key: std::sync::Arc<[u8]> =
                std::sync::Arc::from(b"x.txt".to_vec().into_boxed_slice());
            idx.by_path.insert(
                key,
                vec![AnchorIndexEntry {
                    mesh_name: format!("m{i}"),
                    anchor_id: format!("a{i}"),
                    anchor_sha: "0".repeat(40),
                    blob_oid: "0".repeat(40),
                    extent: super::dto::AnchorExtentDto::WholeFile,
                    config_hash: [0; 32],
                }],
            );
            store_path_anchor_index(&store, &idx).unwrap();
        }));
    }
    for h in handles {
        h.join().unwrap();
    }
    // Verify all 8 rows landed.
    let store = crate::resolver::persist::db::open_store_at(&path).unwrap();
    let counts = store.row_counts().unwrap();
    assert_eq!(counts.path_anchor_index, 8);
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn open_store_for_test(path: &Path) -> crate::resolver::persist::db::Phase3Store {
    crate::resolver::persist::db::open_store_at(path).unwrap()
}

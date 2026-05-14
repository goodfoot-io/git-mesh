//! Single-ref catalog of all meshes.
//!
//! The catalog is stored at `refs/meshes/v1/catalog` as a git tree whose
//! entries are rkyv-format mesh blobs.  Each entry name is the flattened
//! form of the mesh name: `/` is replaced with `--` and `.mesh` is
//! appended (e.g. `billing/checkout-request-flow` →
//! `billing--checkout-request-flow.mesh`).
//!
//! # Thread safety
//!
//! `Catalog` holds an immutable reference to the repo; mutations are
//! in-memory only until [`write`] is called.

use std::collections::HashMap;
use std::str::FromStr;

use crate::mesh::archive::{entry_to_name, name_to_entry, serialize_mesh};
use crate::types::Mesh;
use crate::{Error, Result};

/// The git ref that stores the catalog tree.
pub const CATALOG_REF: &str = "refs/meshes/v1/catalog";

/// Loads the catalog tree from `refs/meshes/v1/catalog`, maps each `.mesh`
/// entry to its mesh name (unflattening `--` → `/`), and eagerly reads blob
/// bytes into memory for fast lookups.
pub struct Catalog<'repo> {
    /// Borrowed repository handle.
    #[allow(dead_code)]
    repo: &'repo gix::Repository,
    /// OID of the loaded tree, if any.
    #[allow(dead_code)]
    tree_oid: Option<String>,
    /// Map of mesh name → blob oid (hex).
    entries: HashMap<String, String>,
    /// Blob bytes keyed by blob oid.
    blobs: HashMap<String, Vec<u8>>,
}

impl<'repo> Catalog<'repo> {
    /// Load the catalog from `refs/meshes/v1/catalog`.
    ///
    /// Returns an empty catalog if the ref does not exist.
    pub fn load(repo: &'repo gix::Repository) -> Result<Self> {
        let result = repo
            .try_find_reference(CATALOG_REF)
            .map_err(|e| Error::Git(format!("find catalog ref `{CATALOG_REF}`: {e}")))?;

        let (tree_oid, entries, blobs) = match result {
            Some(mut r) => {
                let commit_id = r
                    .peel_to_id()
                    .map_err(|e| Error::Git(format!("peel catalog ref: {e}")))?;
                let commit = repo
                    .find_commit(commit_id)
                    .map_err(|e| Error::Git(format!("find catalog commit: {e}")))?;
                let tree = commit
                    .tree()
                    .map_err(|e| Error::Git(format!("catalog commit tree: {e}")))?;
                let tree_oid = tree.id().detach().to_string();
                Self::load_from_tree(repo, &tree, tree_oid)?
            }
            None => (None, HashMap::new(), HashMap::new()),
        };

        Ok(Catalog {
            repo,
            tree_oid,
            entries,
            blobs,
        })
    }

    /// Load the catalog at a specific tree oid (for historical lookups).
    ///
    /// The oid must point to a tree object.
    pub fn load_at(repo: &'repo gix::Repository, tree_oid: &str) -> Result<Self> {
        let oid = gix::ObjectId::from_str(tree_oid)
            .map_err(|e| Error::Git(format!("parse tree oid `{tree_oid}`: {e}")))?;
        let obj = repo
            .find_object(oid)
            .map_err(|e| Error::Git(format!("find tree object `{tree_oid}`: {e}")))?;
        let tree = obj
            .peel_to_tree()
            .map_err(|e| Error::Git(format!("peel to tree `{tree_oid}`: {e}")))?;
        let tree_oid_str = tree.id().detach().to_string();
        let (_, entries, blobs) = Self::load_from_tree(repo, &tree, tree_oid_str)?;

        Ok(Catalog {
            repo,
            tree_oid: Some(tree_oid.to_string()),
            entries,
            blobs,
        })
    }

    /// Common entry extraction from a `gix::Tree` object handle.
    #[allow(clippy::type_complexity)]
    fn load_from_tree(
        repo: &'repo gix::Repository,
        tree: &gix::Tree<'_>,
        tree_oid: String,
    ) -> Result<(
        Option<String>,
        HashMap<String, String>,
        HashMap<String, Vec<u8>>,
    )> {
        let mut name_oid_pairs: Vec<(String, String)> = Vec::new();
        let traversal = tree
            .traverse()
            .breadthfirst
            .files()
            .map_err(|e| Error::Git(format!("traverse catalog tree: {e}")))?;
        for entry in traversal.into_iter() {
            let path = entry.filepath.to_string();
            let oid = entry.oid.to_string();
            if let Some(mesh_name) = entry_to_name(&path) {
                name_oid_pairs.push((mesh_name, oid));
            }
        }

        let mut entries: HashMap<String, String> = HashMap::new();
        let mut blobs: HashMap<String, Vec<u8>> = HashMap::new();
        for (mesh_name, oid_str) in name_oid_pairs {
            let bytes = crate::git::read_blob_bytes(repo, &oid_str)?;
            entries.insert(mesh_name, oid_str.clone());
            blobs.insert(oid_str, bytes);
        }

        Ok((Some(tree_oid), entries, blobs))
    }

    /// Look up a single mesh by name.
    ///
    /// Returns `Ok(None)` if the name is not in the catalog.
    pub fn lookup(&self, name: &str) -> Result<Option<Mesh>> {
        let blob_oid = match self.entries.get(name) {
            Some(oid) => oid,
            None => return Ok(None),
        };
        let bytes = self
            .blobs
            .get(blob_oid)
            .ok_or_else(|| Error::Git(format!("missing blob `{blob_oid}` for mesh `{name}`")))?;
        let mesh = crate::mesh::archive::deserialize_mesh(bytes)?;
        Ok(Some(mesh))
    }

    /// Iterate over all `(name, Mesh)` pairs in the catalog.
    ///
    /// Returns entries sorted by name for deterministic iteration order
    /// (stable cache keys across otherwise-identical resolutions).
    pub fn iter(&self) -> Result<Vec<(String, Mesh)>> {
        let mut names: Vec<&String> = self.entries.keys().collect();
        names.sort();
        let mut out = Vec::with_capacity(self.entries.len());
        for name in names {
            let blob_oid = &self.entries[name];
            let bytes = self.blobs.get(blob_oid).ok_or_else(|| {
                Error::Git(format!("missing blob `{blob_oid}` for mesh `{name}`"))
            })?;
            let mesh = crate::mesh::archive::deserialize_mesh(bytes)?;
            out.push((name.clone(), mesh));
        }
        Ok(out)
    }

    /// Returns true if the catalog has no entries.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Return all mesh names in the catalog, sorted for deterministic
    /// iteration.
    pub fn names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.entries.keys().cloned().collect();
        names.sort();
        names
    }

    /// Get the blob oid for a mesh entry.
    ///
    /// Returns `None` if the name is not found.
    pub fn entry_oid(&self, name: &str) -> Option<String> {
        self.entries.get(name).cloned()
    }

    /// Insert or replace a mesh entry (in-memory only).
    ///
    /// The mesh is serialised to the rkyv archive format.  Call
    /// [`write`](Self::write) to persist.
    pub fn insert(&mut self, name: &str, mesh: &Mesh) -> Result<()> {
        let bytes = serialize_mesh(mesh);
        let oid = gix::objs::compute_hash(gix::hash::Kind::Sha1, gix::objs::Kind::Blob, &bytes)
            .map_err(|e| Error::Git(format!("hash blob for `{name}`: {e}")))?;
        let oid_str = oid.to_string();
        self.entries.insert(name.to_string(), oid_str.clone());
        self.blobs.insert(oid_str, bytes);
        Ok(())
    }

    /// Remove a mesh entry (in-memory only).
    pub fn remove(&mut self, name: &str) -> Result<()> {
        if let Some(oid) = self.entries.remove(name) {
            self.blobs.remove(&oid);
        }
        Ok(())
    }

    /// Write the catalog tree and return the new tree oid.
    ///
    /// Does **not** update the `refs/meshes/v1/catalog` ref.  The caller
    /// is responsible for the CAS ref update.
    pub fn write(&self, repo: &gix::Repository) -> Result<String> {
        // Sort by flattened entry name for deterministic trees.
        let sorted: Vec<String> = {
            let mut keys: Vec<&String> = self.entries.keys().collect();
            keys.sort_by_key(|a| name_to_entry(a));
            keys.into_iter().cloned().collect()
        };

        let mut tree_entries = Vec::with_capacity(sorted.len());
        for name in &sorted {
            let blob_oid = self.entries.get(name).ok_or_else(|| {
                Error::Git(format!("missing entry for mesh `{name}` in catalog"))
            })?;
            let bytes = self.blobs.get(blob_oid).ok_or_else(|| {
                Error::Git(format!("missing blob `{blob_oid}` for mesh `{name}`"))
            })?;
            let written_oid = repo
                .write_blob(bytes)
                .map_err(|e| Error::Git(format!("write blob for `{name}`: {e}")))?;
            tree_entries.push(gix::objs::tree::Entry {
                mode: gix::objs::tree::EntryKind::Blob.into(),
                filename: name_to_entry(name).into(),
                oid: written_oid.detach(),
            });
        }

        let tree = gix::objs::Tree {
            entries: tree_entries,
        };
        let tree_oid = repo
            .write_object(&tree)
            .map_err(|e| Error::Git(format!("write catalog tree: {e}")))?;
        Ok(tree_oid.detach().to_string())
    }
}

/// Return a `Catalog`-backed view that delegates to the real repo.
///
/// Internal helpers that need a catalog call this — it's the single
/// entrypoint for all mesh reads.
#[allow(dead_code)]
pub(crate) fn open_catalog(repo: &gix::Repository) -> Result<Catalog<'_>> {
    Catalog::load(repo)
}

// ---------------------------------------------------------------------------
// Catalog mutation helpers — used by commit, structural, and compact paths.
// ---------------------------------------------------------------------------

/// Write the catalog tree, create a commit, and CAS-update `refs/meshes/v1/catalog`.
///
/// `expected_ref_oid` is the commit that the catalog ref currently points to
/// (obtained via `resolve_ref_oid_optional_repo` before loading the catalog).
/// Pass `None` for the very first write (ref doesn't exist yet).
///
/// On success returns the new commit OID.
pub(crate) fn commit_catalog(
    repo: &gix::Repository,
    catalog: &Catalog<'_>,
    message: &str,
    expected_ref_oid: Option<&str>,
) -> Result<String> {
    let tree_oid = catalog.write(repo)?;
    let parents: Vec<String> = expected_ref_oid
        .map(|o| vec![o.to_string()])
        .unwrap_or_default();
    let new_commit = crate::git::create_commit(repo, &tree_oid, message, &parents)?;

    let update = match expected_ref_oid {
        Some(old) => crate::git::RefUpdate::Update {
            name: CATALOG_REF.to_string(),
            new_oid: new_commit.clone(),
            expected_old_oid: old.to_string(),
        },
        None => crate::git::RefUpdate::Create {
            name: CATALOG_REF.to_string(),
            new_oid: new_commit.clone(),
        },
    };
    crate::git::ensure_log_all_ref_updates_always(repo)?;
    crate::git::apply_ref_transaction_repo(repo, &[update])?;
    Ok(new_commit)
}

/// Build a `Mesh` value from its components (used when writing via catalog).
pub(crate) fn build_mesh(
    name: &str,
    message: &str,
    anchors_v2: &[(String, crate::types::Anchor)],
    config: &crate::types::MeshConfig,
) -> crate::types::Mesh {
    let anchors = anchors_v2.iter().map(|(id, _)| id.clone()).collect();
    crate::types::Mesh {
        name: name.to_string(),
        anchors,
        anchors_v2: anchors_v2.to_vec(),
        message: message.to_string(),
        config: *config,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mesh::archive::{name_to_entry, serialize_mesh};
    use crate::types::{Anchor, AnchorExtent, CopyDetection, Mesh, MeshConfig};
    use std::process::Command;

    fn init_repo() -> (tempfile::TempDir, gix::Repository) {
        let td = tempfile::tempdir().unwrap();
        let dir = td.path();
        Command::new("git")
            .args(["init", "--initial-branch=main"])
            .current_dir(dir)
            .output()
            .unwrap();
        Command::new("git")
            .args(["config", "user.email", "t@t"])
            .current_dir(dir)
            .output()
            .unwrap();
        Command::new("git")
            .args(["config", "user.name", "t"])
            .current_dir(dir)
            .output()
            .unwrap();
        Command::new("git")
            .args(["config", "commit.gpgsign", "false"])
            .current_dir(dir)
            .output()
            .unwrap();
        std::fs::write(dir.join("file.txt"), "hello\n").unwrap();
        Command::new("git")
            .args(["add", "."])
            .current_dir(dir)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "init"])
            .current_dir(dir)
            .output()
            .unwrap();
        let repo = gix::open(dir).unwrap();
        (td, repo)
    }

    fn sample_mesh(name: &str) -> Mesh {
        Mesh {
            name: name.into(),
            anchors: vec!["a1".into()],
            anchors_v2: vec![(
                "a1".into(),
                Anchor {
                    anchor_sha: "abc123".into(),
                    created_at: "2025-01-01T00:00:00Z".into(),
                    path: "src/main.rs".into(),
                    extent: AnchorExtent::WholeFile,
                    blob: "def456".into(),
                },
            )],
            message: "Test mesh".into(),
            config: MeshConfig {
                copy_detection: CopyDetection::SameCommit,
                ignore_whitespace: false,
                follow_moves: false,
            },
        }
    }

    /// Seed a catalog tree with the given meshes and return the tree oid.
    fn seed_catalog(
        _td: &tempfile::TempDir,
        repo: &gix::Repository,
        meshes: &[Mesh],
    ) -> String {
        let mut entries: Vec<(String, Vec<u8>)> = meshes
            .iter()
            .map(|m| {
                let entry = name_to_entry(&m.name);
                let bytes = serialize_mesh(m);
                (entry, bytes)
            })
            .collect();
        entries.sort_by(|a, b| a.0.cmp(&b.0));

        let tree_entries: Vec<gix::objs::tree::Entry> = entries
            .into_iter()
            .map(|(name, blob_bytes)| {
                let oid = repo.write_blob(&blob_bytes).unwrap().detach();
                gix::objs::tree::Entry {
                    mode: gix::objs::tree::EntryKind::Blob.into(),
                    filename: name.into(),
                    oid,
                }
            })
            .collect();

        let tree = gix::objs::Tree {
            entries: tree_entries,
        };
        repo.write_object(&tree)
            .unwrap()
            .detach()
            .to_string()
    }

    #[test]
    fn load_empty_repo() {
        let (_td, repo) = init_repo();
        let catalog = Catalog::load(&repo).unwrap();
        assert!(catalog.names().is_empty());
        assert!(catalog.iter().unwrap().is_empty());
    }

    #[test]
    fn insert_and_lookup_roundtrip() {
        let (_td, repo) = init_repo();
        let mut catalog = Catalog::load(&repo).unwrap();
        let mesh = sample_mesh("test-mesh");
        catalog.insert("test-mesh", &mesh).unwrap();
        let found = catalog.lookup("test-mesh").unwrap();
        assert_eq!(found, Some(mesh));
    }

    #[test]
    fn lookup_missing() {
        let (_td, repo) = init_repo();
        let catalog = Catalog::load(&repo).unwrap();
        let found = catalog.lookup("nonexistent").unwrap();
        assert_eq!(found, None);
    }

    #[test]
    fn insert_then_remove() {
        let (_td, repo) = init_repo();
        let mut catalog = Catalog::load(&repo).unwrap();
        let mesh = sample_mesh("remove-me");
        catalog.insert("remove-me", &mesh).unwrap();
        assert!(catalog.lookup("remove-me").unwrap().is_some());
        catalog.remove("remove-me").unwrap();
        assert!(catalog.lookup("remove-me").unwrap().is_none());
    }

    #[test]
    fn iter_and_names() {
        let (_td, repo) = init_repo();
        let mut catalog = Catalog::load(&repo).unwrap();
        let m1 = sample_mesh("alpha");
        let m2 = sample_mesh("beta");
        catalog.insert("alpha", &m1).unwrap();
        catalog.insert("beta", &m2).unwrap();

        let mut names = catalog.names();
        names.sort();
        assert_eq!(names, vec!["alpha", "beta"]);

        let pairs = catalog.iter().unwrap();
        assert_eq!(pairs.len(), 2);
        let found_names: Vec<String> = pairs.into_iter().map(|(n, _)| n).collect();
        assert!(found_names.contains(&"alpha".into()));
        assert!(found_names.contains(&"beta".into()));
    }

    #[test]
    fn mesh_name_flattening() {
        let (_td, repo) = init_repo();
        let mut catalog = Catalog::load(&repo).unwrap();
        let mesh = sample_mesh("billing/checkout-request-flow");
        catalog
            .insert("billing/checkout-request-flow", &mesh)
            .unwrap();

        let found = catalog
            .lookup("billing/checkout-request-flow")
            .unwrap();
        assert_eq!(found, Some(mesh));
    }

    #[test]
    fn write_and_reload() {
        let (_td, repo) = init_repo();
        let mut catalog = Catalog::load(&repo).unwrap();
        let mesh = sample_mesh("persist-me");
        catalog.insert("persist-me", &mesh).unwrap();

        // Write catalog tree and reload from the resulting tree oid.
        let tree_oid = catalog.write(&repo).unwrap();
        let reloaded = Catalog::load_at(&repo, &tree_oid).unwrap();
        let found = reloaded.lookup("persist-me").unwrap();
        assert_eq!(found, Some(mesh));
    }

    #[test]
    fn entry_oid_returns_blob_oid() {
        let (_td, repo) = init_repo();
        let mut catalog = Catalog::load(&repo).unwrap();
        let mesh = sample_mesh("oid-test");
        catalog.insert("oid-test", &mesh).unwrap();

        let oid = catalog.entry_oid("oid-test");
        assert!(oid.is_some());
        assert_eq!(oid.unwrap().len(), 40); // SHA-1 hex

        // Missing name returns None.
        assert!(catalog.entry_oid("nope").is_none());
    }

    #[test]
    fn load_at_with_seeded_catalog() {
        let (td, repo) = init_repo();
        let meshes = vec![sample_mesh("alpha"), sample_mesh("bravo")];
        let tree_oid = seed_catalog(&td, &repo, &meshes);

        let catalog = Catalog::load_at(&repo, &tree_oid).unwrap();
        let mut names = catalog.names();
        names.sort();
        assert_eq!(names, vec!["alpha", "bravo"]);
    }
}

//! rkyv 0.8 archive format for mesh storage.
//!
//! Mirrors the [`crate::types::Mesh`] struct family with rkyv derives for
//! zero-copy serialization.  Every blob is prefixed with an 8-byte header
//! (`[FORMAT_VERSION: u8, 0u8; 7]`) to satisfy rkyv's alignment without
//! storing unaligned pointers.  The rkyv payload starts at byte offset 8.
//!
//! # Wire format
//!
//! ```text
//! | 0: FORMAT_VERSION  |  1-7: zero padding  |  rkyv(ArchivedMesh)  |
//! |<-------- 8-byte header -------->|<------- variable length ------>|
//! ```

use rkyv::{Archive, Deserialize, Serialize};
use rancor;

use crate::types::{Anchor, AnchorExtent, CopyDetection, Mesh, MeshConfig};
use crate::{Error, Result};

/// Current on-disk format version.  Bump on incompatible changes.
pub const FORMAT_VERSION: u8 = 0;

/// Total header length in bytes (version byte + 7 padding bytes for rkyv
/// alignment).
pub const HEADER_LEN: usize = 8;

// ---------------------------------------------------------------------------
// rkyv mirror types
// ---------------------------------------------------------------------------

/// rkyv-serialisable mirror of [`crate::types::AnchorExtent`].
#[derive(Archive, Serialize, Deserialize, Debug, PartialEq)]
pub enum AnchorExtentArchive {
    WholeFile,
    LineRange { start: u32, end: u32 },
}

/// rkyv-serialisable mirror of [`crate::types::Anchor`].
#[derive(Archive, Serialize, Deserialize, Debug, PartialEq)]
pub struct AnchorEntryArchive {
    pub anchor_sha: String,
    pub created_at: String,
    pub path: String,
    pub extent: AnchorExtentArchive,
    pub blob: String,
}

/// rkyv-serialisable mirror of [`crate::types::CopyDetection`].
#[derive(Archive, Serialize, Deserialize, Debug, PartialEq)]
pub enum CopyDetectionArchive {
    Off,
    SameCommit,
    AnyFileInCommit,
    AnyFileInRepo,
}

/// rkyv-serialisable mirror of [`crate::types::MeshConfig`].
#[derive(Archive, Serialize, Deserialize, Debug, PartialEq)]
pub struct MeshConfigArchive {
    pub copy_detection: CopyDetectionArchive,
    pub ignore_whitespace: bool,
    pub follow_moves: bool,
}

/// rkyv-serialisable mirror of [`crate::types::Mesh`].
///
/// Omits the compatibility `anchors` field (`Vec<String>`) — it is
/// reconstructed from `anchors_v2` on deserialisation.
#[derive(Archive, Serialize, Deserialize, Debug, PartialEq)]
pub struct MeshArchive {
    pub name: String,
    pub anchors_v2: Vec<(String, AnchorEntryArchive)>,
    pub message: String,
    pub config: MeshConfigArchive,
}

// ---------------------------------------------------------------------------
// Conversions: archive types ↔ crate::types
// ---------------------------------------------------------------------------

impl From<AnchorExtent> for AnchorExtentArchive {
    fn from(e: AnchorExtent) -> Self {
        match e {
            AnchorExtent::WholeFile => AnchorExtentArchive::WholeFile,
            AnchorExtent::LineRange { start, end } => {
                AnchorExtentArchive::LineRange { start, end }
            }
        }
    }
}

impl From<AnchorExtentArchive> for AnchorExtent {
    fn from(e: AnchorExtentArchive) -> Self {
        match e {
            AnchorExtentArchive::WholeFile => AnchorExtent::WholeFile,
            AnchorExtentArchive::LineRange { start, end } => {
                AnchorExtent::LineRange { start, end }
            }
        }
    }
}

impl From<&Anchor> for AnchorEntryArchive {
    fn from(a: &Anchor) -> Self {
        AnchorEntryArchive {
            anchor_sha: a.anchor_sha.clone(),
            created_at: a.created_at.clone(),
            path: a.path.clone(),
            extent: AnchorExtentArchive::from(a.extent),
            blob: a.blob.clone(),
        }
    }
}

impl From<AnchorEntryArchive> for Anchor {
    fn from(a: AnchorEntryArchive) -> Self {
        Anchor {
            anchor_sha: a.anchor_sha,
            created_at: a.created_at,
            path: a.path,
            extent: AnchorExtent::from(a.extent),
            blob: a.blob,
        }
    }
}

impl From<CopyDetection> for CopyDetectionArchive {
    fn from(c: CopyDetection) -> Self {
        match c {
            CopyDetection::Off => CopyDetectionArchive::Off,
            CopyDetection::SameCommit => CopyDetectionArchive::SameCommit,
            CopyDetection::AnyFileInCommit => CopyDetectionArchive::AnyFileInCommit,
            CopyDetection::AnyFileInRepo => CopyDetectionArchive::AnyFileInRepo,
        }
    }
}

impl From<CopyDetectionArchive> for CopyDetection {
    fn from(c: CopyDetectionArchive) -> Self {
        match c {
            CopyDetectionArchive::Off => CopyDetection::Off,
            CopyDetectionArchive::SameCommit => CopyDetection::SameCommit,
            CopyDetectionArchive::AnyFileInCommit => CopyDetection::AnyFileInCommit,
            CopyDetectionArchive::AnyFileInRepo => CopyDetection::AnyFileInRepo,
        }
    }
}

impl From<&MeshConfig> for MeshConfigArchive {
    fn from(c: &MeshConfig) -> Self {
        MeshConfigArchive {
            copy_detection: CopyDetectionArchive::from(c.copy_detection),
            ignore_whitespace: c.ignore_whitespace,
            follow_moves: c.follow_moves,
        }
    }
}

impl From<MeshConfigArchive> for MeshConfig {
    fn from(c: MeshConfigArchive) -> Self {
        MeshConfig {
            copy_detection: CopyDetection::from(c.copy_detection),
            ignore_whitespace: c.ignore_whitespace,
            follow_moves: c.follow_moves,
        }
    }
}

impl From<&Mesh> for MeshArchive {
    fn from(m: &Mesh) -> Self {
        MeshArchive {
            name: m.name.clone(),
            anchors_v2: m
                .anchors_v2
                .iter()
                .map(|(id, a)| (id.clone(), AnchorEntryArchive::from(a)))
                .collect(),
            message: m.message.clone(),
            config: MeshConfigArchive::from(&m.config),
        }
    }
}

impl From<MeshArchive> for Mesh {
    fn from(ma: MeshArchive) -> Self {
        let anchors = ma.anchors_v2.iter().map(|(id, _)| id.clone()).collect();
        Mesh {
            name: ma.name,
            anchors,
            anchors_v2: ma
                .anchors_v2
                .into_iter()
                .map(|(id, a)| (id, Anchor::from(a)))
                .collect(),
            message: ma.message,
            config: MeshConfig::from(ma.config),
        }
    }
}

// ---------------------------------------------------------------------------
// Serialisation helpers
// ---------------------------------------------------------------------------

/// Serialise `mesh` into the on-disk blob format (8-byte header + rkyv
/// payload).
pub fn serialize_mesh(mesh: &Mesh) -> Vec<u8> {
    let archive: MeshArchive = MeshArchive::from(mesh);
    let mut header = [0u8; HEADER_LEN];
    header[0] = FORMAT_VERSION;
    let aligned = rkyv::to_bytes::<rancor::Error>(&archive)
        .expect("rkyv serialisation should not fail for valid Mesh")
        .to_vec();
    let mut bytes = header.to_vec();
    bytes.extend_from_slice(&aligned);
    bytes
}

/// Deserialise a `Mesh` from an on-disk blob created by [`serialize_mesh`].
///
/// Returns `Err(Error::Parse)` on format-version mismatch or corrupt data.
pub fn deserialize_mesh(bytes: &[u8]) -> Result<Mesh> {
    if bytes.len() < HEADER_LEN {
        return Err(Error::Parse(
            format!(
                "mesh blob too short: {} bytes (need at least {HEADER_LEN})",
                bytes.len()
            ),
        ));
    }
    if bytes[0] != FORMAT_VERSION {
        return Err(Error::Parse(
            format!(
                "format version mismatch: expected {FORMAT_VERSION}, got {}",
                bytes[0],
            ),
        ));
    }
    let archived = rkyv::access::<ArchivedMeshArchive, rancor::Error>(&bytes[HEADER_LEN..])
        .map_err(|e| Error::Parse(format!("rkyv access error: {e}")))?;
    let native: MeshArchive =
        rkyv::deserialize::<MeshArchive, rancor::Error>(archived)
            .map_err(|e| Error::Parse(format!("rkyv deserialize error: {e}")))?;
    Ok(Mesh::from(native))
}

// ---------------------------------------------------------------------------
// Name flattening helpers
// ---------------------------------------------------------------------------

/// Convert a mesh name (e.g. `billing/checkout-request-flow`) to its
/// flattened tree entry name (e.g. `billing--checkout-request-flow.mesh`).
pub fn name_to_entry(name: &str) -> String {
    format!("{}.mesh", name.replace('/', "--"))
}

/// Recover a mesh name from a flattened tree entry name.
///
/// Returns `None` if the entry does not end with `.mesh`.
pub fn entry_to_name(entry: &str) -> Option<String> {
    let stem = entry.strip_suffix(".mesh")?;
    Some(stem.replace("--", "/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_mesh() -> Mesh {
        Mesh {
            name: "billing/checkout-request-flow".into(),
            anchors: vec!["a1".into()],
            anchors_v2: vec![(
                "a1".into(),
                Anchor {
                    anchor_sha: "abc123".into(),
                    created_at: "2025-01-01T00:00:00Z".into(),
                    path: "src/checkout.rs".into(),
                    extent: AnchorExtent::LineRange { start: 10, end: 20 },
                    blob: "def456".into(),
                },
            )],
            message: "Checkout request flow".into(),
            config: MeshConfig {
                copy_detection: CopyDetection::AnyFileInRepo,
                ignore_whitespace: true,
                follow_moves: true,
            },
        }
    }

    #[test]
    fn serialize_deserialize_roundtrip() {
        let mesh = sample_mesh();
        let bytes = serialize_mesh(&mesh);
        let recovered = deserialize_mesh(&bytes).unwrap();
        assert_eq!(recovered, mesh);
    }

    #[test]
    fn serialize_deserialize_minimal_mesh() {
        let mesh = Mesh {
            name: "simple".into(),
            anchors: vec![],
            anchors_v2: vec![],
            message: "A simple mesh".into(),
            config: MeshConfig {
                copy_detection: CopyDetection::SameCommit,
                ignore_whitespace: false,
                follow_moves: false,
            },
        };
        let bytes = serialize_mesh(&mesh);
        let recovered = deserialize_mesh(&bytes).unwrap();
        assert_eq!(recovered, mesh);
    }

    #[test]
    fn format_version_mismatch() {
        let mesh = sample_mesh();
        let mut bytes = serialize_mesh(&mesh);
        // Corrupt the version byte.
        bytes[0] = 0xFF;
        let result = deserialize_mesh(&bytes);
        assert!(result.is_err());
        let err = result.unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("format version"), "msg={msg}");
    }

    #[test]
    fn corrupt_bytes() {
        let bytes = vec![0u8; 64];
        let result = deserialize_mesh(&bytes);
        assert!(result.is_err());
    }

    #[test]
    fn too_short_blob() {
        let bytes = vec![0u8; 3];
        let result = deserialize_mesh(&bytes);
        assert!(result.is_err());
    }

    #[test]
    fn name_to_entry_flattens_slashes() {
        assert_eq!(
            name_to_entry("billing/checkout-request-flow"),
            "billing--checkout-request-flow.mesh"
        );
    }

    #[test]
    fn name_to_entry_no_slash() {
        assert_eq!(name_to_entry("simple"), "simple.mesh");
    }

    #[test]
    fn entry_to_name_reverses_flattening() {
        assert_eq!(
            entry_to_name("billing--checkout-request-flow.mesh"),
            Some("billing/checkout-request-flow".into())
        );
    }

    #[test]
    fn entry_to_name_no_dot_mesh() {
        assert_eq!(entry_to_name("foo.bar"), None);
    }
}

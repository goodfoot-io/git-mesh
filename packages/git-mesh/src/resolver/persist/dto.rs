//! Serializable mirrors of the runtime resolver types.
//!
//! The runtime `MeshResolved` / `AnchorResolved` / `AnchorLocation` types
//! embed `gix::ObjectId`, `PathBuf`, and other non-`serde` shapes. To
//! persist them across invocations we round-trip through pure-bytes DTOs
//! that derive `Serialize` + `Deserialize` over `String` / `Vec<u8>`.
//! Conversion is total: every runtime instance has exactly one DTO
//! representation and vice versa.
//!
//! Payload framing: every Phase 3 SQLite payload is
//! `bincode::serialize`(DTO). A `format_version: u8` field is the first
//! field of every top-level DTO so a future shape change can be
//! detected as a deserialization failure (and reported as a miss). The
//! `KEY_SALT` namespace bump is the primary invalidation mechanism;
//! `format_version` is a belt-and-braces second line of defense.

use crate::types::{
    AnchorExtent, AnchorLocation, AnchorResolved, AnchorStatus, DriftLocus, DriftSource,
    MeshResolved, StagedOpRef, UnavailableReason,
};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::str::FromStr;

pub(crate) const FORMAT_VERSION: u8 = 1;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) enum AnchorExtentDto {
    WholeFile,
    LineRange { start: u32, end: u32 },
}

impl From<AnchorExtent> for AnchorExtentDto {
    fn from(e: AnchorExtent) -> Self {
        match e {
            AnchorExtent::WholeFile => AnchorExtentDto::WholeFile,
            AnchorExtent::LineRange { start, end } => AnchorExtentDto::LineRange { start, end },
        }
    }
}

impl From<AnchorExtentDto> for AnchorExtent {
    fn from(e: AnchorExtentDto) -> Self {
        match e {
            AnchorExtentDto::WholeFile => AnchorExtent::WholeFile,
            AnchorExtentDto::LineRange { start, end } => AnchorExtent::LineRange { start, end },
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct AnchorLocationDto {
    pub(crate) path: String,
    pub(crate) extent: AnchorExtentDto,
    /// Hex object id when present.
    pub(crate) blob: Option<String>,
}

impl From<&AnchorLocation> for AnchorLocationDto {
    fn from(l: &AnchorLocation) -> Self {
        Self {
            path: l.path.to_string_lossy().into_owned(),
            extent: l.extent.into(),
            blob: l.blob.map(|b| b.to_string()),
        }
    }
}

impl TryFrom<AnchorLocationDto> for AnchorLocation {
    type Error = crate::Error;
    fn try_from(dto: AnchorLocationDto) -> Result<Self, Self::Error> {
        let blob = match dto.blob {
            Some(s) => Some(gix::ObjectId::from_str(&s).map_err(|e| {
                crate::Error::Git(format!("phase3 dto: parse blob oid `{s}`: {e}"))
            })?),
            None => None,
        };
        Ok(AnchorLocation {
            path: PathBuf::from(dto.path),
            extent: dto.extent.into(),
            blob,
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) enum UnavailableReasonDto {
    LfsNotFetched,
    LfsNotInstalled,
    PromisorMissing,
    SparseExcluded,
    FilterFailed { filter: String },
    IoError { message: String },
}

impl From<&UnavailableReason> for UnavailableReasonDto {
    fn from(r: &UnavailableReason) -> Self {
        match r {
            UnavailableReason::LfsNotFetched => UnavailableReasonDto::LfsNotFetched,
            UnavailableReason::LfsNotInstalled => UnavailableReasonDto::LfsNotInstalled,
            UnavailableReason::PromisorMissing => UnavailableReasonDto::PromisorMissing,
            UnavailableReason::SparseExcluded => UnavailableReasonDto::SparseExcluded,
            UnavailableReason::FilterFailed { filter } => UnavailableReasonDto::FilterFailed {
                filter: filter.clone(),
            },
            UnavailableReason::IoError { message } => UnavailableReasonDto::IoError {
                message: message.clone(),
            },
        }
    }
}

impl From<UnavailableReasonDto> for UnavailableReason {
    fn from(r: UnavailableReasonDto) -> Self {
        match r {
            UnavailableReasonDto::LfsNotFetched => UnavailableReason::LfsNotFetched,
            UnavailableReasonDto::LfsNotInstalled => UnavailableReason::LfsNotInstalled,
            UnavailableReasonDto::PromisorMissing => UnavailableReason::PromisorMissing,
            UnavailableReasonDto::SparseExcluded => UnavailableReason::SparseExcluded,
            UnavailableReasonDto::FilterFailed { filter } => {
                UnavailableReason::FilterFailed { filter }
            }
            UnavailableReasonDto::IoError { message } => UnavailableReason::IoError { message },
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) enum AnchorStatusDto {
    Fresh,
    Moved,
    Changed,
    Orphaned,
    MergeConflict,
    Submodule,
    ContentUnavailable(UnavailableReasonDto),
}

impl From<&AnchorStatus> for AnchorStatusDto {
    fn from(s: &AnchorStatus) -> Self {
        match s {
            AnchorStatus::Fresh => AnchorStatusDto::Fresh,
            AnchorStatus::Moved => AnchorStatusDto::Moved,
            AnchorStatus::Changed => AnchorStatusDto::Changed,
            AnchorStatus::Orphaned => AnchorStatusDto::Orphaned,
            AnchorStatus::MergeConflict => AnchorStatusDto::MergeConflict,
            AnchorStatus::Submodule => AnchorStatusDto::Submodule,
            AnchorStatus::ContentUnavailable(r) => AnchorStatusDto::ContentUnavailable(r.into()),
        }
    }
}

impl From<AnchorStatusDto> for AnchorStatus {
    fn from(s: AnchorStatusDto) -> Self {
        match s {
            AnchorStatusDto::Fresh => AnchorStatus::Fresh,
            AnchorStatusDto::Moved => AnchorStatus::Moved,
            AnchorStatusDto::Changed => AnchorStatus::Changed,
            AnchorStatusDto::Orphaned => AnchorStatus::Orphaned,
            AnchorStatusDto::MergeConflict => AnchorStatus::MergeConflict,
            AnchorStatusDto::Submodule => AnchorStatus::Submodule,
            AnchorStatusDto::ContentUnavailable(r) => AnchorStatus::ContentUnavailable(r.into()),
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) enum DriftSourceDto {
    Head,
    Index,
    Worktree,
}

impl From<DriftSource> for DriftSourceDto {
    fn from(s: DriftSource) -> Self {
        match s {
            DriftSource::Head => DriftSourceDto::Head,
            DriftSource::Index => DriftSourceDto::Index,
            DriftSource::Worktree => DriftSourceDto::Worktree,
        }
    }
}

impl From<DriftSourceDto> for DriftSource {
    fn from(s: DriftSourceDto) -> Self {
        match s {
            DriftSourceDto::Head => DriftSource::Head,
            DriftSourceDto::Index => DriftSource::Index,
            DriftSourceDto::Worktree => DriftSource::Worktree,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) enum DriftLocusDto {
    ChangedAt(String),
    OrphanedAt(String),
    Unreachable,
}

impl From<DriftLocus> for DriftLocusDto {
    fn from(l: DriftLocus) -> Self {
        match l {
            DriftLocus::ChangedAt(oid) => DriftLocusDto::ChangedAt(oid.to_string()),
            DriftLocus::OrphanedAt(oid) => DriftLocusDto::OrphanedAt(oid.to_string()),
            DriftLocus::Unreachable => DriftLocusDto::Unreachable,
        }
    }
}

impl TryFrom<DriftLocusDto> for DriftLocus {
    type Error = crate::Error;
    fn try_from(dto: DriftLocusDto) -> Result<Self, Self::Error> {
        Ok(match dto {
            DriftLocusDto::ChangedAt(s) => DriftLocus::ChangedAt(
                gix::ObjectId::from_str(&s)
                    .map_err(|e| crate::Error::Git(format!("phase3 dto: parse locus oid: {e}")))?,
            ),
            DriftLocusDto::OrphanedAt(s) => DriftLocus::OrphanedAt(
                gix::ObjectId::from_str(&s)
                    .map_err(|e| crate::Error::Git(format!("phase3 dto: parse locus oid: {e}")))?,
            ),
            DriftLocusDto::Unreachable => DriftLocus::Unreachable,
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct StagedOpRefDto {
    pub(crate) mesh: String,
    pub(crate) index: u64,
}

impl From<&StagedOpRef> for StagedOpRefDto {
    fn from(r: &StagedOpRef) -> Self {
        Self {
            mesh: r.mesh.clone(),
            index: r.index as u64,
        }
    }
}

impl From<StagedOpRefDto> for StagedOpRef {
    fn from(r: StagedOpRefDto) -> Self {
        Self {
            mesh: r.mesh,
            index: r.index as usize,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct AnchorResolvedDto {
    pub(crate) anchor_id: String,
    pub(crate) anchor_sha: String,
    pub(crate) anchored: AnchorLocationDto,
    pub(crate) current: Option<AnchorLocationDto>,
    pub(crate) status: AnchorStatusDto,
    pub(crate) source: Option<DriftSourceDto>,
    pub(crate) layer_sources: Vec<DriftSourceDto>,
    pub(crate) acknowledged_by: Option<StagedOpRefDto>,
    pub(crate) locus: Option<DriftLocusDto>,
}

impl From<&AnchorResolved> for AnchorResolvedDto {
    fn from(a: &AnchorResolved) -> Self {
        Self {
            anchor_id: a.anchor_id.clone(),
            anchor_sha: a.anchor_sha.clone(),
            anchored: (&a.anchored).into(),
            current: a.current.as_ref().map(Into::into),
            status: (&a.status).into(),
            source: a.source.map(Into::into),
            layer_sources: a.layer_sources.iter().copied().map(Into::into).collect(),
            acknowledged_by: a.acknowledged_by.as_ref().map(Into::into),
            locus: a.locus.map(Into::into),
        }
    }
}

impl TryFrom<AnchorResolvedDto> for AnchorResolved {
    type Error = crate::Error;
    fn try_from(d: AnchorResolvedDto) -> Result<Self, Self::Error> {
        let current = match d.current {
            Some(c) => Some(c.try_into()?),
            None => None,
        };
        let locus = match d.locus {
            Some(l) => Some(l.try_into()?),
            None => None,
        };
        Ok(AnchorResolved {
            anchor_id: d.anchor_id,
            anchor_sha: d.anchor_sha,
            anchored: d.anchored.try_into()?,
            current,
            status: d.status.into(),
            source: d.source.map(Into::into),
            layer_sources: d.layer_sources.into_iter().map(Into::into).collect(),
            acknowledged_by: d.acknowledged_by.map(Into::into),
            locus,
        })
    }
}

/// Persisted shape of `MeshResolved`. `pending` is intentionally always
/// empty in the persisted form: the persistent baseline captures the
/// committed (HEAD-only) resolution, and pending findings derive from
/// the live `.git/mesh/staging/` directory at render time.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct MeshResolvedDto {
    pub(crate) format_version: u8,
    pub(crate) name: String,
    pub(crate) message: String,
    pub(crate) anchors: Vec<AnchorResolvedDto>,
    pub(crate) follow_moves: bool,
}

impl From<&MeshResolved> for MeshResolvedDto {
    fn from(m: &MeshResolved) -> Self {
        Self {
            format_version: FORMAT_VERSION,
            name: m.name.clone(),
            message: m.message.clone(),
            anchors: m.anchors.iter().map(Into::into).collect(),
            follow_moves: m.follow_moves,
        }
    }
}

impl TryFrom<MeshResolvedDto> for MeshResolved {
    type Error = crate::Error;
    fn try_from(d: MeshResolvedDto) -> Result<Self, Self::Error> {
        if d.format_version != FORMAT_VERSION {
            return Err(crate::Error::Git(format!(
                "phase3 dto: format_version {} != expected {}",
                d.format_version, FORMAT_VERSION
            )));
        }
        let mut anchors = Vec::with_capacity(d.anchors.len());
        for a in d.anchors {
            anchors.push(a.try_into()?);
        }
        Ok(MeshResolved {
            name: d.name,
            message: d.message,
            anchors,
            pending: Vec::new(),
            follow_moves: d.follow_moves,
        })
    }
}

//! Suggestion types for the n-ary mesh recommendation pipeline.
//!
//! A `Suggestion` is the output unit of a `Detector` — a scored,
//! confidence-banded recommendation that a set of files/ranges belong
//! together in a mesh. This is distinct from a `Candidate` (the
//! pairwise, session-scoped advice type) in that suggestions are mined
//! from history and scored across n participants.

use serde::{Serialize, Serializer, ser::SerializeStruct};

use crate::advice::candidates::{MeshRange, MeshRangeStatus};

// ── Confidence band ──────────────────────────────────────────────────────────

/// Four-level confidence band matching the v4 `BANDS` constant
/// (`['low', 'medium', 'high', 'high+']`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ConfidenceBand {
    Low,
    Medium,
    High,
    /// `high+` — highest confidence; serializes to `"high+"`.
    #[serde(rename = "high+")]
    HighPlus,
}

// ── Viability ────────────────────────────────────────────────────────────────

/// Whether a suggestion is ready to present, suppressed, or superseded.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Viability {
    /// Ready to surface to the developer.
    Ready,
    /// Filtered out (e.g. same-file dominance, sprawl distance).
    Suppressed,
    /// Absorbed into a larger clique.
    Superseded,
}

// ── Score breakdown ──────────────────────────────────────────────────────────

/// Per-component score breakdown for diagnostics and testing.
#[derive(Clone, Debug, Serialize)]
pub struct ScoreBreakdown {
    /// Shared-identifier weight (IDF-saturated co-occurrence score).
    pub shared_id: f64,
    /// Co-edit frequency score (history channel).
    pub co_edit: f64,
    /// Trigram similarity score.
    pub trigram: f64,
    /// Final composite score (weighted sum of components).
    pub composite: f64,
}

// ── Suggestion ───────────────────────────────────────────────────────────────

/// A scored n-ary mesh suggestion produced by a `Detector`.
///
/// Serializes to a JSON object whose first field is `"v": 1` — the
/// schema version witness. Callers that deserialize suggestions should
/// reject objects where `v != 1`.
#[derive(Clone, Debug)]
pub struct Suggestion {
    /// Schema version. Always `1` for this type. Must appear first in
    /// the serialized form.
    pub version: u8,

    /// Confidence band for this suggestion.
    pub band: ConfidenceBand,

    /// Whether this suggestion is ready to surface.
    pub viability: Viability,

    /// Score breakdown for diagnostics.
    pub score: ScoreBreakdown,

    /// The set of ranges that are recommended to form a new mesh.
    /// Each `MeshRange` carries path, line extent, and (if already
    /// anchored) mesh name and why.
    pub participants: Vec<MeshRange>,

    /// Human-readable label for the suggested mesh relationship.
    /// Empty string when the detector has no label to offer.
    pub label: String,
}

impl Serialize for Suggestion {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut s = serializer.serialize_struct("Suggestion", 6)?;
        s.serialize_field("v", &self.version)?;
        s.serialize_field("band", &self.band)?;
        s.serialize_field("viability", &self.viability)?;
        s.serialize_field("score", &self.score)?;
        // Serialize participants manually since MeshRange does not impl Serialize.
        let participants: Vec<_> = self.participants.iter().map(MeshRangeView).collect();
        s.serialize_field("participants", &participants)?;
        s.serialize_field("label", &self.label)?;
        s.end()
    }
}

/// Serialization view for `MeshRange`.
struct MeshRangeView<'a>(&'a MeshRange);

impl Serialize for MeshRangeView<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let r = self.0;
        let mut s = serializer.serialize_struct("MeshRange", 7)?;
        s.serialize_field("name", &r.name)?;
        s.serialize_field("why", &r.why)?;
        s.serialize_field("path", &r.path.to_string_lossy().as_ref())?;
        s.serialize_field("start", &r.start)?;
        s.serialize_field("end", &r.end)?;
        s.serialize_field("whole", &r.whole)?;
        let status = match r.status {
            MeshRangeStatus::Stable => "stable",
            MeshRangeStatus::Changed => "changed",
            MeshRangeStatus::Moved => "moved",
            MeshRangeStatus::Terminal => "terminal",
        };
        s.serialize_field("status", status)?;
        s.end()
    }
}

impl Suggestion {
    /// Construct a new suggestion with `version` pre-populated to `1`.
    pub fn new(
        band: ConfidenceBand,
        viability: Viability,
        score: ScoreBreakdown,
        participants: Vec<MeshRange>,
        label: String,
    ) -> Self {
        Self {
            version: 1,
            band,
            viability,
            score,
            participants,
            label,
        }
    }
}

//! Suggestion types for the n-ary mesh recommendation pipeline.
//!
//! A `Suggestion` is the output unit of a `Detector` — a scored,
//! confidence-banded recommendation that a set of files/anchors belong
//! together in a mesh. This is distinct from a `Candidate` (the
//! pairwise, session-scoped advice type) in that suggestions are mined
//! from history and scored across n participants.

use serde::{Deserialize, Serialize, Serializer, ser::SerializeStruct};

use crate::advice::candidates::{MeshAnchor, MeshAnchorStatus};

// ── DriftMeta ────────────────────────────────────────────────────────────────

/// Metadata carried by drift-detector `Suggestion`s.
///
/// Populated by `candidate_to_suggestion`; consumed by `render.rs`.
/// All fields carry `#[serde(default)]` so future field additions
/// are forward-compatible with old readers.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DriftMeta {
    #[serde(default)]
    pub reason_kind: String,
    #[serde(default)]
    pub partner_marker: String,
    #[serde(default)]
    pub partner_clause: String,
    #[serde(default)]
    pub density: u8,
    #[serde(default)]
    pub command: String,
    #[serde(default)]
    pub touched_path: String,
    #[serde(default)]
    pub touched_start: Option<i64>,
    #[serde(default)]
    pub touched_end: Option<i64>,
    #[serde(default)]
    pub excerpt_of_path: String,
    #[serde(default)]
    pub excerpt_start: Option<i64>,
    #[serde(default)]
    pub excerpt_end: Option<i64>,
}

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

    /// The set of anchors that are recommended to form a new mesh.
    /// Each `MeshAnchor` carries path, line extent, and (if already
    /// anchored) mesh name and why.
    pub participants: Vec<MeshAnchor>,

    /// Human-readable label for the suggested mesh relationship.
    /// Empty string when the detector has no label to offer.
    pub label: String,

    /// Drift-detector metadata. `Some` for drift-detector suggestions;
    /// `None` for n-ary suggester suggestions. Dispatch on this field —
    /// not on parsing `label` — to distinguish suggestion kinds.
    pub meta: Option<DriftMeta>,
}

impl Serialize for Suggestion {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut s = serializer.serialize_struct("Suggestion", 7)?;
        s.serialize_field("v", &self.version)?;
        s.serialize_field("band", &self.band)?;
        s.serialize_field("viability", &self.viability)?;
        s.serialize_field("score", &self.score)?;
        // Serialize participants manually since MeshAnchor does not impl Serialize.
        let participants: Vec<_> = self.participants.iter().map(MeshAnchorView).collect();
        s.serialize_field("participants", &participants)?;
        s.serialize_field("label", &self.label)?;
        s.serialize_field("meta", &self.meta)?;
        s.end()
    }
}

/// Serialization view for `MeshAnchor`.
struct MeshAnchorView<'a>(&'a MeshAnchor);

impl Serialize for MeshAnchorView<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let r = self.0;
        let mut s = serializer.serialize_struct("MeshAnchor", 7)?;
        s.serialize_field("name", &r.name)?;
        s.serialize_field("why", &r.why)?;
        s.serialize_field("path", &r.path.to_string_lossy().as_ref())?;
        s.serialize_field("start", &r.start)?;
        s.serialize_field("end", &r.end)?;
        s.serialize_field("whole", &r.whole)?;
        let status = match r.status {
            MeshAnchorStatus::Stable => "stable",
            MeshAnchorStatus::Changed => "changed",
            MeshAnchorStatus::Moved => "moved",
            MeshAnchorStatus::Terminal => "terminal",
        };
        s.serialize_field("status", status)?;
        s.end()
    }
}

impl Suggestion {
    /// Construct a new suggestion with `version` pre-populated to `1`
    /// and `meta` set to `None`.
    pub fn new(
        band: ConfidenceBand,
        viability: Viability,
        score: ScoreBreakdown,
        participants: Vec<MeshAnchor>,
        label: String,
    ) -> Self {
        Self {
            version: 1,
            band,
            viability,
            score,
            participants,
            label,
            meta: None,
        }
    }

    /// Construct a drift-detector suggestion with explicit `DriftMeta`.
    pub fn new_drift(
        band: ConfidenceBand,
        viability: Viability,
        score: ScoreBreakdown,
        participants: Vec<MeshAnchor>,
        label: String,
        meta: DriftMeta,
    ) -> Self {
        Self {
            version: 1,
            band,
            viability,
            score,
            participants,
            label,
            meta: Some(meta),
        }
    }
}

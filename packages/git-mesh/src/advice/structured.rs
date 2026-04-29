//! Structured-English render primitives for the four-verb advice CLI.
//!
//! This module owns the display types, overlap predicate, and instruction
//! text generators that the `milestone` and `stop` verbs will use in Phase 3.
//! Phase 1 provides the types and function signatures; Phase 3 will fill
//! in the full verb behaviour.

use std::fmt;

use crate::types::{AnchorExtent, AnchorResolved, AnchorStatus, MeshResolved};

// ── Status ────────────────────────────────────────────────────────────────────

/// Anchor staleness status, mapped from `crate::types::AnchorStatus`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    Fresh,
    Moved,
    Changed,
    Orphaned,
    MergeConflict,
    Submodule,
    ContentUnavailable(String),
}

impl Status {
    /// Map from the resolver's `AnchorStatus`.
    pub fn from_anchor_status(s: &AnchorStatus) -> Self {
        match s {
            AnchorStatus::Fresh => Self::Fresh,
            AnchorStatus::Moved => Self::Moved,
            AnchorStatus::Changed => Self::Changed,
            AnchorStatus::Orphaned => Self::Orphaned,
            AnchorStatus::MergeConflict => Self::MergeConflict,
            AnchorStatus::Submodule => Self::Submodule,
            AnchorStatus::ContentUnavailable(reason) => {
                Self::ContentUnavailable(format!("{reason:?}"))
            }
        }
    }
}

impl fmt::Display for Status {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Fresh => write!(f, "FRESH"),
            Self::Moved => write!(f, "MOVED"),
            Self::Changed => write!(f, "CHANGED"),
            Self::Orphaned => write!(f, "ORPHANED"),
            Self::MergeConflict => write!(f, "MERGE_CONFLICT"),
            Self::Submodule => write!(f, "SUBMODULE"),
            Self::ContentUnavailable(reason) => write!(f, "CONTENT_UNAVAILABLE({reason})"),
        }
    }
}

// ── Action ────────────────────────────────────────────────────────────────────

/// A developer action that may overlap with a mesh anchor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// A line-range read or edit on a specific path.
    Range { path: String, start: u32, end: u32 },
    /// A whole-file read or edit.
    WholeFile { path: String },
}

/// Return true when `action` overlaps `anchor` on the **read** path.
///
/// Rules:
/// - `Action::Range` matches only `AnchorExtent::LineRange` anchors on the
///   same path where the line spans intersect (`max(starts) <= min(ends)`).
/// - `Action::WholeFile` matches only `AnchorExtent::WholeFile` anchors on
///   the same path.
///
/// Cross-kind matches (range action vs whole-file anchor, or vice versa) are
/// intentionally excluded: the spec treats them as distinct coverage types
/// and `read` actions always carry exact extent information.
pub fn read_overlaps(action: &Action, anchor: &AnchorResolved) -> bool {
    let anchor_path = anchor.anchored.path.to_string_lossy();
    match (action, &anchor.anchored.extent) {
        (
            Action::Range {
                path,
                start: a_start,
                end: a_end,
            },
            AnchorExtent::LineRange {
                start: r_start,
                end: r_end,
            },
        ) => {
            if path.as_str() != anchor_path.as_ref() {
                return false;
            }
            // Intersect: [a_start..a_end] ∩ [r_start..r_end] is non-empty.
            let lo = (*a_start).max(*r_start);
            let hi = (*a_end).min(*r_end);
            lo <= hi
        }
        (Action::WholeFile { path }, AnchorExtent::WholeFile) => {
            path.as_str() == anchor_path.as_ref()
        }
        // Cross-kind: no match.
        _ => false,
    }
}

/// Return true when `action` overlaps `anchor` on the **edit** path.
///
/// Same as [`read_overlaps`] for range actions. For `Action::WholeFile`,
/// matches **both** whole-file and range anchors on the same path, because
/// snapshot-derived edits carry no hunk bounds — `Action::WholeFile` is a
/// fallback that means "something changed in this file" and any anchor on
/// the path is potentially affected.
pub fn edit_overlaps(action: &Action, anchor: &AnchorResolved) -> bool {
    let anchor_path = anchor.anchored.path.to_string_lossy();
    match (action, &anchor.anchored.extent) {
        // Range action: same strict intersection as read_overlaps.
        (
            Action::Range {
                path,
                start: a_start,
                end: a_end,
            },
            AnchorExtent::LineRange {
                start: r_start,
                end: r_end,
            },
        ) => {
            if path.as_str() != anchor_path.as_ref() {
                return false;
            }
            let lo = (*a_start).max(*r_start);
            let hi = (*a_end).min(*r_end);
            lo <= hi
        }
        // Range action vs whole-file anchor: no match (same as read_overlaps).
        (Action::Range { .. }, AnchorExtent::WholeFile) => false,
        // Whole-file action: matches both whole-file AND range anchors on same path.
        // This is the relaxed companion: snapshot-derived edits lack hunk bounds,
        // so any anchor on the path is considered potentially affected.
        (Action::WholeFile { path }, _) => path.as_str() == anchor_path.as_ref(),
    }
}

// ── BasicOutput ───────────────────────────────────────────────────────────────

/// One mesh announce block as specified by the structured-English spec's
/// `BASIC_OUTPUT` template:
///
/// ```text
/// <active_anchor> is in the <mesh_name> mesh with:
/// - <non_active_anchor_1>
/// - <non_active_anchor_2>
///
/// <why>
/// ```
pub struct BasicOutput {
    /// The anchor whose action triggered this output.
    pub active_anchor: String,
    /// Mesh name (the `refs/meshes/v1/<name>` suffix).
    pub mesh_name: String,
    /// One-sentence description from `git mesh why`.
    pub why: String,
    /// Non-`Fresh` status of the active anchor, or `None` when fresh.
    /// Retained on the struct for callers that still surface status elsewhere;
    /// no longer emitted on the header line.
    pub status_if_not_fresh: Option<Status>,
    /// The other anchors in the mesh (excluding the active anchor).
    pub non_active_anchors: Vec<String>,
}

impl fmt::Display for BasicOutput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(
            f,
            "{} is in the {} mesh with:",
            self.active_anchor, self.mesh_name
        )?;
        for anchor in &self.non_active_anchors {
            writeln!(f, "- {anchor}")?;
        }
        if !self.why.is_empty() {
            writeln!(f)?;
            writeln!(f, "{}", self.why)?;
        }
        Ok(())
    }
}

// ── Predicates ────────────────────────────────────────────────────────────────

/// Return true when any anchor in `mesh` has a status other than `Fresh`.
pub fn mesh_is_stale(mesh: &MeshResolved) -> bool {
    mesh.anchors
        .iter()
        .any(|a| !matches!(a.status, AnchorStatus::Fresh))
}

// ── Instruction text generators ───────────────────────────────────────────────

/// Return the reconciliation instructions block for a stale mesh.
///
/// The text matches the tone of the existing renderer's preambles in
/// `advice/render.rs`. Printed at most once per session
/// (`SessionFlags::has_printed_reconciliation_instructions`).
///
/// # TODO
/// Align exact wording with the finalized structured-English spec before
/// Phase 3 ships — the phrasing below follows the existing renderer's
/// `render_hint_for_reason` output.
pub fn reconciliation_instructions(mesh: &MeshResolved) -> String {
    let stale_anchors: Vec<String> = mesh
        .anchors
        .iter()
        .filter(|a| !matches!(a.status, AnchorStatus::Fresh))
        .map(|a| format!("  {}", format_anchor_resolved(a)))
        .collect();

    let mut body = String::new();
    body.push_str("Reconcile the following meshes after your edits:\n");
    body.push_str(&format!("{} mesh: {}\n", mesh.name, mesh.message));
    for line in &stale_anchors {
        body.push_str(line);
        body.push('\n');
    }
    body.push('\n');
    body.push_str("To re-record an anchor after edits, run:\n");
    body.push_str("  git mesh add <name> <path>#L<s>-L<e>\n");
    body.push_str("  git mesh commit <name>\n");
    wrap_documentation(&body)
}

/// Return the creation instructions block for a set of anchors the developer
/// may want to associate with a new mesh.
///
/// The text matches the tone of the existing renderer's n-ary suggester
/// preamble. Printed at most once per session
/// (`SessionFlags::has_printed_creation_instructions`).
///
/// # TODO
/// Align exact wording with the finalized structured-English spec before
/// Phase 3 ships.
pub fn creation_instructions(anchors: &[&AnchorResolved]) -> String {
    let mut body = String::new();
    body.push_str("Use `git mesh` to document implicit semantic dependencies.\n");
    body.push_str("Potential candidates:\n");
    for a in anchors {
        body.push_str(&format!("- {}\n", format_anchor_resolved(a)));
    }
    body.push('\n');
    body.push_str("To record a candidate mesh, run:\n");
    body.push_str("  git mesh add <mesh-name> <path-1> <path-2>\n");
    body.push_str("  git mesh why <mesh-name> -m \"What the anchors do together.\"\n");
    body.push_str("  git mesh commit <mesh-name>\n");
    wrap_documentation(&body)
}

/// Wrap an inline-documentation block in `<documentation>` tags with a
/// trailing blank line so adjacent blocks remain visually separated.
pub fn wrap_documentation(body: &str) -> String {
    let trimmed = body.trim_end_matches('\n');
    format!("<documentation>\n{trimmed}\n</documentation>\n\n")
}

// ── Internal helpers ──────────────────────────────────────────────────────────

/// Format an `AnchorResolved` as `path#L<start>-L<end>` (range) or `path` (whole-file).
pub fn format_anchor_resolved(a: &AnchorResolved) -> String {
    let path = a.anchored.path.to_string_lossy();
    match &a.anchored.extent {
        AnchorExtent::LineRange { start, end } => format!("{path}#L{start}-L{end}"),
        AnchorExtent::WholeFile => path.into_owned(),
    }
}

/// Convenience: build an `Action` from a resolved anchor's current location.
/// Returns `None` when the anchor is terminal (no current location).
#[allow(dead_code)]
pub fn action_from_anchor(a: &AnchorResolved) -> Option<Action> {
    let current = a.current.as_ref()?;
    let path = current.path.to_string_lossy().into_owned();
    Some(match &current.extent {
        AnchorExtent::LineRange { start, end } => Action::Range {
            path,
            start: *start,
            end: *end,
        },
        AnchorExtent::WholeFile => Action::WholeFile { path },
    })
}

/// Convenience: build an `Action` from a plain repo-relative path string
/// (no `#L` suffix → whole-file; `path#L<s>-L<e>` → range).
pub fn action_from_spec(spec: &str) -> Option<Action> {
    if let Some((path, frag)) = spec.split_once("#L") {
        let (s, e) = frag.split_once("-L")?;
        let start: u32 = s.parse().ok()?;
        let end: u32 = e.parse().ok()?;
        Some(Action::Range {
            path: path.to_string(),
            start,
            end,
        })
    } else {
        Some(Action::WholeFile {
            path: spec.to_string(),
        })
    }
}

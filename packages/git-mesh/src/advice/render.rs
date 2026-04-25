//! `#`-prefixed markdown renderer for advice flushes.
//!
//! Every output line is prefixed with `#` so the result reads as a comment
//! in any shell, log, or diff view. Excerpts are fenced with triple
//! backticks (or four, when the excerpt itself contains ```), language
//! inferred from the partner path's extension.

use crate::advice::intersections::{Candidate, Density, ReasonKind};

const MAX_LINE: usize = 200;

/// Render a flush given deduped candidates and the list of doc topics
/// that fired for the first time this flush.
///
/// `documentation` gates the per-reason appendix (§12.11).
pub fn render(candidates: &[Candidate], new_doc_topics: &[String], documentation: bool) -> String {
    if candidates.is_empty() {
        return String::new();
    }

    let mut blocks: Vec<String> = Vec::new();

    // Doc-topic preamble.
    for topic in new_doc_topics {
        blocks.push(render_doc_topic(topic));
    }

    // Group candidates by mesh, keep cross-cutting types last.
    let (per_mesh, cross_cutting): (Vec<&Candidate>, Vec<&Candidate>) =
        candidates.iter().partition(|c| {
            !matches!(
                c.reason_kind,
                ReasonKind::NewGroup | ReasonKind::StagingCrossCut | ReasonKind::EmptyMesh
            )
        });

    let mut by_mesh: std::collections::BTreeMap<String, Vec<&Candidate>> =
        std::collections::BTreeMap::new();
    for c in &per_mesh {
        by_mesh.entry(c.mesh.clone()).or_default().push(c);
    }
    // Slice 3 (4.3): excerpt dedup is *flush-scoped* — emit each
    // (path,start,end) excerpt at most once per flush. Subsequent meshes
    // that pin the same partner range still list the address in their
    // partner block but skip the fenced body. Documented in §12.5.
    let mut seen_excerpts: std::collections::BTreeSet<(String, Option<i64>, Option<i64>)> =
        std::collections::BTreeSet::new();
    for (mesh, cands) in &by_mesh {
        blocks.push(render_mesh_block(mesh, cands, &mut seen_excerpts));
    }

    for cc in &cross_cutting {
        blocks.push(render_cross_cutting(cc));
    }

    if documentation {
        // §12.11 — per-reason appendix. Fires for each reason-kind that
        // appeared in this flush.
        let mut seen: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
        let mut appendix = String::new();
        for c in candidates {
            let Some(topic) = c.reason_kind.doc_topic() else {
                continue;
            };
            if !seen.insert(topic) {
                continue;
            }
            if !appendix.is_empty() {
                appendix.push_str("#\n");
            }
            appendix.push_str(&render_doc_topic(topic));
        }
        if !appendix.is_empty() {
            blocks.push(appendix);
        }
    }

    // Blank (comment-only) lines between blocks.
    let mut out = String::new();
    for (i, b) in blocks.iter().enumerate() {
        if i > 0 {
            out.push_str("#\n");
        }
        out.push_str(b);
    }
    out
}

fn render_mesh_block(
    mesh: &str,
    cands: &[&Candidate],
    seen_excerpts: &mut std::collections::BTreeSet<(String, Option<i64>, Option<i64>)>,
) -> String {
    // Order: T1 partners → T2 excerpts → T3-6 clauses/excerpts/commands.
    let why = cands.first().map(|c| c.mesh_why.as_str()).unwrap_or("");
    let mut out = String::new();
    out.push_str(&format!("# {mesh} mesh: {why}\n"));

    // Partner addresses (deduped by (path,start,end)).
    let mut seen_partner: std::collections::BTreeSet<(String, Option<i64>, Option<i64>)> =
        std::collections::BTreeSet::new();
    for c in cands {
        let key = (
            c.partner_path.clone(),
            c.partner_start,
            c.partner_end,
        );
        if !seen_partner.insert(key) {
            continue;
        }
        let addr = format_addr(&c.partner_path, c.partner_start, c.partner_end);
        let mut line = format!("# - {addr}");
        if !c.partner_marker.is_empty() {
            line.push(' ');
            line.push_str(&c.partner_marker);
        }
        if !c.partner_clause.is_empty() {
            line.push_str(" — ");
            line.push_str(&c.partner_clause);
        }
        out.push_str(&truncate_line(&line));
        out.push('\n');
    }

    // Excerpts.
    for c in cands {
        if !matches!(c.density, Density::L1 | Density::L2) {
            continue;
        }
        if c.excerpt_of_path.is_empty() {
            continue;
        }
        // Slice 3 (4.4): whole-file / binary / submodule / deleted /
        // LFS partners are address-only per §12.5. Detected here as
        // either a whole-file pin (no line range) or a non-empty
        // partner_marker that maps to a non-excerpt state.
        let is_whole_file = c.excerpt_start.is_none() || c.excerpt_end.is_none();
        let is_non_excerpt_marker = matches!(
            c.partner_marker.as_str(),
            "[ORPHANED]" | "[CONFLICT]" | "[SUBMODULE]" | "[DELETED]"
        );
        if is_whole_file || is_non_excerpt_marker {
            continue;
        }
        // Slice 3 (4.3): per-flush dedup of identical excerpts.
        let key = (
            c.excerpt_of_path.clone(),
            c.excerpt_start,
            c.excerpt_end,
        );
        if !seen_excerpts.insert(key) {
            continue;
        }
        let body = render_excerpt(c);
        if body.is_empty() {
            // Empty body (e.g. file unreadable, range past EOF). Skip the
            // address line too — an address with no excerpt would render
            // as a stray paragraph.
            continue;
        }
        out.push_str("#\n");
        let addr = format_addr(&c.excerpt_of_path, c.excerpt_start, c.excerpt_end);
        out.push_str(&format!("# {addr}\n"));
        out.push_str(&body);
    }

    // Commands (L2).
    for c in cands {
        if c.density != Density::L2 || c.command.is_empty() {
            continue;
        }
        out.push_str("#\n");
        let lead = command_lead_in(c.reason_kind);
        out.push_str(&format!("# {lead}\n"));
        for line in c.command.lines() {
            out.push_str("#   ");
            out.push_str(line);
            out.push('\n');
        }
    }

    out
}

fn render_cross_cutting(c: &Candidate) -> String {
    let mut out = String::new();
    match c.reason_kind {
        ReasonKind::NewGroup => {
            out.push_str("# Possible new group over:\n");
            out.push_str(&format!("# - {}\n", c.trigger_path));
            out.push_str(&format!("# - {}\n", c.partner_path));
            if !c.partner_clause.is_empty() {
                out.push_str(&format!("# {}.\n", c.partner_clause));
            }
            if !c.command.is_empty() {
                out.push_str("#\n");
                out.push_str("# To record a new group:\n");
                for line in c.command.lines() {
                    out.push_str("#   ");
                    out.push_str(line);
                    out.push('\n');
                }
            }
        }
        ReasonKind::StagingCrossCut => {
            // partner_clause is a structured packing produced by
            // detect_t8: either "overlap|<staged_mesh>|<other_mesh>|<path>|<is>|<ie>|<os>|<oe>|"
            // or "content_differs|<staged_mesh>|<other_mesh>|<path>|<os>|<oe>".
            let parts: Vec<&str> = c.partner_clause.split('|').collect();
            match parts.first().copied() {
                Some("overlap") if parts.len() >= 8 => {
                    let staged_mesh = parts[1];
                    let other_mesh = parts[2];
                    let path = parts[3];
                    let is_ = parts[4];
                    let ie = parts[5];
                    let os = parts[6];
                    let oe = parts[7];
                    let s_start = c.trigger_start.unwrap_or(0);
                    let s_end = c.trigger_end.unwrap_or(0);
                    out.push_str(&format!(
                        "# {staged_mesh} [STAGED] overlaps {other_mesh} at {path}#L{is_}-L{ie}.\n"
                    ));
                    out.push_str(&format!("# - {other_mesh}: {path}#L{os}-L{oe}\n"));
                    out.push_str(&format!(
                        "# - {staged_mesh} [STAGED]: {path}#L{s_start}-L{s_end}\n"
                    ));
                }
                Some("content_differs") if parts.len() >= 6 => {
                    let staged_mesh = parts[1];
                    let other_mesh = parts[2];
                    let path = parts[3];
                    let os = parts[4];
                    let oe = parts[5];
                    out.push_str(&format!(
                        "# {staged_mesh} [STAGED] re-records {path}#L{os}-L{oe} with content that differs from {other_mesh}.\n"
                    ));
                    out.push_str(&format!("# - {other_mesh}: {path}#L{os}-L{oe}\n"));
                    out.push_str(&format!(
                        "# - {staged_mesh} [STAGED]: {path}#L{os}-L{oe}\n"
                    ));
                }
                _ => {
                    out.push_str(&format!("# {} [STAGED]\n", c.mesh));
                    if !c.partner_clause.is_empty() {
                        out.push_str(&format!("# {}.\n", c.partner_clause));
                    }
                }
            }
            if !c.command.is_empty() {
                out.push_str("#\n");
                out.push_str("# To resolve:\n");
                for line in c.command.lines() {
                    out.push_str("#   ");
                    out.push_str(line);
                    out.push('\n');
                }
            }
        }
        ReasonKind::EmptyMesh => {
            // partner_clause is "removed:<addr1>,<addr2>,..." packed by
            // detect_t9. Render the §12.10 / §12.12 T9 template verbatim.
            let removed = c
                .partner_clause
                .strip_prefix("removed:")
                .unwrap_or("");
            let addrs: Vec<&str> = removed.split(',').filter(|s| !s.is_empty()).collect();
            out.push_str(&format!(
                "# The staged removal would leave {} with no ranges.\n",
                c.mesh
            ));
            for addr in &addrs {
                out.push_str(&format!("# - {}: removing {addr}\n", c.mesh));
            }
            if !c.command.is_empty() {
                out.push_str("#\n");
                out.push_str(
                    "# To either add a replacement range or retire the mesh:\n",
                );
                for line in c.command.lines() {
                    out.push_str("#   ");
                    out.push_str(line);
                    out.push('\n');
                }
            }
        }
        _ => {}
    }
    out
}

fn command_lead_in(kind: ReasonKind) -> &'static str {
    match kind {
        ReasonKind::RenameLiteral => "To record the rename:",
        ReasonKind::RangeCollapse => "To re-record with the new extent:",
        ReasonKind::LosingCoherence => "To narrow or retire the group:",
        ReasonKind::SymbolRename => "To re-record both sides:",
        _ => "To reconcile:",
    }
}

fn render_excerpt(c: &Candidate) -> String {
    // Binary / deleted / LFS / submodule / orphaned → address-only, no
    // excerpt. Marker on the partner line already conveys the state.
    if matches!(
        c.partner_marker.as_str(),
        "[ORPHANED]" | "[CONFLICT]" | "[SUBMODULE]" | "[DELETED]"
    ) {
        return String::new();
    }
    // Caller must have loaded the excerpt content via `excerpt_body`.
    // We don't re-fetch from disk here — the flush layer may populate
    // `partner_clause`-driven excerpts in future; for now we inline the
    // raw read via the `excerpt_of_path` fields lazily.
    let body = read_excerpt(c).unwrap_or_default();
    if body.trim().is_empty() {
        return String::new();
    }
    let lang = lang_for(&c.excerpt_of_path);
    let fence = if body.contains("```") { "````" } else { "```" };
    let mut out = String::new();
    out.push_str(&format!("# {fence}{lang}\n"));
    for line in body.lines().take(10) {
        let t = truncate_line_plain(line);
        out.push_str("# ");
        out.push_str(&t);
        out.push('\n');
    }
    out.push_str(&format!("# {fence}\n"));
    out
}

fn read_excerpt(c: &Candidate) -> Option<String> {
    let path = std::path::Path::new(&c.excerpt_of_path);
    if !path.exists() {
        return None;
    }
    let bytes = std::fs::read(path).ok()?;
    let text = std::str::from_utf8(&bytes).ok()?.to_string();
    match (c.excerpt_start, c.excerpt_end) {
        (Some(s), Some(e)) => {
            let lines: Vec<&str> = text.lines().collect();
            let lo = (s.max(1) as usize).saturating_sub(1);
            let hi = (e as usize).min(lines.len());
            if lo >= hi {
                return Some(String::new());
            }
            Some(lines[lo..hi].join("\n"))
        }
        _ => Some(text),
    }
}

fn lang_for(path: &str) -> &'static str {
    match std::path::Path::new(path)
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
    {
        "ts" => "ts",
        "tsx" => "tsx",
        "html" => "html",
        "rs" => "rs",
        "py" => "py",
        _ => "",
    }
}

fn format_addr(path: &str, s: Option<i64>, e: Option<i64>) -> String {
    match (s, e) {
        (Some(s), Some(e)) => format!("{path}#L{s}-L{e}"),
        _ => path.to_string(),
    }
}

fn truncate_line(line: &str) -> String {
    if line.chars().count() <= MAX_LINE {
        line.to_string()
    } else {
        let mut s: String = line.chars().take(MAX_LINE - 1).collect();
        s.push('…');
        s
    }
}

fn truncate_line_plain(line: &str) -> String {
    let max = MAX_LINE - 2; // account for "# " prefix
    if line.chars().count() <= max {
        line.to_string()
    } else {
        let mut s: String = line.chars().take(max - 1).collect();
        s.push('…');
        s
    }
}

fn render_doc_topic(topic: &str) -> String {
    // Short single-sentence doc topic per §12.6/§12.12. We render a
    // compact version inline; the full blocks in advice-notes.md are
    // verbose. Keep one sentence per reason-kind for readability.
    let body: &str = match topic {
        "editing across files" => {
            "When a range in a mesh changes, the other ranges may need matching changes."
        }
        "renames" => {
            "A related range contains the old path as a literal string. Hard-coded paths do not follow a rename."
        }
        "shrinking ranges" => {
            "The edit reduced a range to far fewer lines than were recorded; remove the old range and re-add the new extent."
        }
        "narrow or retire" => {
            "Most ranges in this mesh no longer match; narrow the mesh, or retire it."
        }
        "exported symbols" => {
            "An exported name changed; other ranges may reference the old name as a literal string."
        }
        "recording a group" => {
            "These files move together across session touches and recent history; a mesh can capture that."
        }
        "cross-mesh overlap" => {
            "A staged range overlaps another mesh's range in the same file; confirm both relationships are real."
        }
        "empty groups" => {
            "The staged removal would leave this mesh with no ranges; add a replacement or retire the mesh."
        }
        "terminal states" => {
            "A terminal marker means the resolver cannot evaluate this range: ORPHANED, CONFLICT, or SUBMODULE."
        }
        _ => topic,
    };
    format!("# {body}\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::advice::intersections::{Candidate, Density, ReasonKind};

    fn cand(mesh: &str, partner: &str) -> Candidate {
        Candidate {
            mesh: mesh.into(),
            mesh_why: "why text".into(),
            reason_kind: ReasonKind::Partner,
            partner_path: partner.into(),
            partner_start: Some(1),
            partner_end: Some(10),
            trigger_path: "t.rs".into(),
            trigger_start: None,
            trigger_end: None,
            partner_marker: String::new(),
            partner_clause: String::new(),
            density: Density::L0,
            command: String::new(),
            excerpt_of_path: String::new(),
            excerpt_start: None,
            excerpt_end: None,
        }
    }

    #[test]
    fn empty_input_renders_empty_string() {
        assert_eq!(render(&[], &[], false), "");
    }

    #[test]
    fn every_line_prefixed_with_hash() {
        let c = cand("m1", "b.rs");
        let out = render(&[c], &[], false);
        for line in out.lines() {
            assert!(line.starts_with('#'), "line does not start with #: {line:?}");
        }
    }

    #[test]
    fn mesh_header_and_partner_address() {
        let c = cand("m1", "b.rs");
        let out = render(&[c], &[], false);
        assert!(out.contains("# m1 mesh: why text"));
        assert!(out.contains("# - b.rs#L1-L10"));
    }

    #[test]
    fn blank_comment_lines_between_blocks() {
        let mut c1 = cand("m1", "b.rs");
        c1.mesh = "m1".into();
        let mut c2 = cand("m2", "c.rs");
        c2.mesh = "m2".into();
        let out = render(&[c1, c2], &[], false);
        assert!(out.contains("#\n"));
    }

    #[test]
    fn marker_appended_when_present() {
        let mut c = cand("m1", "b.rs");
        c.partner_marker = "[CHANGED]".into();
        let out = render(&[c], &[], false);
        assert!(out.contains("[CHANGED]"));
    }
}

//! `#`-prefixed markdown renderer for advice flushes.
//!
//! Every output line is prefixed with `#` so the result reads as a comment
//! in any shell, log, or diff view. Excerpts are fenced with triple
//! backticks (or four, when the excerpt itself contains ```), language
//! inferred from the partner path's extension.
//!
//! The public entry point `render` consumes `Vec<Suggestion>`. Each
//! suggestion encodes its type in the `meta` field:
//!
//! - Drift-detector suggestions: `meta` is `Some(DriftMeta)`.
//! - N-ary suggester suggestions: `meta` is `None`.

use crate::advice::suggestion::{DriftMeta, Suggestion};

const MAX_LINE: usize = 200;

// ── Reason-kind strings (mirrored from candidates::ReasonKind::as_str) ───────

const REASON_NEW_GROUP: &str = "new_group";
const REASON_STAGING_CROSS_CUT: &str = "staging_cross_cut";
const REASON_EMPTY_MESH: &str = "empty_mesh";

/// Render a flush given deduped suggestions and the list of doc topics
/// that fired for the first time this flush.
///
/// `documentation` gates the per-reason appendix (§12.11).
pub fn render(suggestions: &[Suggestion], new_doc_topics: &[String], documentation: bool) -> String {
    if suggestions.is_empty() {
        return String::new();
    }

    let mut blocks: Vec<String> = Vec::new();

    // Partition into cross-cutting (NewGroup, StagingCrossCut, EmptyMesh) and
    // per-mesh suggestions, mirroring the previous Candidate-based split.
    let (per_mesh_suggs, cross_cutting_suggs): (Vec<&Suggestion>, Vec<&Suggestion>) =
        suggestions.iter().partition(|s| {
            let reason = drift_reason(s);
            !matches!(
                reason.as_deref(),
                Some(REASON_NEW_GROUP) | Some(REASON_STAGING_CROSS_CUT) | Some(REASON_EMPTY_MESH)
            )
        });

    // Group per-mesh suggestions by mesh name (from participants[0].name).
    let mut by_mesh: std::collections::BTreeMap<String, Vec<&Suggestion>> =
        std::collections::BTreeMap::new();
    for s in &per_mesh_suggs {
        let mesh_name = s.participants.first().map(|p| p.name.as_str()).unwrap_or("").to_string();
        by_mesh.entry(mesh_name).or_default().push(s);
    }

    // Flush-scoped excerpt dedup (path, start, end).
    let mut seen_excerpts: std::collections::BTreeSet<(String, Option<i64>, Option<i64>)> =
        std::collections::BTreeSet::new();

    for (mesh, suggs) in &by_mesh {
        blocks.push(render_mesh_block(mesh, suggs, &mut seen_excerpts));
    }

    for s in &cross_cutting_suggs {
        blocks.push(render_cross_cutting_suggestion(s));
    }

    if documentation {
        // Doc-topic preamble — only when --documentation is requested.
        for topic in new_doc_topics {
            blocks.push(render_doc_topic(topic));
        }

        // §12.11 — per-reason hint appendix. Deduped per flush by reason-kind.
        let mut seen: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        let mut appendix = String::new();
        for s in suggestions {
            let Some(reason) = drift_reason(s) else { continue };
            if !seen.insert(reason.clone()) {
                continue;
            }
            let Some(hint) = render_hint_for_reason(&reason) else { continue };
            appendix.push_str(&hint);
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

/// Extract the reason-kind string from a drift-detector suggestion.
/// Returns None for n-ary suggester suggestions (where `s.meta` is None).
fn drift_reason(s: &Suggestion) -> Option<String> {
    s.meta.as_ref().map(|m| m.reason_kind.clone())
}

fn render_mesh_block(
    mesh: &str,
    suggs: &[&Suggestion],
    seen_excerpts: &mut std::collections::BTreeSet<(String, Option<i64>, Option<i64>)>,
) -> String {
    // Determine the mesh why from the first participant of the first suggestion.
    let why = suggs
        .first()
        .and_then(|s| s.participants.first())
        .map(|p| p.why.as_str())
        .unwrap_or("");

    let mut out = String::new();
    out.push_str(&format!("# {mesh} mesh: {why}\n"));

    // For each suggestion in this block, render its content.
    // Drift suggestions carry DriftMeta in label; n-ary suggestions do not.
    let mut seen_trigger: std::collections::BTreeSet<(String, Option<i64>, Option<i64>)> =
        std::collections::BTreeSet::new();
    let mut seen_bullet: std::collections::BTreeSet<(String, Option<i64>, Option<i64>)> =
        std::collections::BTreeSet::new();

    for s in suggs {
        if let Some(meta) = s.meta.as_ref() {
            let meta = meta.clone();
            // Drift suggestion: participants[0] = partner, participants[1] = trigger (optional).
            let partner = s.participants.first();
            let trigger = s.participants.get(1);

            // Trigger locator line.
            if let Some(t) = trigger {
                let t_path = t.path.to_string_lossy().to_string();
                let (ts, te) = participant_range(s, 1);
                let key = (t_path.clone(), ts, te);
                if seen_trigger.insert(key) {
                    let addr = format_addr(&t_path, ts, te);
                    out.push_str(&format!("# triggered by {addr}\n"));
                }
            }

            // Partner bullet.
            if let Some(p) = partner {
                let p_path = p.path.to_string_lossy().to_string();
                let (ps, pe) = partner_range_from_meta(s, &meta);
                let key = (p_path.clone(), ps, pe);
                if seen_bullet.insert(key) {
                    let addr = format_addr(&p_path, ps, pe);
                    let mut line = format!("# - {addr}");
                    if !meta.partner_marker.is_empty() {
                        line.push(' ');
                        line.push_str(&meta.partner_marker);
                    }
                    if !meta.partner_clause.is_empty() {
                        line.push_str(" — ");
                        line.push_str(&meta.partner_clause);
                    }
                    out.push_str(&truncate_line(&line));
                    out.push('\n');
                }
            }

            // Touched bullet.
            if !meta.touched_path.is_empty() {
                let key = (meta.touched_path.clone(), meta.touched_start, meta.touched_end);
                if seen_bullet.insert(key) {
                    let addr = format_addr(&meta.touched_path, meta.touched_start, meta.touched_end);
                    let line = format!("# - {addr}");
                    out.push_str(&truncate_line(&line));
                    out.push('\n');
                }
            }

            // Excerpts (L1/L2).
            if meta.density >= 1 && !meta.excerpt_of_path.is_empty() {
                let is_whole_file = meta.excerpt_start.is_none() || meta.excerpt_end.is_none();
                let is_non_excerpt_marker = matches!(
                    meta.partner_marker.as_str(),
                    "[ORPHANED]" | "[CONFLICT]" | "[SUBMODULE]" | "[DELETED]"
                );
                if !is_whole_file && !is_non_excerpt_marker {
                    let key = (meta.excerpt_of_path.clone(), meta.excerpt_start, meta.excerpt_end);
                    if seen_excerpts.insert(key) {
                        let body = read_excerpt_from_meta(&meta);
                        if !body.is_empty() {
                            out.push_str("#\n");
                            let addr = format_addr(&meta.excerpt_of_path, meta.excerpt_start, meta.excerpt_end);
                            out.push_str(&format!("# {addr}\n"));
                            out.push_str(&body);
                        }
                    }
                }
            }

            // Commands (L2).
            if meta.density == 2 && !meta.command.is_empty() {
                out.push_str("#\n");
                let lead = command_lead_in_for_reason(&meta.reason_kind);
                out.push_str(&format!("# {lead}\n"));
                for line in meta.command.lines() {
                    out.push_str("#   ");
                    out.push_str(line);
                    out.push('\n');
                }
            }
        } else {
            // N-ary suggestion: render each participant as a bullet.
            for (i, p) in s.participants.iter().enumerate() {
                let p_path = p.path.to_string_lossy().to_string();
                let (ps, pe) = if p.whole {
                    (None, None)
                } else {
                    (Some(p.start as i64), Some(p.end as i64))
                };
                let key = (p_path.clone(), ps, pe);
                if i == 0 {
                    // First participant is the "trigger" conceptually for n-ary.
                    if seen_trigger.insert(key) {
                        let addr = format_addr(&p_path, ps, pe);
                        out.push_str(&format!("# triggered by {addr}\n"));
                    }
                } else if seen_bullet.insert(key) {
                    let addr = format_addr(&p_path, ps, pe);
                    let line = format!("# - {addr}");
                    out.push_str(&truncate_line(&line));
                    out.push('\n');
                }
            }
        }
    }

    out
}

/// For drift suggestions, get the partner (participants[0]) start/end,
/// preferring the DriftMeta-aware whole-file flag from the candidate.
fn partner_range_from_meta(s: &Suggestion, _meta: &DriftMeta) -> (Option<i64>, Option<i64>) {
    participant_range(s, 0)
}

/// Get (start, end) for participant at index `idx` in a suggestion,
/// returning (None, None) for whole-file pins.
fn participant_range(s: &Suggestion, idx: usize) -> (Option<i64>, Option<i64>) {
    let Some(p) = s.participants.get(idx) else { return (None, None) };
    if p.whole {
        (None, None)
    } else {
        (Some(p.start as i64), Some(p.end as i64))
    }
}

fn render_cross_cutting_suggestion(s: &Suggestion) -> String {
    let Some(meta) = s.meta.as_ref() else {
        // N-ary cross-cutting — simple list.
        let mut out = String::new();
        out.push_str("# mesh recommendation:\n");
        for p in &s.participants {
            out.push_str(&format!("# - {}\n", p.path.display()));
        }
        return out;
    };

    // Reconstruct from DriftMeta — mirrors the previous render_cross_cutting(Candidate).
    let partner_path = s.participants.first().map(|p| p.path.to_string_lossy().to_string()).unwrap_or_default();
    let trigger_path = s.participants.get(1).map(|p| p.path.to_string_lossy().to_string()).unwrap_or_default();
    let (trigger_start, trigger_end) = participant_range(s, 1);
    let mesh = s.participants.first().map(|p| p.name.as_str()).unwrap_or("");

    let mut out = String::new();
    match meta.reason_kind.as_str() {
        REASON_NEW_GROUP => {
            out.push_str("# Possible new mesh over:\n");
            out.push_str(&format!("# - {}\n", trigger_path));
            out.push_str(&format!("# - {}\n", partner_path));
            if !meta.partner_clause.is_empty() {
                out.push_str(&format!("# {}.\n", meta.partner_clause));
            }
            if !meta.command.is_empty() {
                out.push_str("#\n");
                out.push_str("# To record a new mesh:\n");
                for line in meta.command.lines() {
                    out.push_str("#   ");
                    out.push_str(line);
                    out.push('\n');
                }
            }
        }
        REASON_STAGING_CROSS_CUT => {
            let parts: Vec<&str> = meta.partner_clause.split('|').collect();
            match parts.first().copied() {
                Some("overlap") if parts.len() >= 8 => {
                    let staged_mesh = parts[1];
                    let other_mesh = parts[2];
                    let path = parts[3];
                    let is_ = parts[4];
                    let ie = parts[5];
                    let os = parts[6];
                    let oe = parts[7];
                    let s_start = trigger_start.unwrap_or(0);
                    let s_end = trigger_end.unwrap_or(0);
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
                    out.push_str(&format!("# {} [STAGED]\n", mesh));
                    if !meta.partner_clause.is_empty() {
                        out.push_str(&format!("# {}.\n", meta.partner_clause));
                    }
                }
            }
            if !meta.command.is_empty() {
                out.push_str("#\n");
                out.push_str("# To resolve:\n");
                for line in meta.command.lines() {
                    out.push_str("#   ");
                    out.push_str(line);
                    out.push('\n');
                }
            }
        }
        REASON_EMPTY_MESH => {
            let removed = meta.partner_clause.strip_prefix("removed:").unwrap_or("");
            let addrs: Vec<&str> = removed.split(',').filter(|s| !s.is_empty()).collect();
            out.push_str(&format!(
                "# The staged removal would leave {} with no ranges.\n",
                mesh
            ));
            for addr in &addrs {
                out.push_str(&format!("# - {}: removing {addr}\n", mesh));
            }
            if !meta.command.is_empty() {
                out.push_str("#\n");
                out.push_str("# To either add a replacement range or retire the mesh:\n");
                for line in meta.command.lines() {
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

fn command_lead_in_for_reason(reason: &str) -> &'static str {
    match reason {
        "rename_literal" => "to re-record after the rename, run:",
        "range_collapse" => "To re-record with the new extent:",
        "losing_coherence" => "To narrow or retire the mesh:",
        "symbol_rename" => "To re-record both sides:",
        _ => "To reconcile:",
    }
}

fn read_excerpt_from_meta(meta: &DriftMeta) -> String {
    if matches!(
        meta.partner_marker.as_str(),
        "[ORPHANED]" | "[CONFLICT]" | "[SUBMODULE]" | "[DELETED]"
    ) {
        return String::new();
    }
    let path = std::path::Path::new(&meta.excerpt_of_path);
    if !path.exists() {
        return String::new();
    }
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(_) => return String::new(),
    };
    let text = match std::str::from_utf8(&bytes) {
        Ok(t) => t.to_string(),
        Err(_) => return String::new(),
    };
    let body = match (meta.excerpt_start, meta.excerpt_end) {
        (Some(s), Some(e)) => {
            let lines: Vec<&str> = text.lines().collect();
            let lo = (s.max(1) as usize).saturating_sub(1);
            let hi = (e as usize).min(lines.len());
            if lo >= hi {
                return String::new();
            }
            lines[lo..hi].join("\n")
        }
        _ => text,
    };
    if body.trim().is_empty() {
        return String::new();
    }
    let lang = lang_for(&meta.excerpt_of_path);
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

// §12.12 doc-topic blocks, verbatim modulo the `# ` prefix added by
// `render_doc_topic`. Each block is a single source of truth — the test
// suite asserts the literal text fragments.
const TOPIC_BASELINE: &str = "\
A mesh is a lightweight contract for an agreement that no schema, type,
or test already enforces. It anchors line ranges (or whole files)
across the repo and carries a durable `why` that defines the subsystem
those ranges collectively form.

The `why` is load-bearing identity, not commentary. It names the
subsystem, flow, or concern and says plainly what the ranges do across
it — e.g. \"Checkout request flow that carries a charge attempt from
the browser to the Stripe-backed server.\" It is evergreen, inherited
across routine re-anchors, and is the line printed after `<name> mesh:`
on every appearance below. Invariants, caveats, and ownership belong
in source comments, commit messages, CODEOWNERS, and PR descriptions —
not in the why.

Inspect a mesh:
  git mesh show <name>           # ranges, why, history
  git mesh ls <path>             # meshes that touch a file
  git mesh stale                 # ranges whose bytes have drifted
  git mesh why <name>            # read the why
";

const TOPIC_T2: &str = "\
When a range in a mesh changes, the other ranges in the same mesh may
need matching changes. The excerpt below is the related range — the
code on the other side of the relationship. Compare, then either update
it or accept that the relationship has shifted and re-record the mesh.

A second `git mesh add` over the identical (path, extent) is a
re-record — last-write-wins, no `rm` needed:

  git mesh add <name> <path>#L<s>-L<e>
  git mesh commit <name>              # finalized by the post-commit hook
";

const TOPIC_T3: &str = "\
A related range contains the old path as a literal string. A renamed
file still works for callers that import it by symbol, but hard-coded
paths — markup src, fetch URLs, doc links — do not follow a rename.
Update the literal, or move the mesh to the new path:

  git mesh rm  <name> <old-path>
  git mesh add <name> <new-path>
  git mesh commit <name>
";

const TOPIC_T4: &str = "\
The edit reduced a range to far fewer lines than were recorded. The
mesh now pins less code than the relationship was about. When the line
span changes, remove the old range first, then add the new one:

  git mesh rm  <name> <path>#L<old-s>-L<old-e>
  git mesh add <name> <path>#L<new-s>-L<new-e>
  git mesh commit <name>
";

const TOPIC_T5: &str = "\
Most ranges in this mesh no longer match what was recorded. When most
of a mesh has drifted, the relationship itself has usually changed.
Narrow the mesh to the ranges still in play, or retire it:

  git mesh rm     <name> <path>          # drop a range
  git mesh delete <name>                 # retire the mesh
  git mesh revert <name> <commit-ish>    # restore a prior correct state
";

const TOPIC_T6: &str = "\
An exported name changed inside one range. Other ranges reference the
old name as a literal string, which a rename-aware refactor tool will
not reach. Update the references, then re-record both sides in the
same commit:

  git mesh add <name> <path>#L<s>-L<e>
  git mesh commit <name>
";

const TOPIC_T7: &str = "\
These files move together: the session has touched them together and
git history shows them co-changing. A mesh captures that so future
edits on one side surface the others. Only record one if the
relationship is real and not already enforced by a type, schema,
validator, or test — those reject violations automatically and are
strictly better than a mesh over the same surface.

Record:
  git mesh add <mesh-name> <path-1> <path-2> [...]
  git mesh why <mesh-name> -m \"What the ranges do together.\"
  git mesh commit <mesh-name>

Name with a kebab-case slug that titles the subsystem, optionally
prefixed by a category: billing/, platform/, experiments/, auth/.
One relationship per mesh — if ranges split into two reasons to change
together, record two meshes.
";

const TOPIC_T8: &str = "\
A range staged on one mesh overlaps a range already recorded on
another mesh in the same file. Both meshes will observe edits to the
shared bytes independently. Confirm both relationships are real; if
they describe the same thing, collapse them:

  git mesh restore <name>                # drop staged changes on a mesh
  git mesh delete  <name>                # retire the redundant mesh
";

const TOPIC_T9: &str = "\
The staged removal would leave this mesh with no ranges. A mesh with
nothing in it cannot surface drift. Either add a replacement range in
the same commit, or retire the mesh:

  git mesh add    <name> <path>[#L<s>-L<e>]
  git mesh delete <name>
";

const TOPIC_T11: &str = "\
A terminal marker means the resolver cannot evaluate this range at all.

[ORPHANED]  — the recorded commit is unreachable. Usually a force-push
              or a partial clone. Fetch and re-record if needed:
                git fetch --all && git mesh fetch
                git mesh add <name> <path>#L<s>-L<e>
                git mesh commit <name>

[CONFLICT]  — the file is mid-merge. Finish the merge first.

[SUBMODULE] — the range points inside a submodule, which mesh does not
              open. Pin the submodule root or a parent-repo path that
              witnesses the same relationship:
                git mesh rm  <name> <submodule>/inner/file.ts#L10-L20
                git mesh add <name> <submodule>
                git mesh commit <name>
";

/// Map a canonical topic name (§12.12 quoted titles, hyphen-separated)
/// to its body. Returns `None` for unknown topics so the flush layer
/// fails closed rather than emitting a stub.
pub(crate) fn topic_body(topic: &str) -> Option<&'static str> {
    Some(match topic {
        "baseline" => TOPIC_BASELINE,
        "editing-across-files" => TOPIC_T2,
        "renames" => TOPIC_T3,
        "shrinking-ranges" => TOPIC_T4,
        "narrow-or-retire" => TOPIC_T5,
        "exported-symbols" => TOPIC_T6,
        "recording-a-group" => TOPIC_T7,
        "cross-mesh-overlap" => TOPIC_T8,
        "empty-groups" => TOPIC_T9,
        "terminal-states" => TOPIC_T11,
        _ => return None,
    })
}

fn render_doc_topic(topic: &str) -> String {
    // §12.12 — full multi-paragraph block, every line `# `-prefixed
    // (blank lines become a bare `#`, matching the §12.2 comment-prefix
    // rule). Unknown topics produce empty output rather than a stub —
    // fail-closed.
    let Some(body) = topic_body(topic) else {
        return String::new();
    };
    let mut out = String::new();
    for line in body.lines() {
        if line.is_empty() {
            out.push_str("#\n");
        } else {
            out.push_str("# ");
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

/// Per-reason `--documentation` hint sentence (§12.11). One short
/// sentence per reason-kind string.
fn render_hint_for_reason(reason: &str) -> Option<String> {
    let body: &str = match reason {
        "partner" => {
            "to re-record a range after edits, run `git mesh add <name> <path>#L<s>-L<e>` and then `git mesh commit <name>`."
        }
        "write_across" => {
            "to re-record a partner that needed matching edits, run `git mesh add <name> <path>#L<s>-L<e>` and then `git mesh commit <name>`."
        }
        "rename_literal" => {
            "to follow a rename, run `git mesh add <name> <new-path>` and then `git mesh commit <name>`."
        }
        "range_collapse" => {
            "to re-record a shrunk extent, run `git mesh rm <name> <path>#L<old-s>-L<old-e>` and then `git mesh add <name> <path>#L<new-s>-L<new-e>`."
        }
        "losing_coherence" => {
            "to narrow or retire a mesh, run `git mesh rm <name> <path>` or `git mesh delete <name>`."
        }
        "symbol_rename" => {
            "to re-record after a symbol rename, run `git mesh add <name> <path>#L<s>-L<e>` and then `git mesh commit <name>`."
        }
        "new_group" => {
            "to record a candidate mesh, run `git mesh add <mesh-name> <path-1> <path-2>`, set `git mesh why <mesh-name> -m \"...\"`, then `git mesh commit <mesh-name>`."
        }
        "staging_cross_cut" => {
            "to resolve a cross-mesh overlap, run `git mesh restore <name>` or `git mesh delete <name>`."
        }
        "empty_mesh" => {
            "to unblock an empty mesh, run `git mesh add <name> <path>` or `git mesh delete <name>`."
        }
        "pending_commit" => return None,
        "terminal" => {
            "to recover from a terminal state, see `git mesh fetch`, finish the merge, or pin the submodule root."
        }
        _ => return None,
    };
    Some(format!("# {body}\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::advice::candidates::candidate_to_suggestion;
    use crate::advice::suggestion::Suggestion;

    /// Build a minimal drift Suggestion via candidate_to_suggestion, mirroring
    /// the old `cand()` helper that built a Candidate.
    fn sugg(mesh: &str, partner: &str) -> Suggestion {
        use crate::advice::candidates::{Candidate, Density as CDensity, ReasonKind as CRK};
        let c = Candidate {
            mesh: mesh.into(),
            mesh_why: "why text".into(),
            reason_kind: CRK::Partner,
            partner_path: partner.into(),
            partner_start: Some(1),
            partner_end: Some(10),
            trigger_path: "t.rs".into(),
            trigger_start: None,
            trigger_end: None,
            touched_path: String::new(),
            touched_start: None,
            touched_end: None,
            partner_marker: String::new(),
            partner_clause: String::new(),
            density: CDensity::L0,
            command: String::new(),
            excerpt_of_path: String::new(),
            excerpt_start: None,
            excerpt_end: None,
            old_blob: None,
            new_blob: None,
            old_path: None,
            new_path: None,
        };
        candidate_to_suggestion(&c)
    }

    #[test]
    fn empty_input_renders_empty_string() {
        assert_eq!(render(&[], &[], false), "");
    }

    #[test]
    fn every_line_prefixed_with_hash() {
        let s = sugg("m1", "b.rs");
        let out = render(&[s], &[], false);
        for line in out.lines() {
            assert!(line.starts_with('#'), "line does not start with #: {line:?}");
        }
    }

    #[test]
    fn mesh_header_and_partner_address() {
        let s = sugg("m1", "b.rs");
        let out = render(&[s], &[], false);
        assert!(out.contains("# m1 mesh: why text"));
        assert!(out.contains("# - b.rs#L1-L10"));
    }

    #[test]
    fn blank_comment_lines_between_blocks() {
        let s1 = sugg("m1", "b.rs");
        let s2 = sugg("m2", "c.rs");
        let out = render(&[s1, s2], &[], false);
        assert!(out.contains("#\n"));
    }

    #[test]
    fn marker_appended_when_present() {
        use crate::advice::candidates::{Candidate, Density as CDensity, ReasonKind as CRK};
        let c = Candidate {
            mesh: "m1".into(),
            mesh_why: "why text".into(),
            reason_kind: CRK::Partner,
            partner_path: "b.rs".into(),
            partner_start: Some(1),
            partner_end: Some(10),
            trigger_path: "t.rs".into(),
            trigger_start: None,
            trigger_end: None,
            touched_path: String::new(),
            touched_start: None,
            touched_end: None,
            partner_marker: "[CHANGED]".into(),
            partner_clause: String::new(),
            density: CDensity::L0,
            command: String::new(),
            excerpt_of_path: String::new(),
            excerpt_start: None,
            excerpt_end: None,
            old_blob: None,
            new_blob: None,
            old_path: None,
            new_path: None,
        };
        let s = candidate_to_suggestion(&c);
        let out = render(&[s], &[], false);
        assert!(out.contains("[CHANGED]"));
    }

    /// Bare render (documentation=false) must NOT emit any doc-topic preamble
    /// (Bug 3). The terminal-states topic must be absent.
    #[test]
    fn bare_render_does_not_emit_doc_topic_preamble() {
        use crate::advice::candidates::{Candidate, Density as CDensity, ReasonKind as CRK};
        let c = Candidate {
            mesh: "m1".into(),
            mesh_why: "why text".into(),
            reason_kind: CRK::Terminal,
            partner_path: "b.rs".into(),
            partner_start: Some(1),
            partner_end: Some(10),
            trigger_path: "t.rs".into(),
            trigger_start: None,
            trigger_end: None,
            touched_path: String::new(),
            touched_start: None,
            touched_end: None,
            partner_marker: "[CHANGED]".into(),
            partner_clause: String::new(),
            density: CDensity::L0,
            command: String::new(),
            excerpt_of_path: String::new(),
            excerpt_start: None,
            excerpt_end: None,
            old_blob: None,
            new_blob: None,
            old_path: None,
            new_path: None,
        };
        let s = candidate_to_suggestion(&c);
        let out = render(&[s], &["terminal-states".into()], false);
        assert!(
            !out.contains("A terminal marker"),
            "bare render must not emit terminal-states topic; got:\n{out}"
        );
        assert!(
            !out.contains("[ORPHANED]"),
            "bare render must not emit terminal-states body; got:\n{out}"
        );
    }

    /// --documentation render must emit the doc-topic preamble (Bug 3).
    #[test]
    fn documentation_render_emits_doc_topic_preamble() {
        use crate::advice::candidates::{Candidate, Density as CDensity, ReasonKind as CRK};
        let c = Candidate {
            mesh: "m1".into(),
            mesh_why: "why text".into(),
            reason_kind: CRK::Terminal,
            partner_path: "b.rs".into(),
            partner_start: Some(1),
            partner_end: Some(10),
            trigger_path: "t.rs".into(),
            trigger_start: None,
            trigger_end: None,
            touched_path: String::new(),
            touched_start: None,
            touched_end: None,
            partner_marker: "[CHANGED]".into(),
            partner_clause: String::new(),
            density: CDensity::L0,
            command: String::new(),
            excerpt_of_path: String::new(),
            excerpt_start: None,
            excerpt_end: None,
            old_blob: None,
            new_blob: None,
            old_path: None,
            new_path: None,
        };
        let s = candidate_to_suggestion(&c);
        let out = render(&[s], &["terminal-states".into()], true);
        assert!(
            out.contains("A terminal marker"),
            "--documentation render must emit terminal-states topic; got:\n{out}"
        );
    }

    /// Partner-drift candidate (trigger_path empty) must render as
    /// `# - a/one.rs [CHANGED]` with NO `# triggered by` line (Bug 4).
    #[test]
    fn partner_drift_renders_bullet_with_marker_no_triggered_by() {
        use crate::advice::candidates::{Candidate, Density as CDensity, ReasonKind as CRK};
        let c = Candidate {
            mesh: "my-mesh".into(),
            mesh_why: "why text".into(),
            reason_kind: CRK::Terminal,
            partner_path: "a/one.rs".into(),
            partner_start: None,
            partner_end: None,
            trigger_path: String::new(), // empty — partner drift
            trigger_start: None,
            trigger_end: None,
            touched_path: String::new(),
            touched_start: None,
            touched_end: None,
            partner_marker: "[CHANGED]".into(),
            partner_clause: String::new(),
            density: CDensity::L0,
            command: String::new(),
            excerpt_of_path: String::new(),
            excerpt_start: None,
            excerpt_end: None,
            old_blob: None,
            new_blob: None,
            old_path: None,
            new_path: None,
        };
        let s = candidate_to_suggestion(&c);
        let out = render(&[s], &[], false);
        assert!(
            out.contains("# - a/one.rs [CHANGED]"),
            "must render partner bullet with marker; got:\n{out}"
        );
        assert!(
            !out.contains("# triggered by"),
            "must not emit triggered-by line for empty trigger_path; got:\n{out}"
        );
    }

    /// Bug 5: a candidate with partner_marker="[RENAMED]" and partner_clause
    /// "was src/foo.ts" must render as:
    ///   `# - src/bar.ts [RENAMED] — was src/foo.ts`
    #[test]
    fn renamed_partner_renders_marker_and_clause() {
        use crate::advice::candidates::{Candidate, Density as CDensity, ReasonKind as CRK};
        let c = Candidate {
            mesh: "link".into(),
            mesh_why: "why text".into(),
            reason_kind: CRK::Partner,
            partner_path: "src/bar.ts".into(),
            partner_start: None,
            partner_end: None,
            trigger_path: "src/uses.ts".into(),
            trigger_start: None,
            trigger_end: None,
            touched_path: String::new(),
            touched_start: None,
            touched_end: None,
            partner_marker: "[RENAMED]".into(),
            partner_clause: "was src/foo.ts".into(),
            density: CDensity::L0,
            command: String::new(),
            excerpt_of_path: String::new(),
            excerpt_start: None,
            excerpt_end: None,
            old_blob: None,
            new_blob: None,
            old_path: None,
            new_path: None,
        };
        let s = candidate_to_suggestion(&c);
        let out = render(&[s], &[], false);
        assert!(
            out.contains("# - src/bar.ts [RENAMED] — was src/foo.ts"),
            "must render renamed partner with marker and clause; got:\n{out}"
        );
    }

    /// Bug 5: an L2 RenameLiteral candidate with a command must emit the
    /// command block with the correct lead-in.
    #[test]
    fn rename_literal_l2_renders_command_block() {
        use crate::advice::candidates::{Candidate, Density as CDensity, ReasonKind as CRK};
        let c = Candidate {
            mesh: "link".into(),
            mesh_why: "why text".into(),
            reason_kind: CRK::RenameLiteral,
            partner_path: "src/bar.ts".into(),
            partner_start: None,
            partner_end: None,
            trigger_path: "src/bar.ts".into(),
            trigger_start: None,
            trigger_end: None,
            touched_path: String::new(),
            touched_start: None,
            touched_end: None,
            partner_marker: String::new(),
            partner_clause: String::new(),
            density: CDensity::L2,
            command: "git mesh rm  link src/foo.ts\ngit mesh add link src/bar.ts\ngit mesh commit link".into(),
            excerpt_of_path: String::new(),
            excerpt_start: None,
            excerpt_end: None,
            old_blob: None,
            new_blob: None,
            old_path: None,
            new_path: None,
        };
        let s = candidate_to_suggestion(&c);
        let out = render(&[s], &[], false);
        assert!(
            out.contains("# to re-record after the rename, run:"),
            "must emit rename lead-in; got:\n{out}"
        );
        assert!(
            out.contains("#   git mesh rm  link src/foo.ts"),
            "must emit rm command; got:\n{out}"
        );
        assert!(
            out.contains("#   git mesh add link src/bar.ts"),
            "must emit add command; got:\n{out}"
        );
        assert!(
            out.contains("#   git mesh commit link"),
            "must emit commit command; got:\n{out}"
        );
    }

    /// A whole-file partner (partner_start=None, partner_end=None) must render
    /// as a bare path with no `#L…` suffix (Bug 1).
    #[test]
    fn whole_file_partner_renders_without_line_suffix() {
        use crate::advice::candidates::{Candidate, Density as CDensity, ReasonKind as CRK};
        let c = Candidate {
            mesh: "checkout-flow".into(),
            mesh_why: "why text".into(),
            reason_kind: CRK::Partner,
            partner_path: "api/charge.ts".into(),
            partner_start: None,
            partner_end: None,
            trigger_path: "t.rs".into(),
            trigger_start: None,
            trigger_end: None,
            touched_path: String::new(),
            touched_start: None,
            touched_end: None,
            partner_marker: String::new(),
            partner_clause: String::new(),
            density: CDensity::L0,
            command: String::new(),
            excerpt_of_path: String::new(),
            excerpt_start: None,
            excerpt_end: None,
            old_blob: None,
            new_blob: None,
            old_path: None,
            new_path: None,
        };
        let s = candidate_to_suggestion(&c);
        let out = render(&[s], &[], false);
        assert!(
            out.contains("# - api/charge.ts\n"),
            "whole-file partner must render as bare path; got:\n{out}"
        );
        assert!(
            !out.contains("api/charge.ts#L"),
            "whole-file partner must not have #L suffix; got:\n{out}"
        );
    }

    /// Regression: rendered output must never contain the word "group" (case-insensitive,
    /// whole word) in user-visible text, for any ReasonKind, with or without --documentation.
    /// Internal symbol names (REASON_NEW_GROUP, new_group, etc.) are not rendered to output.
    #[test]
    fn no_group_word_in_rendered_output() {
        use crate::advice::candidates::{Candidate, Density as CDensity, ReasonKind as CRK};
        let all_kinds = [
            CRK::Partner,
            CRK::WriteAcross,
            CRK::RenameLiteral,
            CRK::RangeCollapse,
            CRK::LosingCoherence,
            CRK::SymbolRename,
            CRK::NewGroup,
            CRK::StagingCrossCut,
            CRK::EmptyMesh,
            CRK::PendingCommit,
            CRK::Terminal,
        ];
        let re = regex_word_group();
        for kind in &all_kinds {
            let c = Candidate {
                mesh: "test-mesh".into(),
                mesh_why: "why text".into(),
                reason_kind: *kind,
                partner_path: "b.rs".into(),
                partner_start: Some(1),
                partner_end: Some(5),
                trigger_path: "t.rs".into(),
                trigger_start: None,
                trigger_end: None,
                touched_path: String::new(),
                touched_start: None,
                touched_end: None,
                partner_marker: String::new(),
                partner_clause: String::new(),
                density: CDensity::L0,
                command: String::new(),
                excerpt_of_path: String::new(),
                excerpt_start: None,
                excerpt_end: None,
                old_blob: None,
                new_blob: None,
                old_path: None,
                new_path: None,
            };
            let s = candidate_to_suggestion(&c);
            let topics: Vec<String> = kind.doc_topic().into_iter().map(str::to_string).collect();
            // Without --documentation
            let out_bare = render(&[s.clone()], &[], false);
            assert!(
                !re(&out_bare),
                "bare render for {:?} contains the word 'group': {:?}",
                kind, out_bare
            );
            // With --documentation
            let out_doc = render(&[s], &topics, true);
            assert!(
                !re(&out_doc),
                "--documentation render for {:?} contains the word 'group': {:?}",
                kind, out_doc
            );
        }
    }

    /// Returns a simple pattern that matches the word "group" (case-insensitive,
    /// whole-word boundaries using ASCII word-character assumptions).
    fn regex_word_group() -> impl Fn(&str) -> bool {
        |text: &str| {
            let lower = text.to_lowercase();
            let bytes = lower.as_bytes();
            let needle = b"group";
            let n = needle.len();
            let mut i = 0;
            while i + n <= bytes.len() {
                if &bytes[i..i + n] == needle {
                    let before_ok = i == 0 || !bytes[i - 1].is_ascii_alphanumeric();
                    let after_ok = i + n >= bytes.len() || !bytes[i + n].is_ascii_alphanumeric();
                    if before_ok && after_ok {
                        return true;
                    }
                }
                i += 1;
            }
            false
        }
    }
}

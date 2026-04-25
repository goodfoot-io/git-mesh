//! Structural intersections (T1–T11).
//!
//! Each detector queries the advice DB — `{read,write,commit}_events`,
//! `mesh_ranges`, seen-set — together with the current gix repo to emit
//! `Candidate` rows. One `Candidate` per (mesh, reason-kind, partner,
//! trigger) tuple; the flush pipeline dedups against `flush_additions`.

use anyhow::Result;
use rusqlite::Connection;

/// Density ladder — §12.5.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Density {
    /// Partner list only.
    L0,
    /// Partner list + one excerpt.
    L1,
    /// Partner list + excerpt + ready-to-run command.
    L2,
}

/// Reason-kind: matches the T1…T11 message-type inventory. Used as a
/// stable dedup key and as the key for per-reason doc topics.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ReasonKind {
    /// T1 partner list.
    Partner,
    /// T2 partner excerpt on write.
    WriteAcross,
    /// T3 rename literal in partner.
    RenameLiteral,
    /// T4 range collapse on partner.
    RangeCollapse,
    /// T5 losing coherence.
    LosingCoherence,
    /// T6 symbol rename hits in partner.
    SymbolRename,
    /// T7 new-group candidate.
    NewGroup,
    /// T8 staging cross-cut.
    StagingCrossCut,
    /// T9 empty-mesh risk.
    EmptyMesh,
    /// T10 pending-commit re-anchor.
    PendingCommit,
    /// T11 terminal status.
    Terminal,
}

impl ReasonKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ReasonKind::Partner => "partner",
            ReasonKind::WriteAcross => "write_across",
            ReasonKind::RenameLiteral => "rename_literal",
            ReasonKind::RangeCollapse => "range_collapse",
            ReasonKind::LosingCoherence => "losing_coherence",
            ReasonKind::SymbolRename => "symbol_rename",
            ReasonKind::NewGroup => "new_group",
            ReasonKind::StagingCrossCut => "staging_cross_cut",
            ReasonKind::EmptyMesh => "empty_mesh",
            ReasonKind::PendingCommit => "pending_commit",
            ReasonKind::Terminal => "terminal",
        }
    }

    pub fn doc_topic(self) -> Option<&'static str> {
        match self {
            ReasonKind::Partner => None, // L0 — no topic
            ReasonKind::WriteAcross => Some("editing across files"),
            ReasonKind::RenameLiteral => Some("renames"),
            ReasonKind::RangeCollapse => Some("shrinking ranges"),
            ReasonKind::LosingCoherence => Some("narrow or retire"),
            ReasonKind::SymbolRename => Some("exported symbols"),
            ReasonKind::NewGroup => Some("recording a group"),
            ReasonKind::StagingCrossCut => Some("cross-mesh overlap"),
            ReasonKind::EmptyMesh => Some("empty groups"),
            ReasonKind::PendingCommit => None, // L0 — no topic
            ReasonKind::Terminal => Some("terminal states"),
        }
    }

    pub fn default_density(self) -> Density {
        match self {
            ReasonKind::Partner | ReasonKind::PendingCommit | ReasonKind::Terminal => Density::L0,
            ReasonKind::WriteAcross => Density::L1,
            _ => Density::L2,
        }
    }
}

/// A surfacing candidate — one row per (mesh, reason, partner, trigger).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    pub mesh: String,
    pub mesh_why: String,
    pub reason_kind: ReasonKind,
    pub partner_path: String,
    pub partner_start: Option<i64>,
    pub partner_end: Option<i64>,
    /// The file the developer just touched (trigger range) — only used for
    /// dedup and for the command text. May be empty.
    pub trigger_path: String,
    pub trigger_start: Option<i64>,
    pub trigger_end: Option<i64>,
    /// Bracket marker appended to the partner line (CHANGED, STAGED, …).
    /// Empty = no marker.
    pub partner_marker: String,
    /// Prose clause after an em-dash on the partner line. Empty = none.
    pub partner_clause: String,
    pub density: Density,
    /// Optional ready-to-run command (L2). Empty for L0/L1.
    pub command: String,
    /// L1/L2 excerpt block attached to a specific partner path+range. Empty
    /// for L0.
    pub excerpt_of_path: String,
    pub excerpt_start: Option<i64>,
    pub excerpt_end: Option<i64>,
}

impl Candidate {
    fn bare(
        mesh: &str,
        mesh_why: &str,
        kind: ReasonKind,
        partner_path: &str,
        partner_start: Option<i64>,
        partner_end: Option<i64>,
        trigger_path: &str,
    ) -> Self {
        Self {
            mesh: mesh.to_string(),
            mesh_why: mesh_why.to_string(),
            reason_kind: kind,
            partner_path: partner_path.to_string(),
            partner_start,
            partner_end,
            trigger_path: trigger_path.to_string(),
            trigger_start: None,
            trigger_end: None,
            partner_marker: String::new(),
            partner_clause: String::new(),
            density: kind.default_density(),
            command: String::new(),
            excerpt_of_path: String::new(),
            excerpt_start: None,
            excerpt_end: None,
        }
    }
}

/// A single row from the flush-scoped `mesh_ranges` snapshot.
#[derive(Clone, Debug)]
pub struct MeshRangeRow {
    pub mesh: String,
    pub path: String,
    pub start_line: Option<i64>,
    pub end_line: Option<i64>,
    pub status: String,
    pub source: String,
    pub ack: bool,
    /// Slice 4: `""` for committed ranges, `"add"` for a staged add not
    /// yet committed (`source == "S"`), `"remove"` for a committed range
    /// that has a matching staged remove pending.
    pub staged_op: String,
}

/// Load all mesh_ranges rows from the DB, indexed by mesh.
pub fn load_mesh_ranges(conn: &Connection) -> Result<Vec<MeshRangeRow>> {
    let mut stmt = conn.prepare(
        "SELECT mesh, path, start_line, end_line, COALESCE(status,''), COALESCE(source,''), \
                ack, COALESCE(staged_op,'') \
         FROM mesh_ranges",
    )?;
    let rows = stmt
        .query_map([], |r| {
            Ok(MeshRangeRow {
                mesh: r.get(0)?,
                path: r.get(1)?,
                start_line: r.get(2)?,
                end_line: r.get(3)?,
                status: r.get(4)?,
                source: r.get(5)?,
                ack: {
                    let v: i64 = r.get(6)?;
                    v != 0
                },
                staged_op: r.get(7)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Collect the distinct touched paths from read+write events.
pub fn session_touched_paths(conn: &Connection) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT path FROM (\
           SELECT path FROM read_events WHERE path IS NOT NULL \
           UNION \
           SELECT path FROM write_events WHERE path IS NOT NULL)",
    )?;
    let rows = stmt
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Mesh why-messages from a snapshot-style lookup — populated from
/// `stale_meshes` output during flush and persisted on the `mesh_ranges`
/// side channel. For simplicity we accept a map at call time.
pub type WhyMap = std::collections::HashMap<String, String>;

fn why_for(whys: &WhyMap, mesh: &str) -> String {
    whys.get(mesh).cloned().unwrap_or_default()
}

fn marker_for_status(status: &str) -> &'static str {
    match status {
        "CHANGED" => "[CHANGED]",
        "MOVED" => "[MOVED]",
        "ORPHANED" => "[ORPHANED]",
        "MERGE_CONFLICT" => "[CONFLICT]",
        "SUBMODULE" => "[SUBMODULE]",
        _ => "",
    }
}

// ---------------------------------------------------------------------------
// T1 — partner list. Baseline: any touched path intersecting a mesh.
// ---------------------------------------------------------------------------

/// Override the partner marker to `[DELETED]` when the partner path does
/// not exist in the worktree. Per `docs/advice-notes.md` §12.4 a deleted
/// file takes a `[DELETED]` marker regardless of how the resolver
/// classified the range (resolver typically reports `CHANGED` or
/// `ORPHANED`).
fn marker_with_deletion(repo: &gix::Repository, partner_path: &str, fallback: &str) -> String {
    if let Ok(wd) = crate::git::work_dir(repo)
        && !wd.join(partner_path).exists()
    {
        return "[DELETED]".to_string();
    }
    fallback.to_string()
}

/// Compose markers per §12.4. Staging precedes status: a staged add
/// renders as `[STAGED]`, combined with `[CHANGED]` etc. as `[STAGED]
/// [CHANGED]`. Documented marker order: staging first, then status.
fn compose_marker(base_marker: &str, partner: &MeshRangeRow) -> String {
    if partner.staged_op == "add" {
        if base_marker.is_empty() {
            return "[STAGED]".to_string();
        }
        return format!("[STAGED] {base_marker}");
    }
    base_marker.to_string()
}

pub(crate) fn detect_t1(
    conn: &Connection,
    repo: &gix::Repository,
    mesh_ranges: &[MeshRangeRow],
    whys: &WhyMap,
) -> Result<Vec<Candidate>> {
    let touched = session_touched_paths(conn)?;
    let mut out = Vec::new();

    // Meshes that have a range on any touched path.
    let mut meshes_touching: std::collections::BTreeSet<String> =
        std::collections::BTreeSet::new();
    for row in mesh_ranges {
        if touched.iter().any(|t| t == &row.path) {
            meshes_touching.insert(row.mesh.clone());
        }
    }

    for mesh in &meshes_touching {
        // Find trigger path(s) for this mesh — touched paths that the mesh
        // has a range on. Emit one Candidate per (partner, trigger) combo,
        // where partner ranges are the *other* ranges in the mesh.
        let mesh_rows: Vec<&MeshRangeRow> =
            mesh_ranges.iter().filter(|r| &r.mesh == mesh).collect();
        let triggers: Vec<&MeshRangeRow> = mesh_rows
            .iter()
            .copied()
            .filter(|r| touched.iter().any(|t| t == &r.path))
            .collect();
        for trigger in &triggers {
            for partner in &mesh_rows {
                if partner.path == trigger.path
                    && partner.start_line == trigger.start_line
                    && partner.end_line == trigger.end_line
                {
                    continue;
                }
                let mut c = Candidate::bare(
                    mesh,
                    &why_for(whys, mesh),
                    ReasonKind::Partner,
                    &partner.path,
                    partner.start_line,
                    partner.end_line,
                    &trigger.path,
                );
                c.trigger_start = trigger.start_line;
                c.trigger_end = trigger.end_line;
                let base_marker = marker_for_status(&partner.status);
                let with_del = marker_with_deletion(repo, &partner.path, base_marker);
                c.partner_marker = compose_marker(&with_del, partner);
                if matches!(
                    partner.status.as_str(),
                    "ORPHANED" | "MERGE_CONFLICT" | "SUBMODULE"
                ) && !c.partner_marker.contains("[DELETED]")
                {
                    // T11 terminal promotes this reason-kind.
                    c.reason_kind = ReasonKind::Terminal;
                }
                out.push(c);
            }
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// T2 — partner excerpt on write. A write event whose path lives in a mesh
// produces an L1 candidate with excerpts of the *other* partners.
// ---------------------------------------------------------------------------

pub(crate) fn detect_t2(
    conn: &Connection,
    repo: &gix::Repository,
    mesh_ranges: &[MeshRangeRow],
    whys: &WhyMap,
) -> Result<Vec<Candidate>> {
    let mut out = Vec::new();
    let mut stmt = conn.prepare(
        "SELECT DISTINCT path FROM write_events WHERE path IS NOT NULL",
    )?;
    let written: Vec<String> = stmt
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    drop(stmt);

    let mut meshes_touching: std::collections::BTreeSet<String> =
        std::collections::BTreeSet::new();
    for row in mesh_ranges {
        if written.iter().any(|w| w == &row.path) {
            meshes_touching.insert(row.mesh.clone());
        }
    }

    for mesh in &meshes_touching {
        let mesh_rows: Vec<&MeshRangeRow> =
            mesh_ranges.iter().filter(|r| &r.mesh == mesh).collect();
        let triggers: Vec<&MeshRangeRow> = mesh_rows
            .iter()
            .copied()
            .filter(|r| written.iter().any(|w| w == &r.path))
            .collect();
        for trigger in &triggers {
            for partner in &mesh_rows {
                if partner.path == trigger.path {
                    continue;
                }
                let mut c = Candidate::bare(
                    mesh,
                    &why_for(whys, mesh),
                    ReasonKind::WriteAcross,
                    &partner.path,
                    partner.start_line,
                    partner.end_line,
                    &trigger.path,
                );
                c.trigger_start = trigger.start_line;
                c.trigger_end = trigger.end_line;
                let base_marker = marker_for_status(&partner.status);
                let with_del = marker_with_deletion(repo, &partner.path, base_marker);
                c.partner_marker = compose_marker(&with_del, partner);
                c.excerpt_of_path = partner.path.clone();
                c.excerpt_start = partner.start_line;
                c.excerpt_end = partner.end_line;
                c.density = Density::L1;
                out.push(c);
            }
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// T3 — rename literal in partner. Given a write event whose path is a mesh
// path, and whose pre→post blob reveals a rename of a co-mesh path/basename,
// surface partners that still contain the old literal.
// We keep this conservative: check if any partner range contains the
// (current) basename of another mesh path — as a stand-in, we surface
// partners whose body contains the trigger's basename as a plain substring
// and flag only when an edit touched the trigger.
//
// Greenfield scope: full rename detection via gix `--follow` is out; we
// structurally detect the pattern plan §11.7 targets — "partner contains a
// mesh-member path/basename as literal" — over the mesh itself.
// ---------------------------------------------------------------------------

pub(crate) fn detect_t3(
    conn: &Connection,
    repo: &gix::Repository,
    mesh_ranges: &[MeshRangeRow],
    whys: &WhyMap,
) -> Result<Vec<Candidate>> {
    let mut out = Vec::new();
    // meshes with ≥2 ranges; for each pair (a, b), if the body of a's range
    // contains b's basename as a literal and the session touched b, emit.
    let mut stmt =
        conn.prepare("SELECT DISTINCT path FROM write_events WHERE path IS NOT NULL")?;
    let written: Vec<String> = stmt
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    drop(stmt);
    if written.is_empty() {
        return Ok(out);
    }

    let mut by_mesh: std::collections::BTreeMap<String, Vec<&MeshRangeRow>> =
        std::collections::BTreeMap::new();
    for row in mesh_ranges {
        by_mesh.entry(row.mesh.clone()).or_default().push(row);
    }

    for (mesh, rows) in &by_mesh {
        if rows.len() < 2 {
            continue;
        }
        for trigger in rows {
            if !written.iter().any(|w| w == &trigger.path) {
                continue;
            }
            let tbase =
                std::path::Path::new(&trigger.path).file_name().map(|s| s.to_string_lossy().into_owned());
            let Some(tbase) = tbase else { continue };
            if tbase.is_empty() {
                continue;
            }
            for partner in rows {
                if partner.path == trigger.path {
                    continue;
                }
                let Some(body) = read_partner_bytes(repo, partner) else {
                    continue;
                };
                if body.contains(&tbase) {
                    let mut c = Candidate::bare(
                        mesh,
                        &why_for(whys, mesh),
                        ReasonKind::RenameLiteral,
                        &partner.path,
                        partner.start_line,
                        partner.end_line,
                        &trigger.path,
                    );
                    c.trigger_start = trigger.start_line;
                    c.trigger_end = trigger.end_line;
                    c.partner_clause = format!("still references \"{tbase}\"");
                    c.excerpt_of_path = partner.path.clone();
                    c.excerpt_start = partner.start_line;
                    c.excerpt_end = partner.end_line;
                    c.density = Density::L2;
                    c.command = format!(
                        "git mesh add {mesh} {}{}",
                        trigger.path,
                        addr_suffix(trigger.start_line, trigger.end_line)
                    );
                    out.push(c);
                }
            }
        }
    }
    Ok(out)
}

/// T3 (commit-rename variant) — for each `--commit <sha>` event recorded
/// in the session, detect file renames via `gix` and, when the OLD path
/// is meshed, surface partner ranges that still reference the OLD path
/// or its basename as a literal substring.
///
/// Marker ordering (slice 4, decision): we leave the renamed-asset
/// partner address bare (no marker; the rename is the news on the body
/// side). The literal-bearing partner takes the spec clause "— still
/// references "<old>"". gix's default rename detection (50% similarity)
/// is used; called out in the slice report.
pub(crate) fn detect_t3_renames(
    conn: &Connection,
    repo: &gix::Repository,
    mesh_ranges: &[MeshRangeRow],
    whys: &WhyMap,
) -> Result<Vec<Candidate>> {
    let mut out = Vec::new();
    let mut stmt = conn.prepare("SELECT sha FROM commit_events WHERE sha IS NOT NULL")?;
    let shas: Vec<String> = stmt
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    drop(stmt);
    if shas.is_empty() {
        return Ok(out);
    }

    let meshed_paths: std::collections::HashSet<&str> =
        mesh_ranges.iter().map(|r| r.path.as_str()).collect();

    let mut by_mesh: std::collections::BTreeMap<String, Vec<&MeshRangeRow>> =
        std::collections::BTreeMap::new();
    for row in mesh_ranges {
        by_mesh.entry(row.mesh.clone()).or_default().push(row);
    }

    for sha in &shas {
        let renames = match commit_rename_pairs(repo, sha) {
            Ok(rs) => rs,
            Err(_) => continue,
        };
        for (old_path, new_path) in renames {
            if !meshed_paths.contains(old_path.as_str()) {
                continue;
            }
            let old_base = std::path::Path::new(&old_path)
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            // For each mesh that includes the old path, scan partners.
            for (mesh, rows) in &by_mesh {
                if !rows.iter().any(|r| r.path == old_path) {
                    continue;
                }
                for partner in rows {
                    if partner.path == old_path {
                        continue;
                    }
                    let Some(body) = read_partner_bytes(repo, partner) else {
                        continue;
                    };
                    let mut hit_literal: Option<String> = None;
                    if body.contains(&old_path) {
                        hit_literal = Some(old_path.clone());
                    } else if !old_base.is_empty() && body.contains(&old_base) {
                        hit_literal = Some(old_base.clone());
                    }
                    let Some(literal) = hit_literal else { continue };

                    let mut c = Candidate::bare(
                        mesh,
                        &why_for(whys, mesh),
                        ReasonKind::RenameLiteral,
                        &partner.path,
                        partner.start_line,
                        partner.end_line,
                        &old_path,
                    );
                    c.partner_clause = format!("still references \"{literal}\"");
                    c.excerpt_of_path = partner.path.clone();
                    c.excerpt_start = partner.start_line;
                    c.excerpt_end = partner.end_line;
                    c.density = Density::L2;
                    c.command = format!("git mesh add {mesh} {new_path}");
                    c.trigger_start = None;
                    c.trigger_end = None;
                    out.push(c);

                    // Companion address-only entry for the renamed-to
                    // path. Same mesh, same reason-kind, but unique
                    // (partner_path, trigger_path) so it survives dedup.
                    let mut companion = Candidate::bare(
                        mesh,
                        &why_for(whys, mesh),
                        ReasonKind::RenameLiteral,
                        &new_path,
                        None,
                        None,
                        &old_path,
                    );
                    companion.density = Density::L0;
                    out.push(companion);
                }
            }
        }
    }
    Ok(out)
}

/// Walk one commit and return `(old_path, new_path)` rename pairs against
/// its first parent. Uses `gix`'s default rename detection (50% similarity).
fn commit_rename_pairs(repo: &gix::Repository, sha: &str) -> Result<Vec<(String, String)>> {
    use gix::object::tree::diff::Change as DC;
    let oid = repo
        .rev_parse_single(sha)
        .map_err(|e| anyhow::anyhow!("resolve {sha}: {e}"))?
        .detach();
    let commit = repo.find_object(oid)?.try_into_commit()?;
    let Some(parent_id) = commit.parent_ids().next() else {
        return Ok(Vec::new());
    };
    let parent = repo.find_object(parent_id)?.try_into_commit()?;
    let parent_tree = parent.tree()?;
    let new_tree = commit.tree()?;
    let mut platform = parent_tree.changes()?;
    platform.options(|opts| {
        opts.track_path().track_rewrites(Some(gix::diff::Rewrites {
            copies: None,
            percentage: Some(0.5),
            limit: 1000,
            track_empty: false,
        }));
    });
    let mut out: Vec<(String, String)> = Vec::new();
    platform
        .for_each_to_obtain_tree(&new_tree, |change| -> Result<std::ops::ControlFlow<()>> {
            if let DC::Rewrite {
                source_location,
                location,
                copy,
                ..
            } = change
                && !copy
            {
                out.push((source_location.to_string(), location.to_string()));
            }
            Ok(std::ops::ControlFlow::Continue(()))
        })
        .ok();
    Ok(out)
}

// ---------------------------------------------------------------------------
// T4 — range collapse. Post-edit extent ≤ 25% of anchored extent (skip
// single-line anchors).
// ---------------------------------------------------------------------------

pub(crate) fn detect_t4(
    conn: &Connection,
    mesh_ranges: &[MeshRangeRow],
    whys: &WhyMap,
) -> Result<Vec<Candidate>> {
    let mut out = Vec::new();
    // T4 requires `--post` content to be present; per slice 3 (and §13's
    // resolved threshold) we fail-closed on missing post content. This
    // makes T4 a deliberate "I observed the post bytes" signal rather than
    // a heuristic guess from the recorded write extent.
    //
    // Threshold: post_extent ≤ 50% of recorded extent AND
    //            recorded_extent − post_extent ≥ 2 lines.
    let mut stmt = conn.prepare(
        "SELECT DISTINCT path, start_line, end_line, post_blob FROM write_events \
         WHERE path IS NOT NULL AND start_line IS NOT NULL AND end_line IS NOT NULL \
           AND post_blob IS NOT NULL",
    )?;
    let writes: Vec<(String, i64, i64, String)> = stmt
        .query_map([], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    drop(stmt);

    for (wpath, ws, we, post_blob) in &writes {
        // Post extent: the span of the post content that overlaps the
        // recorded mesh range. We use the post line count (clamped) as the
        // new extent of the touched range and anchor it at the mesh
        // range's recorded start.
        let post_lines = post_blob.lines().count() as i64;
        for row in mesh_ranges {
            if &row.path != wpath {
                continue;
            }
            let (Some(ms), Some(me)) = (row.start_line, row.end_line) else {
                continue;
            };
            let m_extent = (me - ms + 1).max(1);
            if m_extent <= 1 {
                continue; // skip single-line anchors
            }
            // overlap?
            if we < &ms || ws > &me {
                continue;
            }
            // Post extent within the recorded range. Clamp so it is at
            // most the recorded extent (the post can't make the recorded
            // range larger by definition; we measure shrinkage only).
            let post_extent = post_lines.clamp(0, m_extent);
            // §13 threshold: post ≤ 50% of recorded AND difference ≥ 2.
            if post_extent * 2 > m_extent {
                continue;
            }
            if m_extent - post_extent < 2 {
                continue;
            }
            // New extent anchored at the recorded start.
            let new_s = ms;
            let new_e = (ms + post_extent - 1).max(ms);
            // Collapse detected — surface the *partner* (other ranges in the
            // mesh), per plan §11.8.
            for partner in mesh_ranges.iter().filter(|r| r.mesh == row.mesh) {
                if partner.path == row.path
                    && partner.start_line == row.start_line
                    && partner.end_line == row.end_line
                {
                    continue;
                }
                let mut c = Candidate::bare(
                    &row.mesh,
                    &why_for(whys, &row.mesh),
                    ReasonKind::RangeCollapse,
                    &partner.path,
                    partner.start_line,
                    partner.end_line,
                    wpath,
                );
                c.trigger_start = Some(*ws);
                c.trigger_end = Some(*we);
                c.excerpt_of_path = partner.path.clone();
                c.excerpt_start = partner.start_line;
                c.excerpt_end = partner.end_line;
                c.density = Density::L2;
                c.command = format!(
                    "git mesh rm {m} {p}#L{ms}-L{me}\ngit mesh add {m} {p}#L{ns}-L{ne}",
                    m = row.mesh,
                    p = row.path,
                    ms = ms,
                    me = me,
                    ns = new_s,
                    ne = new_e,
                );
                out.push(c);
            }
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// T5 — losing coherence. Mesh has any FRESH range and all other ranges are
// non-FRESH (CHANGED/DELETED/MOVED/…). Trigger is any touched path on that
// mesh.
// ---------------------------------------------------------------------------

pub(crate) fn detect_t5(
    conn: &Connection,
    mesh_ranges: &[MeshRangeRow],
    whys: &WhyMap,
) -> Result<Vec<Candidate>> {
    let mut out = Vec::new();
    let touched = session_touched_paths(conn)?;
    let mut by_mesh: std::collections::BTreeMap<String, Vec<&MeshRangeRow>> =
        std::collections::BTreeMap::new();
    for row in mesh_ranges {
        by_mesh.entry(row.mesh.clone()).or_default().push(row);
    }
    for (mesh, rows) in &by_mesh {
        if rows.len() < 2 {
            continue;
        }
        let fresh_count = rows.iter().filter(|r| r.status == "FRESH").count();
        let non_fresh = rows.len() - fresh_count;
        if !(fresh_count >= 1 && non_fresh >= rows.len().saturating_sub(1) && non_fresh >= 2) {
            continue;
        }
        // Require at least one touched path on this mesh.
        let Some(trigger) = rows.iter().find(|r| touched.iter().any(|t| t == &r.path)) else {
            continue;
        };
        for partner in rows {
            let mut c = Candidate::bare(
                mesh,
                &why_for(whys, mesh),
                ReasonKind::LosingCoherence,
                &partner.path,
                partner.start_line,
                partner.end_line,
                &trigger.path,
            );
            c.trigger_start = trigger.start_line;
            c.trigger_end = trigger.end_line;
            c.partner_marker = marker_for_status(&partner.status).to_string();
            c.density = Density::L2;
            c.command = format!("git mesh rm {mesh} <path>");
            out.push(c);
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// T6 — symbol rename hits in partner. Heuristic: write events whose
// pre→post blobs differ in exported symbol names that appear as
// word-boundary matches in partner range bodies.
// ---------------------------------------------------------------------------

/// Extract identifiers that immediately follow exported-symbol keywords.
/// Recognised patterns (ASCII identifiers only):
///   pub fn <id>   export function <id>   export class <id>
///   export const <id>   export let <id>  export default function <id>
///   export default class <id>            export <id> (bare re-export)
fn extract_exported_symbols(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    // Tokenise by whitespace boundaries so we can do a sliding-window scan.
    let tokens: Vec<&str> = text.split_ascii_whitespace().collect();
    let mut i = 0;
    while i < tokens.len() {
        let t = tokens[i];
        // Match keyword patterns; advance i and capture the identifier.
        let ident_idx = if t == "pub" && tokens.get(i + 1).copied() == Some("fn") {
            // pub fn <id>
            i += 2;
            Some(i)
        } else if t == "export" {
            match tokens.get(i + 1).copied() {
                Some("function") | Some("class") => {
                    i += 2;
                    Some(i)
                }
                Some("const") | Some("let") | Some("var") => {
                    i += 2;
                    Some(i)
                }
                Some("default") => {
                    match tokens.get(i + 2).copied() {
                        Some("function") | Some("class") => {
                            i += 3;
                            Some(i)
                        }
                        _ => {
                            i += 1;
                            None
                        }
                    }
                }
                Some(candidate) if is_ident(candidate) => {
                    // bare `export <id>` — only if it looks like an identifier
                    i += 2;
                    Some(i)
                }
                Some(_) => {
                    i += 1;
                    None
                }
                None => {
                    i += 1;
                    None
                }
            }
        } else {
            i += 1;
            None
        };

        if let Some(idx) = ident_idx
            && let Some(raw) = tokens.get(idx)
        {
            // Strip trailing punctuation (parens, braces, semicolons).
            let clean = raw.trim_end_matches(|c: char| !c.is_alphanumeric() && c != '_');
            if is_ident(clean) {
                out.push(clean.to_string());
            }
        }
    }
    out
}

fn is_ident(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {
            chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
        }
        _ => false,
    }
}

/// Return true iff `name` appears as a word-boundary token in `body`.
fn contains_word(body: &str, name: &str) -> bool {
    // Simple linear scan: look for `name` surrounded by non-ident chars.
    let nb = name.as_bytes();
    let bb = body.as_bytes();
    if nb.is_empty() || bb.len() < nb.len() {
        return false;
    }
    'outer: for start in 0..=(bb.len() - nb.len()) {
        if &bb[start..start + nb.len()] != nb {
            continue;
        }
        // Check left boundary.
        if start > 0 {
            let prev = bb[start - 1] as char;
            if prev.is_ascii_alphanumeric() || prev == '_' {
                continue 'outer;
            }
        }
        // Check right boundary.
        let end = start + nb.len();
        if end < bb.len() {
            let next = bb[end] as char;
            if next.is_ascii_alphanumeric() || next == '_' {
                continue 'outer;
            }
        }
        return true;
    }
    false
}

/// Core T6 logic: given a map from partner path to its body text, emit
/// candidates for symbol renames found in pre→post blob diffs.
/// Extracted as a pure function so unit tests can supply synthetic bodies
/// without a gix repository.
fn detect_t6_inner(
    written_path: &str,
    pre_blob: &str,
    post_blob: &str,
    mesh_ranges: &[MeshRangeRow],
    whys: &WhyMap,
    partner_body: &dyn Fn(&MeshRangeRow) -> Option<String>,
) -> Vec<Candidate> {
    let mut out = Vec::new();

    let pre_syms: std::collections::HashSet<String> =
        extract_exported_symbols(pre_blob).into_iter().collect();
    let post_syms: std::collections::HashSet<String> =
        extract_exported_symbols(post_blob).into_iter().collect();

    let mut old_names: Vec<String> = pre_syms.difference(&post_syms).cloned().collect();
    let new_names: std::collections::HashSet<&String> = post_syms.difference(&pre_syms).collect();

    if old_names.is_empty() || new_names.is_empty() {
        return out;
    }
    old_names.sort();
    old_names.truncate(3);

    // Group by mesh.
    let mut by_mesh: std::collections::BTreeMap<String, Vec<&MeshRangeRow>> =
        std::collections::BTreeMap::new();
    for row in mesh_ranges {
        by_mesh.entry(row.mesh.clone()).or_default().push(row);
    }

    // Find meshes that include the written path.
    let meshes_with_path: Vec<&str> = mesh_ranges
        .iter()
        .filter(|r| r.path == written_path)
        .map(|r| r.mesh.as_str())
        .collect();

    for mesh in &meshes_with_path {
        let Some(rows) = by_mesh.get(*mesh) else {
            continue;
        };
        let trigger_row = rows.iter().find(|r| r.path == written_path);
        let (tstart, tend) = trigger_row
            .map(|r| (r.start_line, r.end_line))
            .unwrap_or((None, None));

        for partner in rows {
            if partner.path == written_path {
                continue;
            }
            let Some(body) = partner_body(partner) else {
                continue;
            };
            for old_name in &old_names {
                if !contains_word(&body, old_name) {
                    continue;
                }
                let mut c = Candidate::bare(
                    mesh,
                    &why_for(whys, mesh),
                    ReasonKind::SymbolRename,
                    &partner.path,
                    partner.start_line,
                    partner.end_line,
                    written_path,
                );
                c.trigger_start = tstart;
                c.trigger_end = tend;
                c.partner_clause =
                    format!("still references \"{old_name}\" (renamed in {written_path})");
                c.density = Density::L2;
                c.command = format!(
                    "git mesh add {mesh} {}{}",
                    partner.path,
                    addr_suffix(partner.start_line, partner.end_line)
                );
                out.push(c);
                break; // one candidate per (mesh, partner, trigger) — first hit
            }
        }
    }
    out
}

pub(crate) fn detect_t6(
    conn: &Connection,
    repo: &gix::Repository,
    mesh_ranges: &[MeshRangeRow],
    whys: &WhyMap,
) -> Result<Vec<Candidate>> {
    let mut out = Vec::new();

    let mut stmt = conn.prepare(
        "SELECT DISTINCT path, pre_blob, post_blob FROM write_events \
         WHERE path IS NOT NULL AND pre_blob IS NOT NULL AND post_blob IS NOT NULL",
    )?;
    struct WriteBlobs {
        path: String,
        pre: String,
        post: String,
    }
    let writes: Vec<WriteBlobs> = stmt
        .query_map([], |r| {
            Ok(WriteBlobs {
                path: r.get(0)?,
                pre: r.get(1)?,
                post: r.get(2)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    drop(stmt);

    for w in &writes {
        let cands = detect_t6_inner(
            &w.path,
            &w.pre,
            &w.post,
            mesh_ranges,
            whys,
            &|partner| read_partner_bytes(repo, partner),
        );
        out.extend(cands);
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// T7 — new-group candidate. Files the session has touched together ≥3
// times AND that co-change in ≥5 of the last 40 commits. No mesh currently
// covers them.
// ---------------------------------------------------------------------------

pub(crate) fn detect_t7(
    conn: &Connection,
    repo: &gix::Repository,
    mesh_ranges: &[MeshRangeRow],
    _whys: &WhyMap,
) -> Result<Vec<Candidate>> {
    let mut out = Vec::new();

    // Touched multiset.
    let mut stmt = conn.prepare(
        "SELECT path FROM (\
           SELECT path FROM read_events WHERE path IS NOT NULL \
           UNION ALL SELECT path FROM write_events WHERE path IS NOT NULL) \
           GROUP BY path HAVING COUNT(*) >= 3",
    )?;
    let mut popular: Vec<String> = stmt
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    drop(stmt);
    popular.sort();
    popular.dedup();
    if popular.len() < 2 {
        return Ok(out);
    }

    // Per-session co-touch counts: number of distinct events (read+write)
    // touching each path. We compose a pair count as
    //   min(touches(a), touches(b))
    // which lower-bounds how many times the two appeared in the same
    // session window. (See §12.10 — "Touched together N times this
    // session".) Distinct events are the unit because each `add --read`
    // / `add --write` invocation is the natural per-touch granularity.
    let mut touch_stmt = conn.prepare(
        "SELECT path, COUNT(*) FROM (\
           SELECT path FROM read_events WHERE path IS NOT NULL \
           UNION ALL SELECT path FROM write_events WHERE path IS NOT NULL) \
           GROUP BY path",
    )?;
    let touch_counts: std::collections::HashMap<String, u32> = touch_stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? as u32)))?
        .collect::<Result<std::collections::HashMap<_, _>, _>>()?;
    drop(touch_stmt);

    // Paths already in a mesh — any pair including one of them is filtered.
    let meshed: std::collections::HashSet<&str> =
        mesh_ranges.iter().map(|r| r.path.as_str()).collect();

    // Historical co-change counts over last 40 commits.
    let co = historical_co_change(repo, &popular, 40).unwrap_or_default();

    // Enumerate pairs.
    for i in 0..popular.len() {
        for j in (i + 1)..popular.len() {
            let a = &popular[i];
            let b = &popular[j];
            if meshed.contains(a.as_str()) || meshed.contains(b.as_str()) {
                continue;
            }
            let n_co = co.get(&pair_key(a, b)).copied().unwrap_or(0);
            if n_co < 5 {
                continue;
            }
            let mut c = Candidate {
                mesh: String::new(), // placeholder — new group, no name yet
                mesh_why: String::new(),
                reason_kind: ReasonKind::NewGroup,
                partner_path: b.clone(),
                partner_start: None,
                partner_end: None,
                trigger_path: a.clone(),
                trigger_start: None,
                trigger_end: None,
                partner_marker: String::new(),
                partner_clause: {
                    let n_session = touch_counts
                        .get(a)
                        .copied()
                        .unwrap_or(0)
                        .min(touch_counts.get(b).copied().unwrap_or(0));
                    format!(
                        "Touched together {n_session} times this session; also co-changed in {n_co} of the last 40 commits"
                    )
                },
                density: Density::L2,
                command: format!("git mesh add <group-name> {a} {b}"),
                excerpt_of_path: String::new(),
                excerpt_start: None,
                excerpt_end: None,
            };
            // Put the two paths into the trigger/partner slots; give them
            // deterministic ordering for dedup.
            if a > b {
                std::mem::swap(&mut c.trigger_path, &mut c.partner_path);
            }
            out.push(c);
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// T8 — staging cross-cut. Stubbed minimal: walks `.git/mesh/staging/*` via
// `read_staging` and surfaces a candidate when a staged add's (path,
// extent) overlaps a mesh range on a *different* mesh.
// ---------------------------------------------------------------------------

/// T8 — staging cross-cut. A staged add row in `mesh_ranges` (source `S`,
/// `staged_op = 'add'`) that overlaps a *committed* row in a different
/// mesh on the same path. Surfaced as a cross-cutting block per
/// §12.9 / §12.10. Two clause variants ride on the same reason-kind:
///   - "overlap": staged range overlaps a committed range in another mesh.
///   - "content-differs": same `(path, extent)` recorded in another mesh
///     whose anchored bytes differ from the staged sidecar bytes.
///     §11 #9 third sub-case; chosen as a clause variant rather than a
///     sibling reason kind to keep dedup keyed on the same `t8` slot
///     per (mesh, partner range, trigger range).
pub(crate) fn detect_t8(
    _conn: &Connection,
    repo: &gix::Repository,
    mesh_ranges: &[MeshRangeRow],
    whys: &WhyMap,
) -> Result<Vec<Candidate>> {
    let mut out = Vec::new();
    let staged_rows: Vec<&MeshRangeRow> = mesh_ranges
        .iter()
        .filter(|r| r.staged_op == "add")
        .collect();
    if staged_rows.is_empty() {
        return Ok(out);
    }
    for staged in &staged_rows {
        let (Some(s_start), Some(s_end)) = (staged.start_line, staged.end_line) else {
            continue;
        };
        for row in mesh_ranges {
            if row.mesh == staged.mesh || row.path != staged.path {
                continue;
            }
            if row.staged_op == "add" {
                continue; // committed rows only as the partner
            }
            let (Some(ms), Some(me)) = (row.start_line, row.end_line) else {
                continue;
            };
            if s_end < ms || s_start > me {
                continue;
            }
            let mut c = Candidate::bare(
                &staged.mesh,
                &why_for(whys, &staged.mesh),
                ReasonKind::StagingCrossCut,
                &row.path,
                row.start_line,
                row.end_line,
                &staged.path,
            );
            c.trigger_start = staged.start_line;
            c.trigger_end = staged.end_line;
            // Carry the structural details on the candidate; the renderer
            // composes the §12.10 block from these fields.
            c.partner_clause = format!(
                "overlap|{}|{}|{}|{}|{}|{}|{}|{}",
                staged.mesh,
                row.mesh,
                row.path,
                ms.max(s_start),
                me.min(s_end),
                row.start_line.unwrap_or(0),
                row.end_line.unwrap_or(0),
                // staged extent appears via trigger_start/end; we pack the
                // committed extent in the clause for the renderer.
                "",
            );
            c.density = Density::L2;
            // No L2 command in the §12.10 example body; keep empty so the
            // render stays faithful.
            c.command = String::new();
            out.push(c);
        }

        // Content-differs variant: same `(path, extent)` recorded in a
        // *different* mesh with different anchored bytes than the staged
        // sidecar. §11 #9 third sub-case.
        for row in mesh_ranges {
            if row.mesh == staged.mesh
                || row.path != staged.path
                || row.staged_op == "add"
                || row.start_line != staged.start_line
                || row.end_line != staged.end_line
            {
                continue;
            }
            let staged_bytes = read_staged_sidecar_bytes(repo, &staged.mesh, staged);
            let other_bytes = read_committed_range_bytes(repo, row);
            let (Some(a), Some(b)) = (staged_bytes, other_bytes) else {
                continue;
            };
            if a == b {
                continue;
            }
            let mut c = Candidate::bare(
                &staged.mesh,
                &why_for(whys, &staged.mesh),
                ReasonKind::StagingCrossCut,
                &row.path,
                row.start_line,
                row.end_line,
                &staged.path,
            );
            c.trigger_start = staged.start_line;
            c.trigger_end = staged.end_line;
            c.partner_clause = format!(
                "content_differs|{}|{}|{}|{}|{}",
                staged.mesh,
                row.mesh,
                row.path,
                row.start_line.unwrap_or(0),
                row.end_line.unwrap_or(0),
            );
            c.density = Density::L2;
            out.push(c);
        }
    }
    Ok(out)
}

fn read_staged_sidecar_bytes(
    repo: &gix::Repository,
    mesh: &str,
    row: &MeshRangeRow,
) -> Option<Vec<u8>> {
    // Find the staged add by matching path+extent and read its sidecar.
    let staging = crate::staging::read_staging(repo, mesh).ok()?;
    let extent_match = staging.adds.iter().find(|a| {
        a.path == row.path
            && match (a.extent, row.start_line, row.end_line) {
                (
                    crate::types::RangeExtent::Lines { start, end },
                    Some(rs),
                    Some(re),
                ) => start as i64 == rs && end as i64 == re,
                (crate::types::RangeExtent::Whole, None, None) => true,
                _ => false,
            }
    })?;
    let path = crate::staging::sidecar_path_pub(repo, mesh, extent_match.line_number).ok()?;
    std::fs::read(path).ok()
}

fn read_committed_range_bytes(repo: &gix::Repository, row: &MeshRangeRow) -> Option<Vec<u8>> {
    let body = read_partner_bytes(repo, row)?;
    Some(body.into_bytes())
}

// ---------------------------------------------------------------------------
// T9 — empty-mesh risk. A staged remove that, if committed, would leave
// a mesh with no ranges.
// ---------------------------------------------------------------------------

/// T9 — empty-mesh risk. Computes from the in-flush mesh_ranges snapshot
/// directly: a mesh whose committed rows are all marked `staged_op='remove'`
/// AND has no `staged_op='add'` rows would end up with zero ranges if the
/// staged ops were committed. Renders one block per affected mesh listing
/// the removed ranges, per §12.8 T9 / §12.10.
pub(crate) fn detect_t9(
    _conn: &Connection,
    _repo: &gix::Repository,
    mesh_ranges: &[MeshRangeRow],
    whys: &WhyMap,
) -> Result<Vec<Candidate>> {
    let mut out = Vec::new();
    let mut by_mesh: std::collections::BTreeMap<String, Vec<&MeshRangeRow>> =
        std::collections::BTreeMap::new();
    for row in mesh_ranges {
        by_mesh.entry(row.mesh.clone()).or_default().push(row);
    }
    for (mesh, rows) in &by_mesh {
        let any_remove = rows.iter().any(|r| r.staged_op == "remove");
        if !any_remove {
            continue;
        }
        let any_committed_keep = rows
            .iter()
            .any(|r| r.staged_op != "remove" && r.staged_op != "add");
        if any_committed_keep {
            continue;
        }
        let any_staged_add = rows.iter().any(|r| r.staged_op == "add");
        if any_staged_add {
            continue;
        }
        // Emit a single T9 candidate carrying every removed range in the
        // partner_clause. The renderer parses this to produce the block
        // shape from §12.10.
        let mut removed_addrs: Vec<String> = rows
            .iter()
            .filter(|r| r.staged_op == "remove")
            .map(|r| {
                let suffix = match (r.start_line, r.end_line) {
                    (Some(s), Some(e)) => format!("#L{s}-L{e}"),
                    _ => String::new(),
                };
                format!("{}{}", r.path, suffix)
            })
            .collect();
        removed_addrs.sort();
        removed_addrs.dedup();
        let mut c = Candidate::bare(
            mesh,
            &why_for(whys, mesh),
            ReasonKind::EmptyMesh,
            "",
            None,
            None,
            "",
        );
        c.partner_clause = format!("removed:{}", removed_addrs.join(","));
        c.density = Density::L2;
        // Renderer handles the multi-line command snippet.
        c.command = format!("git mesh add    {mesh} <path>[#L<s>-L<e>]\ngit mesh delete {mesh}");
        out.push(c);
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// T10 — pending-commit re-anchor. A commit_event whose tree touched a meshed
// path: the mesh will re-anchor at post-commit time.
// ---------------------------------------------------------------------------

pub(crate) fn detect_t10(
    conn: &Connection,
    repo: &gix::Repository,
    mesh_ranges: &[MeshRangeRow],
    whys: &WhyMap,
) -> Result<Vec<Candidate>> {
    let mut out = Vec::new();
    let mut stmt = conn.prepare("SELECT sha FROM commit_events WHERE sha IS NOT NULL")?;
    let shas: Vec<String> = stmt
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    drop(stmt);
    if shas.is_empty() {
        return Ok(out);
    }

    let mut touched_paths: std::collections::BTreeSet<String> =
        std::collections::BTreeSet::new();
    for sha in &shas {
        if let Ok(paths) = commit_touched_paths(repo, sha) {
            touched_paths.extend(paths);
        }
    }

    for row in mesh_ranges {
        if !touched_paths.contains(&row.path) {
            continue;
        }
        let mut c = Candidate::bare(
            &row.mesh,
            &why_for(whys, &row.mesh),
            ReasonKind::PendingCommit,
            &row.path,
            row.start_line,
            row.end_line,
            &row.path,
        );
        c.partner_marker = "[WILL RE-ANCHOR]".into();
        out.push(c);
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// T11 — terminal status. Already folded into T1 (promoted reason-kind) and
// we add a standalone detector that surfaces terminal ranges even without
// a touch. Runs at L0 with the terminal marker.
// ---------------------------------------------------------------------------

pub(crate) fn detect_t11(
    _conn: &Connection,
    mesh_ranges: &[MeshRangeRow],
    whys: &WhyMap,
) -> Result<Vec<Candidate>> {
    let mut out = Vec::new();
    for row in mesh_ranges {
        let marker = marker_for_status(&row.status);
        if !matches!(
            row.status.as_str(),
            "ORPHANED" | "MERGE_CONFLICT" | "SUBMODULE"
        ) {
            continue;
        }
        let mut c = Candidate::bare(
            &row.mesh,
            &why_for(whys, &row.mesh),
            ReasonKind::Terminal,
            &row.path,
            row.start_line,
            row.end_line,
            "",
        );
        c.partner_marker = marker.to_string();
        out.push(c);
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Helpers.
// ---------------------------------------------------------------------------

fn pair_key(a: &str, b: &str) -> String {
    if a < b {
        format!("{a}\0{b}")
    } else {
        format!("{b}\0{a}")
    }
}

/// Walk the last `limit` commits on HEAD and count, for each pair in
/// `paths`, the number of commits that modified both.
fn historical_co_change(
    repo: &gix::Repository,
    paths: &[String],
    limit: usize,
) -> Result<std::collections::HashMap<String, u32>> {
    let mut out: std::collections::HashMap<String, u32> = std::collections::HashMap::new();
    let Ok(head) = repo.head_commit() else {
        return Ok(out);
    };
    let mut cur = Some(head);
    let mut n = 0usize;
    while let Some(commit) = cur {
        if n >= limit {
            break;
        }
        n += 1;
        let paths_in_commit = commit_touched_paths_commit(repo, &commit).unwrap_or_default();
        let hit: Vec<&String> = paths
            .iter()
            .filter(|p| paths_in_commit.iter().any(|q| q == p.as_str()))
            .collect();
        for i in 0..hit.len() {
            for j in (i + 1)..hit.len() {
                *out.entry(pair_key(hit[i], hit[j])).or_insert(0) += 1;
            }
        }
        // advance to first parent
        let parents: Vec<_> = commit.parent_ids().collect();
        cur = parents
            .first()
            .and_then(|id| repo.find_object(*id).ok())
            .and_then(|o| o.try_into_commit().ok());
    }
    Ok(out)
}

/// Paths modified by a commit relative to its first parent. Returns empty
/// vec for the initial commit (no parent) for simplicity.
fn commit_touched_paths(repo: &gix::Repository, sha: &str) -> Result<Vec<String>> {
    let oid = repo
        .rev_parse_single(sha)
        .map_err(|e| anyhow::anyhow!("resolve {sha}: {e}"))?
        .detach();
    let commit = repo.find_object(oid)?.try_into_commit()?;
    commit_touched_paths_commit(repo, &commit)
}

fn commit_touched_paths_commit(
    repo: &gix::Repository,
    commit: &gix::Commit<'_>,
) -> Result<Vec<String>> {
    let tree = commit.tree()?;
    let Some(parent_id) = commit.parent_ids().next() else {
        let mut out = Vec::new();
        collect_tree_paths(repo, &tree, "", &mut out)?;
        return Ok(out);
    };
    let parent_commit = repo.find_object(parent_id)?.try_into_commit()?;
    let parent_tree = parent_commit.tree()?;
    let mut out: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let mut changes = parent_tree.changes()?;
    changes
        .for_each_to_obtain_tree(&tree, |c| -> Result<std::ops::ControlFlow<()>> {
            out.insert(c.location().to_string());
            Ok(std::ops::ControlFlow::Continue(()))
        })
        .ok();
    Ok(out.into_iter().collect())
}

fn collect_tree_paths(
    repo: &gix::Repository,
    tree: &gix::Tree<'_>,
    prefix: &str,
    out: &mut Vec<String>,
) -> Result<()> {
    for entry in tree.iter() {
        let entry = entry?;
        let name = entry.filename().to_string();
        let path = if prefix.is_empty() {
            name
        } else {
            format!("{prefix}/{name}")
        };
        if entry.mode().is_tree() {
            let sub = repo.find_object(entry.object_id())?.try_into_tree()?;
            collect_tree_paths(repo, &sub, &path, out)?;
        } else {
            out.push(path);
        }
    }
    Ok(())
}

fn read_partner_bytes(repo: &gix::Repository, row: &MeshRangeRow) -> Option<String> {
    let bytes = crate::git::read_worktree_bytes(repo, &row.path).ok()?;
    let s = std::str::from_utf8(&bytes).ok()?.to_string();
    match (row.start_line, row.end_line) {
        (Some(start), Some(end)) => {
            let lines: Vec<&str> = s.lines().collect();
            let lo = (start.max(1) as usize).saturating_sub(1);
            let hi = (end as usize).min(lines.len());
            if lo > hi {
                return None;
            }
            Some(lines[lo..hi].join("\n"))
        }
        _ => Some(s),
    }
}

fn addr_suffix(start: Option<i64>, end: Option<i64>) -> String {
    match (start, end) {
        (Some(s), Some(e)) => format!("#L{s}-L{e}"),
        _ => String::new(),
    }
}

// ---------------------------------------------------------------------------
// Orchestrator: run all detectors.
// ---------------------------------------------------------------------------

pub fn run_all(
    conn: &Connection,
    repo: &gix::Repository,
    mesh_ranges: &[MeshRangeRow],
    whys: &WhyMap,
) -> Result<Vec<Candidate>> {
    let mut out = Vec::new();
    out.extend(detect_t1(conn, repo, mesh_ranges, whys)?);
    out.extend(detect_t2(conn, repo, mesh_ranges, whys)?);
    out.extend(detect_t3(conn, repo, mesh_ranges, whys)?);
    out.extend(detect_t3_renames(conn, repo, mesh_ranges, whys)?);
    out.extend(detect_t4(conn, mesh_ranges, whys)?);
    out.extend(detect_t5(conn, mesh_ranges, whys)?);
    out.extend(detect_t6(conn, repo, mesh_ranges, whys)?);
    out.extend(detect_t7(conn, repo, mesh_ranges, whys)?);
    out.extend(detect_t8(conn, repo, mesh_ranges, whys)?);
    out.extend(detect_t9(conn, repo, mesh_ranges, whys)?);
    out.extend(detect_t10(conn, repo, mesh_ranges, whys)?);
    out.extend(detect_t11(conn, mesh_ranges, whys)?);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::advice::db::init_or_verify_schema_pub;

    fn conn() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        init_or_verify_schema_pub(&c).unwrap();
        c
    }

    /// Create an empty temp repo (returned tuple keeps tempdir alive for
    /// the repo's lifetime). Tests that don't need worktree files use this
    /// purely to satisfy detectors that take a `&gix::Repository`.
    fn test_repo() -> (tempfile::TempDir, gix::Repository) {
        let td = tempfile::tempdir().unwrap();
        let dir = td.path();
        let run = |args: &[&str]| {
            std::process::Command::new("git")
                .current_dir(dir)
                .args(args)
                .output()
                .unwrap();
        };
        run(&["init", "--initial-branch=main"]);
        run(&["config", "user.email", "t@t"]);
        run(&["config", "user.name", "t"]);
        run(&["config", "commit.gpgsign", "false"]);
        // Seed scratch files used by T1/T11 unit tests so the
        // `[DELETED]` override (added in slice 3) doesn't fire.
        for f in &["a.rs", "b.rs", "c.rs", "z.rs"] {
            std::fs::write(dir.join(f), "// scratch\n").unwrap();
        }
        run(&["add", "."]);
        run(&["commit", "-m", "init"]);
        let repo = gix::open(dir).unwrap();
        (td, repo)
    }

    fn insert_read(conn: &Connection, path: &str) {
        conn.execute(
            "INSERT INTO events(kind,ts,payload) VALUES('read','t','{}')",
            [],
        )
        .unwrap();
        let id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO read_events(event_id,path,start_line,end_line) VALUES(?,?,NULL,NULL)",
            rusqlite::params![id, path],
        )
        .unwrap();
    }

    fn insert_mesh_range(
        conn: &Connection,
        mesh: &str,
        path: &str,
        s: Option<i64>,
        e: Option<i64>,
        status: &str,
    ) {
        conn.execute(
            "INSERT INTO mesh_ranges(mesh,path,start_line,end_line,status,source,ack) \
             VALUES(?,?,?,?,?,'W',0)",
            rusqlite::params![mesh, path, s, e, status],
        )
        .unwrap();
    }

    #[test]
    fn t1_produces_partner_rows() {
        let c = conn();
        insert_read(&c, "a.rs");
        insert_mesh_range(&c, "m1", "a.rs", Some(1), Some(10), "FRESH");
        insert_mesh_range(&c, "m1", "b.rs", Some(5), Some(20), "CHANGED");

        let rows = load_mesh_ranges(&c).unwrap();
        let whys = WhyMap::new();
        let (_td, repo) = test_repo();
        let cands = detect_t1(&c, &repo, &rows, &whys).unwrap();
        assert_eq!(cands.len(), 1);
        let cand = &cands[0];
        assert_eq!(cand.reason_kind, ReasonKind::Partner);
        assert_eq!(cand.partner_path, "b.rs");
        assert_eq!(cand.partner_marker, "[CHANGED]");
        assert_eq!(cand.trigger_path, "a.rs");
    }

    #[test]
    fn t1_promotes_to_terminal_on_orphaned() {
        let c = conn();
        insert_read(&c, "a.rs");
        insert_mesh_range(&c, "m1", "a.rs", Some(1), Some(10), "FRESH");
        insert_mesh_range(&c, "m1", "b.rs", Some(5), Some(20), "ORPHANED");

        let rows = load_mesh_ranges(&c).unwrap();
        let whys = WhyMap::new();
        let (_td, repo) = test_repo();
        let cands = detect_t1(&c, &repo, &rows, &whys).unwrap();
        assert_eq!(cands.len(), 1);
        assert_eq!(cands[0].reason_kind, ReasonKind::Terminal);
        assert_eq!(cands[0].partner_marker, "[ORPHANED]");
    }

    #[test]
    fn t5_losing_coherence_fires_only_when_mostly_drift() {
        let c = conn();
        insert_read(&c, "a.rs");
        insert_mesh_range(&c, "m", "a.rs", Some(1), Some(10), "FRESH");
        insert_mesh_range(&c, "m", "b.rs", Some(1), Some(10), "CHANGED");
        insert_mesh_range(&c, "m", "c.rs", Some(1), Some(10), "CHANGED");

        let rows = load_mesh_ranges(&c).unwrap();
        let whys = WhyMap::new();
        let cands = detect_t5(&c, &rows, &whys).unwrap();
        assert!(!cands.is_empty(), "should fire");
        assert!(cands.iter().all(|c| c.reason_kind == ReasonKind::LosingCoherence));
    }

    #[test]
    fn t4_collapse_detects() {
        let c = conn();
        // Write on a.rs#L1-L2 (extent 2); mesh range a.rs#L1-L20 (extent 20).
        // Post content is 2 lines; 2 ≤ 50% of 20 AND 20-2 ≥ 2, so T4 fires.
        c.execute(
            "INSERT INTO events(kind,ts,payload) VALUES('write','t','{}')",
            [],
        ).unwrap();
        let id = c.last_insert_rowid();
        c.execute(
            "INSERT INTO write_events(event_id,path,start_line,end_line,pre_blob,post_blob) \
             VALUES(?,'a.rs',1,2,NULL,'one\ntwo\n')",
            rusqlite::params![id],
        ).unwrap();

        insert_mesh_range(&c, "m", "a.rs", Some(1), Some(20), "CHANGED");
        insert_mesh_range(&c, "m", "b.rs", Some(1), Some(10), "FRESH");
        let rows = load_mesh_ranges(&c).unwrap();
        let whys = WhyMap::new();
        let cands = detect_t4(&c, &rows, &whys).unwrap();
        assert!(!cands.is_empty());
        assert_eq!(cands[0].reason_kind, ReasonKind::RangeCollapse);
        // Partner is the *other* range (b.rs), per §11.8.
        assert_eq!(cands[0].partner_path, "b.rs");
    }

    #[test]
    fn t11_surfaces_orphaned() {
        let c = conn();
        insert_mesh_range(&c, "m", "z.rs", Some(1), Some(5), "ORPHANED");
        let rows = load_mesh_ranges(&c).unwrap();
        let cands = detect_t11(&c, &rows, &WhyMap::new()).unwrap();
        assert_eq!(cands.len(), 1);
        assert_eq!(cands[0].partner_marker, "[ORPHANED]");
    }

    /// T6: pub fn old_name → pub fn new_name in pre/post, partner contains
    /// old_name → one Candidate with SymbolRename, L2, git mesh add command.
    #[test]
    fn t6_symbol_rename_detects_partner_hit() {
        let pre_blob = "pub fn old_name() -> u32 { 0 }";
        let post_blob = "pub fn new_name() -> u32 { 0 }";
        // Partner body that references the old symbol.
        let partner_body_text = "// calls old_name to compute value\nlet x = old_name();";

        let ranges = vec![
            // Trigger range: written file a.rs
            MeshRangeRow {
                mesh: "auth/flow".to_string(),
                path: "a.rs".to_string(),
                start_line: Some(1),
                end_line: Some(10),
                status: "FRESH".to_string(),
                source: "W".to_string(),
                ack: false, staged_op: String::new(),
            },
            // Partner range: b.rs (same mesh, different file)
            MeshRangeRow {
                mesh: "auth/flow".to_string(),
                path: "b.rs".to_string(),
                start_line: Some(1),
                end_line: Some(5),
                status: "FRESH".to_string(),
                source: "W".to_string(),
                ack: false, staged_op: String::new(),
            },
        ];

        let whys = WhyMap::new();
        let cands = detect_t6_inner(
            "a.rs",
            pre_blob,
            post_blob,
            &ranges,
            &whys,
            &|row| {
                if row.path == "b.rs" {
                    Some(partner_body_text.to_string())
                } else {
                    None
                }
            },
        );

        assert_eq!(cands.len(), 1, "expected exactly one candidate");
        let c = &cands[0];
        assert_eq!(c.reason_kind, ReasonKind::SymbolRename);
        assert_eq!(c.density, Density::L2);
        assert!(
            c.command.starts_with("git mesh add auth/flow b.rs"),
            "command was: {}",
            c.command
        );
    }

    /// T6: no hit when partner body does not contain old_name.
    #[test]
    fn t6_symbol_rename_no_hit_when_partner_clean() {
        let pre_blob = "export function oldFn() {}";
        let post_blob = "export function newFn() {}";
        let partner_body_text = "// only references newFn\nnewFn();";

        let ranges = vec![
            MeshRangeRow {
                mesh: "m".to_string(),
                path: "src.ts".to_string(),
                start_line: None,
                end_line: None,
                status: "FRESH".to_string(),
                source: "W".to_string(),
                ack: false, staged_op: String::new(),
            },
            MeshRangeRow {
                mesh: "m".to_string(),
                path: "consumer.ts".to_string(),
                start_line: None,
                end_line: None,
                status: "FRESH".to_string(),
                source: "W".to_string(),
                ack: false, staged_op: String::new(),
            },
        ];

        let whys = WhyMap::new();
        let cands = detect_t6_inner(
            "src.ts",
            pre_blob,
            post_blob,
            &ranges,
            &whys,
            &|_| Some(partner_body_text.to_string()),
        );
        assert!(cands.is_empty(), "no hit expected when partner is already updated");
    }
}

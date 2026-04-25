//! Flush pipeline — the `git mesh advice <sessionId>` render event.
//!
//! Wraps mesh_ranges rebuild, intersection detection, seen-set dedup,
//! render, and INSERTs in a single `BEGIN IMMEDIATE` transaction. Returns
//! the rendered markdown to the caller — printing happens *after* the
//! commit so a rollback (on error) leaves no trace on stdout.

use anyhow::{Context, Result};
use rusqlite::Connection;
use std::collections::HashMap;

use crate::advice::events::AuditRecord;
use crate::advice::intersections::{Candidate, WhyMap, run_all};
use crate::advice::render;
use crate::types::{EngineOptions, LayerSet, RangeExtent, RangeStatus};
use std::collections::BTreeSet;

/// Run a flush: rebuild snapshot, detect intersections, dedup, render,
/// record seen-set + doc topics. Returns the rendered markdown string.
///
/// `print-after-commit`: caller is responsible for printing the returned
/// string only when this function succeeds — a rolled-back transaction
/// must not surface to the user.
pub fn run_flush(
    conn: &mut Connection,
    repo: &gix::Repository,
    documentation: bool,
) -> Result<(String, AuditRecord)> {
    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .context("begin immediate")?;

    // 1. Rebuild mesh_ranges from library-level resolver. Errors are
    //    logged to stderr and the snapshot is left empty (fail-soft per
    //    plan: "no mesh case" must return an empty string, not an error).
    tx.execute("DELETE FROM mesh_ranges", [])
        .context("truncate mesh_ranges")?;
    let (whys, rebuild_err) = match rebuild_mesh_ranges(&tx, repo) {
        Ok(whys) => (whys, None),
        Err(e) => (WhyMap::new(), Some(e)),
    };
    if let Some(e) = &rebuild_err {
        eprintln!("git mesh advice: warning: failed to rebuild mesh snapshot: {e}");
    }

    // 2. Mesh ranges snapshot.
    let mesh_ranges = crate::advice::intersections::load_mesh_ranges(&tx)?;

    // 3. Detect candidates across all intersections.
    let mut candidates = run_all(&tx, repo, &mesh_ranges, &whys)?;

    // 4. Dedup against flush_additions (trigger-aware PK).
    candidates = dedup_against_seen(&tx, candidates)?;

    // 5. Render. Doc topics filter down to first-use only, scoped to
    //    reason-kinds that fired this flush.
    let fired_topics: Vec<&'static str> = candidates
        .iter()
        .filter_map(|c| c.reason_kind.doc_topic())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    let unseen_topics = topics_not_yet_recorded(&tx, &fired_topics)?;
    let rendered = render::render(&candidates, &unseen_topics, documentation);

    // 6. Record flush event + additions + doc topics.
    let flush_record = record_flush_event(&tx, &rendered, documentation)?;
    record_additions(&tx, flush_record.id, &candidates)?;
    record_doc_topics(&tx, flush_record.id, &unseen_topics)?;

    // 7. Commit the transaction. Anything that mutated the DB becomes
    //    durable only on success.
    //
    // Retain why-map size as a lint hint (silences unused-mut if we did
    // not end up mutating it in this path).
    let _ = whys.len();
    let _ = whys.keys().next();
    drop(whys);
    tx.commit().context("commit flush tx")?;

    Ok((rendered, flush_record))
}

// ---------------------------------------------------------------------------
// Mesh snapshot rebuild.
// ---------------------------------------------------------------------------

/// Populate `mesh_ranges` from `stale_meshes`. Returns a per-mesh why map
/// harvested from the same resolver pass. Per plan: no shelling out.
fn rebuild_mesh_ranges(tx: &rusqlite::Transaction<'_>, repo: &gix::Repository) -> Result<WhyMap> {
    let mut whys: WhyMap = HashMap::new();
    let options = EngineOptions {
        layers: LayerSet {
            worktree: true,
            index: true,
            staged_mesh: true,
        },
        ignore_unavailable: false,
        since: None,
    };
    let meshes = crate::resolver::stale_meshes(repo, options)?;

    let mut insert = tx.prepare(
        "INSERT INTO mesh_ranges(mesh,path,start_line,end_line,status,source,ack,staged_op) \
         VALUES(?,?,?,?,?,?,?,?)",
    )?;

    let mut committed_names: BTreeSet<String> = BTreeSet::new();
    for mesh in &meshes {
        committed_names.insert(mesh.name.clone());
        let first_line = mesh.message.lines().next().unwrap_or("").to_string();
        whys.insert(mesh.name.clone(), first_line);
        // Read staging once per mesh so we can mark committed ranges as
        // staged-for-removal in the same pass.
        let staging = crate::staging::read_staging(repo, &mesh.name).ok();
        for r in &mesh.ranges {
            let (s, e) = match r.anchored.extent {
                RangeExtent::Lines { start, end } => (Some(start as i64), Some(end as i64)),
                RangeExtent::Whole => (None, None),
            };
            let status = status_str(&r.status);
            let src = match r.source {
                Some(crate::types::DriftSource::Head) => "H",
                Some(crate::types::DriftSource::Index) => "I",
                Some(crate::types::DriftSource::Worktree) => "W",
                None => "",
            };
            let ack = if r.acknowledged_by.is_some() { 1 } else { 0 };
            let path_str = r.anchored.path.to_string_lossy().into_owned();
            let staged_op = staging
                .as_ref()
                .map(|st| {
                    let matches_remove = st.removes.iter().any(|rm| {
                        rm.path == path_str
                            && match (rm.extent, r.anchored.extent) {
                                (
                                    RangeExtent::Lines { start: rs, end: re },
                                    RangeExtent::Lines { start: as_, end: ae },
                                ) => rs == as_ && re == ae,
                                (RangeExtent::Whole, RangeExtent::Whole) => true,
                                _ => false,
                            }
                    });
                    if matches_remove { "remove" } else { "" }
                })
                .unwrap_or("");
            insert.execute(rusqlite::params![
                mesh.name, path_str, s, e, status, src, ack, staged_op,
            ])?;
        }
    }

    // Slice 4: include staged adds (and staged-only meshes) so the
    // intersection layer can see ranges that haven't reached HEAD/Index
    // yet. Each staged add becomes a row with `source='S'` and
    // `staged_op='add'`; the why is taken from the staged `.why` if the
    // mesh has no committed message yet.
    let staged_names = crate::staging::list_staged_mesh_names(repo).unwrap_or_default();
    for name in &staged_names {
        let Ok(staging) = crate::staging::read_staging(repo, name) else {
            continue;
        };
        if !committed_names.contains(name)
            && let Some(w) = &staging.why
        {
            let first_line = w.lines().next().unwrap_or("").to_string();
            whys.entry(name.clone()).or_insert(first_line);
        }
        for add in &staging.adds {
            let (s, e) = match add.extent {
                RangeExtent::Lines { start, end } => (Some(start as i64), Some(end as i64)),
                RangeExtent::Whole => (None, None),
            };
            insert.execute(rusqlite::params![
                name, add.path, s, e, "FRESH", "S", 0, "add",
            ])?;
        }
    }
    Ok(whys)
}

fn status_str(s: &RangeStatus) -> &'static str {
    match s {
        RangeStatus::Fresh => "FRESH",
        RangeStatus::Moved => "MOVED",
        RangeStatus::Changed => "CHANGED",
        RangeStatus::Orphaned => "ORPHANED",
        RangeStatus::MergeConflict => "MERGE_CONFLICT",
        RangeStatus::Submodule => "SUBMODULE",
        RangeStatus::ContentUnavailable(_) => "CONTENT_UNAVAILABLE",
    }
}

// ---------------------------------------------------------------------------
// Seen-set dedup.
// ---------------------------------------------------------------------------

fn dedup_against_seen(
    tx: &rusqlite::Transaction<'_>,
    candidates: Vec<Candidate>,
) -> Result<Vec<Candidate>> {
    // Use a sentinel for NULL start/end to satisfy the non-NULL PK.
    // Match events.rs convention: sentinel 0 never collides with real
    // 1-based line numbers, and empty-string path for a missing partner.
    let mut out = Vec::with_capacity(candidates.len());
    let mut stmt = tx.prepare(
        "SELECT 1 FROM flush_additions \
         WHERE mesh=? AND reason_kind=? AND range_path=? \
           AND start_line=? AND end_line=? AND trigger_path=?",
    )?;
    for c in candidates {
        let s = c.partner_start.unwrap_or(0);
        let e = c.partner_end.unwrap_or(0);
        let seen: bool = stmt
            .exists(rusqlite::params![
                c.mesh,
                c.reason_kind.as_str(),
                c.partner_path,
                s,
                e,
                c.trigger_path,
            ])?;
        if !seen {
            out.push(c);
        }
    }
    Ok(out)
}

fn record_flush_event(
    tx: &rusqlite::Transaction<'_>,
    rendered: &str,
    documentation: bool,
) -> Result<AuditRecord> {
    let ts = chrono::Utc::now().to_rfc3339();
    let output_sha = blake_short(rendered);
    let output_len = rendered.len() as i64;
    let payload = serde_json::json!({
        "documentation": documentation,
        "output_len": output_len,
        "output_sha": output_sha,
    });
    let payload_str = payload.to_string();
    tx.execute(
        "INSERT INTO events(kind,ts,payload) VALUES('flush',?,?)",
        rusqlite::params![ts, payload_str],
    )?;
    let id = tx.last_insert_rowid();
    tx.execute(
        "INSERT INTO flush_events(event_id,output_sha) VALUES(?,?)",
        rusqlite::params![id, output_sha],
    )?;
    Ok(AuditRecord {
        id,
        kind: "flush",
        ts,
        payload,
    })
}

fn blake_short(s: &str) -> String {
    // Cheap content id (session-local, not cryptographic). Truncated FNV
    // over bytes — avoids pulling in a hash crate.
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("{h:016x}")
}

fn record_additions(
    tx: &rusqlite::Transaction<'_>,
    flush_event_id: i64,
    candidates: &[Candidate],
) -> Result<()> {
    let mut stmt = tx.prepare(
        "INSERT OR IGNORE INTO flush_additions\
         (flush_event_id,mesh,reason_kind,range_path,start_line,end_line,trigger_path) \
         VALUES(?,?,?,?,?,?,?)",
    )?;
    for c in candidates {
        let s = c.partner_start.unwrap_or(0);
        let e = c.partner_end.unwrap_or(0);
        stmt.execute(rusqlite::params![
            flush_event_id,
            c.mesh,
            c.reason_kind.as_str(),
            c.partner_path,
            s,
            e,
            c.trigger_path,
        ])?;
    }
    Ok(())
}

fn topics_not_yet_recorded(
    tx: &rusqlite::Transaction<'_>,
    topics: &[&str],
) -> Result<Vec<String>> {
    let mut out = Vec::new();
    let mut stmt = tx.prepare("SELECT 1 FROM flush_doc_topics WHERE doc_topic=?")?;
    for t in topics {
        let seen: bool = stmt.exists(rusqlite::params![t])?;
        if !seen {
            out.push((*t).to_string());
        }
    }
    Ok(out)
}

fn record_doc_topics(
    tx: &rusqlite::Transaction<'_>,
    flush_event_id: i64,
    topics: &[String],
) -> Result<()> {
    let mut stmt = tx.prepare(
        "INSERT OR IGNORE INTO flush_doc_topics(flush_event_id,doc_topic) VALUES(?,?)",
    )?;
    for t in topics {
        stmt.execute(rusqlite::params![flush_event_id, t])?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::advice::db::init_or_verify_schema_pub;
    use crate::advice::intersections::ReasonKind;
    use std::process::Command;
    use tempfile::TempDir;

    fn seed_repo(td: &TempDir) -> gix::Repository {
        let dir = td.path();
        let run = |args: &[&str]| {
            Command::new("git")
                .current_dir(dir)
                .args(args)
                .output()
                .unwrap();
        };
        run(&["init", "--initial-branch=main"]);
        run(&["config", "user.email", "t@t"]);
        run(&["config", "user.name", "t"]);
        run(&["config", "commit.gpgsign", "false"]);
        std::fs::write(dir.join("foo.txt"), "hi\n").unwrap();
        run(&["add", "."]);
        run(&["commit", "-m", "init"]);
        gix::open(dir).unwrap()
    }

    fn open_conn() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        init_or_verify_schema_pub(&c).unwrap();
        c
    }

    #[test]
    fn no_mesh_returns_empty() {
        let td = TempDir::new().unwrap();
        let repo = seed_repo(&td);
        let mut conn = open_conn();
        let (out, rec) = run_flush(&mut conn, &repo, false).unwrap();
        assert_eq!(out, "");
        assert_eq!(rec.kind, "flush");
        assert_eq!(rec.payload["documentation"], false);
    }

    #[test]
    fn dedup_suppresses_second_flush() {
        let td = TempDir::new().unwrap();
        let repo = seed_repo(&td);
        let mut conn = open_conn();

        // Manually seed a read event and a mesh_ranges row so we can drive
        // dedup without touching the resolver — we're exercising the dedup
        // layer, not the snapshot rebuild.
        conn.execute(
            "INSERT INTO events(kind,ts,payload) VALUES('read','t','{}')",
            [],
        )
        .unwrap();
        let id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO read_events(event_id,path,start_line,end_line) VALUES(?,'a.rs',NULL,NULL)",
            rusqlite::params![id],
        )
        .unwrap();

        // Manually write a candidate into flush_additions as if a prior
        // flush had recorded it. Subsequent call must not re-emit.
        conn.execute(
            "INSERT INTO flush_events(event_id,output_sha) VALUES(0,'x')",
            [],
        )
        .ok();
        conn.execute(
            "INSERT INTO flush_additions\
              (flush_event_id,mesh,reason_kind,range_path,start_line,end_line,trigger_path) \
              VALUES(0,'m','partner','b.rs',5,20,'a.rs')",
            [],
        )
        .unwrap();

        let cand = Candidate {
            mesh: "m".into(),
            mesh_why: "w".into(),
            reason_kind: ReasonKind::Partner,
            partner_path: "b.rs".into(),
            partner_start: Some(5),
            partner_end: Some(20),
            trigger_path: "a.rs".into(),
            trigger_start: None,
            trigger_end: None,
            partner_marker: String::new(),
            partner_clause: String::new(),
            density: crate::advice::intersections::Density::L0,
            command: String::new(),
            excerpt_of_path: String::new(),
            excerpt_start: None,
            excerpt_end: None,
        };
        let tx = conn.transaction().unwrap();
        let out = dedup_against_seen(&tx, vec![cand.clone()]).unwrap();
        assert!(out.is_empty(), "second emission must be dedup'd");
        tx.rollback().unwrap();

        // But a new trigger re-surfaces it.
        let mut cand2 = cand;
        cand2.trigger_path = "c.rs".into();
        let tx = conn.transaction().unwrap();
        let out = dedup_against_seen(&tx, vec![cand2]).unwrap();
        assert_eq!(out.len(), 1, "new trigger must re-surface the partner");
        tx.rollback().unwrap();

        // drop unused field warning avoidance
        let _ = repo.head_id();
    }
}

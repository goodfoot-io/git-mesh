//! Phase F.4 — End-to-end pipeline test for the card's flagship example.
//!
//! A session that
//!   - whole-file reads `compact.rs`
//!   - ranged-reads `stale_output.rs#L1-L80`
//!   - whole-file reads `mod.rs`
//!   - ranged-edits `compact.rs#L88-L120` inside `fn run_compact (L13-L156)`
//!
//! must, after running through `build_op_stream` →
//! `build_participants` → `resolve_extent_precedence_with` (with the real
//! tree-sitter resolver) → `merge_ranges_per_file` → `build_canonical_ranges`,
//! produce a `compact.rs` participant whose `(start, end)` is the hunk extent
//! `(88, 120)` AND whose `extent_source` is `Symbol` (so the symbol enclosure
//! is preserved as provenance, not as a rewritten range).

use std::fs;
use std::path::PathBuf;

use git_mesh::advice::session::state::{ReadRecord, TouchInterval, TouchKind};
use git_mesh::advice::suggest::op_stream::{SessionRecord, build_op_stream};
use git_mesh::advice::suggest::participants::resolve_extent_precedence_with;
use git_mesh::advice::suggest::symbol_extent::enclosing_symbol_range;
use git_mesh::advice::suggest::{
    ExtentSource, SuggestConfig, build_canonical_ranges, build_participants, merge_ranges_per_file,
};
use tempfile::tempdir;

/// Build `compact.rs` whose lines 13..=156 form the body of
/// `pub fn run_compact`. Lines 88..=120 sit well inside that body so the
/// real tree-sitter resolver returns `Some((13, 156, "run_compact"))`.
fn make_compact_rs() -> String {
    // Lines 1..=12: prelude.
    let mut s = String::new();
    s.push_str("//! compact.rs — fixture for Phase F.4 end-to-end test.\n"); // 1
    s.push_str("//\n"); // 2
    s.push_str("// The body of `run_compact` spans lines 13..=156. Lines 88..=120\n"); // 3
    s.push_str("// sit inside that body — the Edit hunk fixture targets that range.\n"); // 4
    s.push('\n'); // 5
    s.push_str("use std::io;\n"); // 6
    s.push_str("use std::path::Path;\n"); // 7
    s.push('\n'); // 8
    s.push_str("pub struct CompactOpts {\n"); // 9
    s.push_str("    pub dry_run: bool,\n"); // 10
    s.push_str("}\n"); // 11
    s.push('\n'); // 12

    // Line 13: `pub fn run_compact(...)` opens.
    s.push_str("pub fn run_compact(opts: &CompactOpts, root: &Path) -> io::Result<()> {\n"); // 13

    // Lines 14..=155: function body. Fill with plausible Rust statements so
    // tree-sitter parses without error and the function spans through line 156.
    let mut line = 14u32;
    while line <= 155 {
        // Vary the statement so the body isn't a wall of identical lines but
        // every line still parses as a statement inside the fn.
        let stmt = match line % 6 {
            0 => format!("    let _value_{line}: usize = {line};\n"),
            1 => format!("    let _name_{line} = String::from(\"line_{line}\");\n"),
            2 => "    if opts.dry_run { let _ = root; }\n".to_string(),
            3 => format!("    for _i_{line} in 0..{line} {{ let _ = _i_{line}; }}\n"),
            4 => format!("    let _path_{line} = root.join(\"file_{line}.txt\");\n"),
            _ => format!("    let _ = ({line}u32, opts.dry_run);\n"),
        };
        s.push_str(&stmt);
        line += 1;
    }

    // Line 156: closing brace of `run_compact`.
    s.push_str("}\n"); // 156

    s
}

fn make_mod_rs() -> String {
    let mut s = String::new();
    s.push_str("//! cli/mod.rs — fixture for Phase F.4 end-to-end test.\n");
    s.push('\n');
    s.push_str("pub mod compact;\n");
    s.push_str("pub mod stale_output;\n");
    s.push('\n');
    for i in 0..50 {
        s.push_str(&format!("pub const ITEM_{i}: u32 = {i};\n"));
    }
    s
}

fn make_stale_output_rs() -> String {
    let mut s = String::new();
    s.push_str("//! cli/stale_output.rs — fixture for Phase F.4 end-to-end test.\n");
    s.push('\n');
    s.push_str("pub fn render_stale_summary(count: usize) -> String {\n");
    s.push_str("    let mut out = String::new();\n");
    for i in 0..70 {
        s.push_str(&format!(
            "    out.push_str(&format!(\"line_{i}: {{}}\\n\", count + {i}));\n"
        ));
    }
    s.push_str("    out\n");
    s.push_str("}\n");
    s
}

fn write_file(root: &std::path::Path, rel: &str, contents: &str) -> PathBuf {
    let abs = root.join(rel);
    fs::create_dir_all(abs.parent().unwrap()).unwrap();
    fs::write(&abs, contents).unwrap();
    abs
}

#[test]
fn card_example_compact_hunk_extent_with_symbol_provenance() {
    // ── Arrange: lay down real source files so tree-sitter can parse them.
    let tmp = tempdir().unwrap();
    let root = tmp.path();

    let compact_rel = "packages/git-mesh/src/cli/compact.rs";
    let mod_rel = "packages/git-mesh/src/cli/mod.rs";
    let stale_rel = "packages/git-mesh/src/cli/stale_output.rs";

    let compact_abs = write_file(root, compact_rel, &make_compact_rs());
    write_file(root, mod_rel, &make_mod_rs());
    write_file(root, stale_rel, &make_stale_output_rs());

    // Sanity-check the fixture: the resolver should find run_compact at L13-L156
    // when asked for the hunk's enclosing range. If this assert fails the test
    // body is invalid (the fixture isn't shaped how we claim).
    let enclosed =
        enclosing_symbol_range(&compact_abs, (88, 120)).expect("fixture: hunk inside run_compact");
    assert_eq!(enclosed.0, 13, "fixture: run_compact starts at line 13");
    assert_eq!(enclosed.1, 156, "fixture: run_compact ends at line 156");
    assert_eq!(enclosed.2, "run_compact");

    // ── Build the session: 3 reads + 1 ranged Modified touch.
    let session = SessionRecord {
        sid: "s1".to_string(),
        reads: vec![
            ReadRecord {
                path: compact_rel.to_string(),
                ts: "2024-01-01T00:00:01Z".to_string(),
                start_line: None,
                end_line: None,
                id: None,
            },
            ReadRecord {
                path: stale_rel.to_string(),
                ts: "2024-01-01T00:00:02Z".to_string(),
                start_line: Some(1),
                end_line: Some(80),
                id: None,
            },
            ReadRecord {
                path: mod_rel.to_string(),
                ts: "2024-01-01T00:00:03Z".to_string(),
                start_line: None,
                end_line: None,
                id: None,
            },
        ],
        touches: vec![TouchInterval {
            path: compact_rel.to_string(),
            kind: TouchKind::Modified,
            id: "tuid-1".to_string(),
            ts: "2024-01-01T00:00:04Z".to_string(),
            start: Some(88),
            end: Some(120),
        }],
    };

    // ── Act: run through the pipeline using the real symbol resolver.
    let cfg = SuggestConfig::default();
    let ops = build_op_stream(&session, &cfg);
    let raw = build_participants(&ops);
    let workdir = root.to_path_buf();
    let resolved = resolve_extent_precedence_with(raw, |rel, range| {
        enclosing_symbol_range(&workdir.join(rel), range)
    });

    // ── Assert: the participant layer.
    let by_path = |path: &str| -> Vec<_> {
        resolved
            .iter()
            .filter(|p| p.path == path)
            .cloned()
            .collect::<Vec<_>>()
    };

    let compact_parts = by_path(compact_rel);
    assert_eq!(
        compact_parts.len(),
        1,
        "compact.rs whole-file dropped by the ranged Edit"
    );
    let cp = &compact_parts[0];
    // Phase F.1 contract: the symbol provenance is recorded …
    assert_eq!(
        cp.extent_source,
        ExtentSource::Symbol,
        "symbol enclosure exists, so source promotes to Symbol for provenance"
    );
    // … but the printed range stays at the hunk extent the user actually edited.
    assert_eq!(cp.start, 88, "hunk start preserved (not rewritten to 13)");
    assert_eq!(cp.end, 120, "hunk end preserved (not rewritten to 156)");
    assert_eq!(cp.m_start, 88);
    assert_eq!(cp.m_end, 120);

    let stale_parts = by_path(stale_rel);
    assert_eq!(stale_parts.len(), 1);
    assert_eq!(stale_parts[0].extent_source, ExtentSource::Read);
    assert_eq!(stale_parts[0].start, 1);
    assert_eq!(stale_parts[0].end, 80);

    let mod_parts = by_path(mod_rel);
    assert_eq!(mod_parts.len(), 1);
    assert_eq!(mod_parts[0].extent_source, ExtentSource::Whole);
    assert_eq!(
        mod_parts[0].m_start,
        git_mesh::advice::suggest::participants::WHOLE_FILE_START
    );
    assert_eq!(
        mod_parts[0].m_end,
        git_mesh::advice::suggest::participants::WHOLE_FILE_END
    );

    // ── Assert: the canonical-range layer (what the renderer reads).
    let merged = merge_ranges_per_file(&resolved, &cfg);
    let canonical = build_canonical_ranges(&merged, &cfg);

    let canon_for = |path: &str| -> Vec<_> {
        canonical
            .ranges
            .iter()
            .filter(|r| r.path == path)
            .cloned()
            .collect::<Vec<_>>()
    };

    let cc = canon_for(compact_rel);
    assert_eq!(cc.len(), 1, "compact.rs canonical not collapsed to whole");
    assert_eq!(cc[0].start, 88, "canonical preserves hunk start");
    assert_eq!(cc[0].end, 120, "canonical preserves hunk end");
    assert_eq!(
        cc[0].source,
        ExtentSource::Symbol,
        "canonical carries Symbol provenance"
    );

    let sc = canon_for(stale_rel);
    assert_eq!(sc.len(), 1);
    assert_eq!(sc[0].start, 1);
    assert_eq!(sc[0].end, 80);
    assert_eq!(sc[0].source, ExtentSource::Read);

    let mc = canon_for(mod_rel);
    assert_eq!(mc.len(), 1);
    assert_eq!(
        mc[0].start,
        git_mesh::advice::suggest::participants::WHOLE_FILE_START
    );
    assert_eq!(
        mc[0].end,
        git_mesh::advice::suggest::participants::WHOLE_FILE_END
    );
    assert_eq!(mc[0].source, ExtentSource::Whole);
}

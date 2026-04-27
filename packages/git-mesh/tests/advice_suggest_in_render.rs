//! Slice 2 contract tests for card main-13.
//!
//! Pin the contract that `git mesh advice <SESSION_ID>` (the per-session
//! render) now folds in the n-ary, line-bounded recommendations produced
//! by `advice::suggest::run_suggest_pipeline`, supplanting the old pairwise
//! `detect_session_co_touch` channel.
//!
//! Both tests are `#[ignore]`d on landing — the surrounding integration
//! harness for invoking `run_advice_render` end-to-end with a fixture
//! corpus is wired in slice 3. They compile, are discovered by `cargo test`,
//! and document the expected behaviour so a future flip-from-ignored is
//! a one-line change once the harness exists.

#![allow(unused_imports)]

use git_mesh::advice::suggest::{SessionRecord, SuggestConfig, run_suggest_pipeline};

/// When the session corpus contains a co-touched triad (A, B, C),
/// `run_advice_render` must emit a single n-ary recommendation covering
/// all three files, not three pairwise A↔B / A↔C / B↔C suggestions.
#[test]
#[ignore = "slice-2: contract pinned; render-harness wiring in slice 3"]
fn render_includes_suggest_recommendations() {
    // TODO(slice-3): wire fixture (e.g. tests/fixtures/advice/triad_strong)
    // into a `run_advice_render` invocation and assert that the rendered
    // output contains exactly one suggestion-block whose participants set
    // is {A, B, C} — never three pairwise blocks.
    //
    // Until the harness lands, exercise the suggester directly to keep
    // the test compiling and to lock in the n-ary shape.
    let sessions: Vec<SessionRecord> = Vec::new();
    let cfg = SuggestConfig::from_env();
    let out = run_suggest_pipeline(&sessions, None, std::path::Path::new("."), &cfg);
    // With an empty corpus the pipeline returns no suggestions; the real
    // assertion (n-ary, single block) lives behind the slice-3 harness.
    let _ = out;
}

/// Every recommendation surfaced by `run_advice_render` must carry a
/// non-empty line range per participant — except for participants that
/// are inherently opaque (binary, submodule). This pins card outcome 4
/// (recommendations carry line ranges).
#[test]
#[ignore = "slice-2: contract pinned; render-harness wiring in slice 3"]
fn render_suggestions_carry_line_ranges() {
    // TODO(slice-3): wire fixture and assert that for every participant
    // p in every emitted suggestion: p.whole == false AND p.end >= p.start
    // AND p.end < u32::MAX, *unless* p.path is a known opaque type
    // (binary/submodule) — in which case skip the assertion.
    let sessions: Vec<SessionRecord> = Vec::new();
    let cfg = SuggestConfig::from_env();
    let out = run_suggest_pipeline(&sessions, None, std::path::Path::new("."), &cfg);
    let _ = out;
}

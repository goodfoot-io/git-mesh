//! Parity tests: run `git mesh advice <sid> suggest` against hand-crafted fixture
//! scenarios and assert the JSONL output matches `expected.jsonl` line-for-line
//! (after parsing each line as JSON so key ordering is ignored).
//!
//! Every test is `#[ignore]` until the Step 3 pipeline stages are implemented.
//! The fixture layout under `tests/fixtures/advice/<scenario>/` must match
//! the session-store schema consumed by `packages/git-mesh/src/advice/session/`.

mod support;

use std::path::Path;
use std::process::Command;

use anyhow::Result;
use serde_json::Value;

/// Manifest directory, resolved at compile time.
const MANIFEST_DIR: &str = env!("CARGO_MANIFEST_DIR");

/// Run `git mesh advice <session_id> suggest` with `GIT_MESH_ADVICE_DIR` set
/// to the given path and return stdout as a String. The binary is expected to
/// succeed (exit 0).
fn run_suggest(advice_dir: &Path, session_id: &str) -> Result<String> {
    let bin = env!("CARGO_BIN_EXE_git-mesh");
    // `git mesh advice suggest` is a hidden subcommand that does not require a
    // real git repository — it reads sessions from GIT_MESH_ADVICE_DIR
    // (post–Step 3) and emits one JSON line per suggestion.
    //
    // During Step 2 (TDD bootstrap) the command is a no-op stub and will emit
    // nothing. The test body is `#[ignore]`d so the assertion is not reached.
    let out = Command::new(bin)
        .env("GIT_MESH_ADVICE_DIR", advice_dir)
        // The `advice` subcommand requires a git repo; use a temp dir. When
        // the pipeline is implemented it will read sessions from the env var
        // rather than walking the repo, so this will still be valid.
        .env("HOME", "/tmp")
        .args(["advice", session_id, "suggest"])
        .output()
        .map_err(|e| anyhow::anyhow!("spawn git-mesh: {e}"))?;
    // NOTE: `git mesh advice <id>` currently requires a git repository in cwd.
    // In Step 3 the `suggest` subcommand will be refactored to not need one.
    // For now we just capture stdout regardless of exit status.
    Ok(String::from_utf8(out.stdout)
        .map_err(|e| anyhow::anyhow!("non-utf8 stdout: {e}"))?)
}

/// Parse every non-empty line of `text` as a `serde_json::Value`.
fn parse_jsonl(text: &str) -> Result<Vec<Value>> {
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str::<Value>(l).map_err(|e| anyhow::anyhow!("parse JSON: {e}")))
        .collect()
}

/// Load `expected.jsonl` from a fixture scenario directory.
fn load_expected(scenario: &str) -> Result<Vec<Value>> {
    let path = Path::new(MANIFEST_DIR)
        .join("tests/fixtures/advice")
        .join(scenario)
        .join("expected.jsonl");
    let text = std::fs::read_to_string(&path)
        .map_err(|e| anyhow::anyhow!("read {}: {e}", path.display()))?;
    parse_jsonl(&text)
}

// ---------------------------------------------------------------------------
// pair_only — two files co-touched in one session; expected: one size-2
// Suggestion via pair-escape hatch (ConfidenceBand::Low, Viability::Ready).
// ---------------------------------------------------------------------------

#[test]
#[ignore = "phase 3 — suggester not yet implemented"]
fn parity_pair_only() -> Result<()> {
    let fixture = Path::new(MANIFEST_DIR).join("tests/fixtures/advice/pair_only");
    let sessions_dir = fixture.join("sessions");

    let actual_text = run_suggest(&sessions_dir, "s1")?;
    let actual = parse_jsonl(&actual_text)?;
    let expected = load_expected("pair_only")?;

    assert_eq!(
        actual.len(),
        expected.len(),
        "pair_only: line count mismatch\nactual:\n{actual_text}\nexpected:\n{expected:?}"
    );
    for (i, (a, e)) in actual.iter().zip(expected.iter()).enumerate() {
        assert_eq!(
            a, e,
            "pair_only: line {i} mismatch\nactual:   {a}\nexpected: {e}"
        );
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// triad_strong — three files co-touched across multiple sessions with
// overlapping ranges; expected: one size-3 Suggestion at High band.
// ---------------------------------------------------------------------------

#[test]
#[ignore = "phase 3 — suggester not yet implemented"]
fn parity_triad_strong() -> Result<()> {
    let fixture = Path::new(MANIFEST_DIR).join("tests/fixtures/advice/triad_strong");
    let sessions_dir = fixture.join("sessions");

    let actual_text = run_suggest(&sessions_dir, "s1")?;
    let actual = parse_jsonl(&actual_text)?;
    let expected = load_expected("triad_strong")?;

    assert_eq!(
        actual.len(),
        expected.len(),
        "triad_strong: line count mismatch\nactual:\n{actual_text}\nexpected:\n{expected:?}"
    );
    for (i, (a, e)) in actual.iter().zip(expected.iter()).enumerate() {
        assert_eq!(
            a, e,
            "triad_strong: line {i} mismatch\nactual:   {a}\nexpected: {e}"
        );
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// subsumed_pair — a triad that also produces a constituent pair; expected:
// only the triad survives (subsumption suppression, pair is Superseded).
// ---------------------------------------------------------------------------

#[test]
#[ignore = "phase 3 — suggester not yet implemented"]
fn parity_subsumed_pair() -> Result<()> {
    let fixture = Path::new(MANIFEST_DIR).join("tests/fixtures/advice/subsumed_pair");
    let sessions_dir = fixture.join("sessions");

    let actual_text = run_suggest(&sessions_dir, "s1")?;
    let actual = parse_jsonl(&actual_text)?;
    let expected = load_expected("subsumed_pair")?;

    assert_eq!(
        actual.len(),
        expected.len(),
        "subsumed_pair: line count mismatch\nactual:\n{actual_text}\nexpected:\n{expected:?}"
    );
    for (i, (a, e)) in actual.iter().zip(expected.iter()).enumerate() {
        assert_eq!(
            a, e,
            "subsumed_pair: line {i} mismatch\nactual:   {a}\nexpected: {e}"
        );
    }
    Ok(())
}

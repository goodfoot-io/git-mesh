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

/// Run `git mesh advice suggest` with `GIT_MESH_ADVICE_DIR` set to the given
/// path and return stdout as a String. The binary must exit 0.
///
/// `repo_root` is passed as `GIT_MESH_SUGGEST_REPO_ROOT` so the cohesion
/// stage can find source files in the fixture directory.
///
/// `extra_env` is an optional slice of `(key, value)` pairs appended to the
/// child environment (e.g. `&[("GIT_MESH_SUGGEST_TRIGRAM", "0")]`).
fn run_suggest(advice_dir: &Path, repo_root: &Path, extra_env: &[(&str, &str)]) -> Result<String> {
    let bin = env!("CARGO_BIN_EXE_git-mesh");
    // `git mesh advice suggest` is a hidden subcommand that does not require a
    // real git repository — it reads sessions from GIT_MESH_ADVICE_DIR and
    // emits one JSON line per suggestion.
    let mut cmd = Command::new(bin);
    cmd.env("GIT_MESH_ADVICE_DIR", advice_dir)
        .env("GIT_MESH_SUGGEST_REPO_ROOT", repo_root)
        // Fixture runs must not pull in history from an unrelated repo.
        .env("GIT_MESH_SUGGEST_FIXTURE", "1")
        .env("HOME", "/tmp");
    for (k, v) in extra_env {
        cmd.env(k, v);
    }
    let out = cmd
        .args(["advice", "suggest"])
        .output()
        .map_err(|e| anyhow::anyhow!("spawn git-mesh: {e}"))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        anyhow::bail!(
            "git mesh advice suggest failed (exit {:?}):\n{stderr}",
            out.status.code()
        );
    }
    String::from_utf8(out.stdout).map_err(|e| anyhow::anyhow!("non-utf8 stdout: {e}"))
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
fn parity_pair_only() -> Result<()> {
    let fixture = Path::new(MANIFEST_DIR).join("tests/fixtures/advice/pair_only");
    let sessions_dir = fixture.join("sessions");

    // pair_only: trigram disabled (only 2 canonical ranges — shared tokens have
    // IDF=0 with N=2, so pair_cohesion is always 0 and the trigram gate would
    // filter the pair). The expected output reflects this with trigram:0.0.
    let actual_text = run_suggest(
        &sessions_dir,
        &fixture,
        &[("GIT_MESH_SUGGEST_TRIGRAM", "0")],
    )?;
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
fn parity_triad_strong() -> Result<()> {
    let fixture = Path::new(MANIFEST_DIR).join("tests/fixtures/advice/triad_strong");
    let sessions_dir = fixture.join("sessions");

    let actual_text = run_suggest(&sessions_dir, &fixture, &[])?;
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
fn parity_subsumed_pair() -> Result<()> {
    let fixture = Path::new(MANIFEST_DIR).join("tests/fixtures/advice/subsumed_pair");
    let sessions_dir = fixture.join("sessions");

    let actual_text = run_suggest(&sessions_dir, &fixture, &[])?;
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

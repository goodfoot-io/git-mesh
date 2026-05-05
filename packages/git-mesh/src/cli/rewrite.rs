//! `git mesh rewrite` handler — reads old→new SHA pairs from stdin and
//! advances mesh anchors via CAS (the post-rewrite hook protocol).

use crate::cli::RewriteArgs;
use crate::mesh::rewrite::{AnchorRewriteOutcome, RewriteOutcome, rewrite_meshes};
use anyhow::Result;
use std::collections::HashMap;
use std::io::Read as _;

pub fn run_rewrite(repo: &gix::Repository, args: RewriteArgs) -> Result<i32> {
    // Read all of stdin.
    let mut stdin_text = String::new();
    std::io::stdin()
        .read_to_string(&mut stdin_text)
        .map_err(|e| anyhow::anyhow!("failed to read stdin: {e}"))?;

    // Parse the old→new map.
    let map = match parse_map(&stdin_text) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("git-mesh rewrite: {e}");
            return Ok(1);
        }
    };

    if map.is_empty() {
        return Ok(0);
    }

    let outcomes = rewrite_meshes(repo, &map)?;

    // Render output.
    let use_json = matches!(args.format, crate::cli::RewriteFormat::Json);
    let mut hard_error = false;

    for outcome in &outcomes {
        if outcome.is_hard_error() {
            hard_error = true;
        }
        if use_json {
            if outcome.advanced > 0 || outcome.is_hard_error() {
                render_json_one(outcome)?;
            }
        } else {
            render_human_one(outcome);
        }
    }

    if hard_error { Ok(1) } else { Ok(0) }
}

fn parse_map(text: &str) -> Result<HashMap<String, String>, String> {
    let mut map: HashMap<String, String> = HashMap::new();
    for line in text.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let mut tokens = line.split_ascii_whitespace();
        let old_sha = tokens
            .next()
            .ok_or_else(|| format!("malformed line: {line:?}"))?;
        let new_sha = tokens
            .next()
            .ok_or_else(|| format!("malformed line: {line:?}"))?;

        if !is_valid_hex40(old_sha) {
            return Err(format!("malformed sha: {old_sha:?}"));
        }
        if !is_valid_hex40(new_sha) {
            return Err(format!("malformed sha: {new_sha:?}"));
        }

        // Drop old == new pairs silently.
        if old_sha == new_sha {
            continue;
        }

        // Duplicate old_sha is an error.
        if map.contains_key(old_sha) {
            return Err(format!("duplicate old_sha: {old_sha}"));
        }

        map.insert(old_sha.to_string(), new_sha.to_string());
    }
    Ok(map)
}

fn is_valid_hex40(s: &str) -> bool {
    s.len() == 40
        && s.chars()
            .all(|c| c.is_ascii_digit() || matches!(c, 'a'..='f'))
}

fn render_human_one(outcome: &RewriteOutcome) {
    if let Some(err) = &outcome.hard_error {
        eprintln!("git-mesh rewrite: {}: {}", outcome.name, err);
        return;
    }
    if outcome.advanced > 0 {
        let total = outcome.anchors.len() as u32;
        println!(
            "{}: advanced {}/{} anchors",
            outcome.name, outcome.advanced, total
        );
    }
    // Skipped anchors → stderr.
    for a in &outcome.anchors {
        match &a.outcome {
            AnchorRewriteOutcome::SkippedBlobChanged => {
                let new_sha = a.new_sha.as_deref().unwrap_or("?");
                eprintln!(
                    "{}: {} ({} → {}): blob changed",
                    outcome.name,
                    a.path,
                    &a.old_sha[..12.min(a.old_sha.len())],
                    &new_sha[..12.min(new_sha.len())]
                );
            }
            AnchorRewriteOutcome::SkippedPathMissing => {
                let new_sha = a.new_sha.as_deref().unwrap_or("?");
                eprintln!(
                    "{}: {} ({} → {}): path missing",
                    outcome.name,
                    a.path,
                    &a.old_sha[..12.min(a.old_sha.len())],
                    &new_sha[..12.min(new_sha.len())]
                );
            }
            _ => {}
        }
    }
}

fn render_json_one(outcome: &RewriteOutcome) -> Result<()> {
    let anchors: Vec<serde_json::Value> = outcome
        .anchors
        .iter()
        .map(|a| {
            serde_json::json!({
                "anchor_id": a.anchor_id,
                "outcome": match &a.outcome {
                    AnchorRewriteOutcome::Advanced => "advanced",
                    AnchorRewriteOutcome::SkippedBlobChanged => "skipped_blob_changed",
                    AnchorRewriteOutcome::SkippedPathMissing => "skipped_path_missing",
                    AnchorRewriteOutcome::ConflictExhausted => "conflict_exhausted",
                    AnchorRewriteOutcome::NoMatch => "no_match",
                },
                "old_sha": a.old_sha,
                "new_sha": a.new_sha,
                "path": a.path,
            })
        })
        .collect();

    let obj = serde_json::json!({
        "schema": "rewrite-v1",
        "mesh": outcome.name,
        "advanced": outcome.advanced,
        "skipped_blob_changed": outcome.skipped_blob_changed,
        "skipped_path_missing": outcome.skipped_path_missing,
        "errors": outcome.errors,
        "hard_error": outcome.hard_error,
        "anchors": anchors,
    });
    println!("{}", serde_json::to_string(&obj)?);
    Ok(())
}

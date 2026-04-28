//! Integration tests for the history stage.
//!
//! `git_log_name_only` tests use a real in-process gix repository (seeded via
//! bare git commands) so no subprocess is invoked from the tested function.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::process::Command;

use git_mesh::advice::suggest::SuggestConfig;
use git_mesh::advice::suggest::history::{HistoryIndex, load_git_history, pair_history_score};

// ---------------------------------------------------------------------------
// HistoryIndex helpers (no real repo)
// ---------------------------------------------------------------------------

fn make_history(
    commits_by_path: BTreeMap<String, BTreeSet<String>>,
    commit_weight: BTreeMap<String, f64>,
) -> HistoryIndex {
    let total = commit_weight.len();
    HistoryIndex {
        available: true,
        commits_by_path,
        commit_weight,
        total_commits: total,
        mass_refactor_cap: 12,
    }
}

#[test]
fn pair_score_no_overlap_returns_zero() {
    let mut cbp = BTreeMap::new();
    cbp.insert("a.rs".to_string(), ["c1".to_string()].into());
    cbp.insert("b.rs".to_string(), ["c2".to_string()].into());
    let mut cw = BTreeMap::new();
    cw.insert("c1".to_string(), 1.0);
    cw.insert("c2".to_string(), 1.0);
    let h = make_history(cbp, cw);
    let (count, weighted) = pair_history_score(&h, "a.rs", "b.rs");
    assert_eq!(count, 0);
    assert_eq!(weighted, 0.0);
}

#[test]
fn pair_score_shared_commit_accumulates_weight() {
    let mut cbp = BTreeMap::new();
    cbp.insert(
        "a.rs".to_string(),
        ["c1".to_string(), "c2".to_string()].into(),
    );
    cbp.insert("b.rs".to_string(), ["c1".to_string()].into());
    let mut cw = BTreeMap::new();
    cw.insert("c1".to_string(), 0.5);
    cw.insert("c2".to_string(), 0.9);
    let h = make_history(cbp, cw);
    let (count, weighted) = pair_history_score(&h, "a.rs", "b.rs");
    assert_eq!(count, 1);
    assert!((weighted - 0.5).abs() < 1e-9);
}

#[test]
fn pair_score_unavailable_returns_zero() {
    let h = HistoryIndex::default();
    let (c, w) = pair_history_score(&h, "x.rs", "y.rs");
    assert_eq!(c, 0);
    assert_eq!(w, 0.0);
}

// ---------------------------------------------------------------------------
// git_log_name_only — real gix repository
// ---------------------------------------------------------------------------

fn run_git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .current_dir(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&out.stderr)
    );
}

fn seed_repo_with_history() -> (tempfile::TempDir, gix::Repository) {
    let td = tempfile::tempdir().unwrap();
    let dir = td.path();
    run_git(dir, &["init", "--initial-branch=main"]);
    run_git(dir, &["config", "user.email", "t@t"]);
    run_git(dir, &["config", "user.name", "T"]);
    run_git(dir, &["config", "commit.gpgsign", "false"]);

    // commit 1: a.rs + b.rs
    std::fs::write(dir.join("a.rs"), "fn a() {}").unwrap();
    std::fs::write(dir.join("b.rs"), "fn b() {}").unwrap();
    run_git(dir, &["add", "."]);
    run_git(dir, &["commit", "-m", "c1"]);

    // commit 2: a.rs only
    std::fs::write(dir.join("a.rs"), "fn a2() {}").unwrap();
    run_git(dir, &["add", "a.rs"]);
    run_git(dir, &["commit", "-m", "c2"]);

    let repo = gix::open(dir).unwrap();
    (td, repo)
}

#[test]
fn git_log_name_only_captures_changed_paths() {
    let (_td, repo) = seed_repo_with_history();
    let commits = git_mesh::git::git_log_name_only(&repo, 10).unwrap();
    // Should have 2 commits (no merges).
    assert_eq!(commits.len(), 2);
    // Most recent commit: only a.rs changed.
    let recent = &commits[0];
    assert!(
        recent.changed_paths.contains(&"a.rs".to_string()),
        "most recent commit should touch a.rs"
    );
    assert!(
        !recent.changed_paths.contains(&"b.rs".to_string()),
        "most recent commit should NOT touch b.rs"
    );
    // Older commit: both a.rs and b.rs.
    let older = &commits[1];
    assert!(older.changed_paths.contains(&"a.rs".to_string()));
    assert!(older.changed_paths.contains(&"b.rs".to_string()));
}

#[test]
fn git_log_name_only_respects_n_cap() {
    let (_td, repo) = seed_repo_with_history();
    let commits = git_mesh::git::git_log_name_only(&repo, 1).unwrap();
    assert_eq!(commits.len(), 1, "n=1 must yield at most 1 commit");
}

#[test]
fn load_git_history_builds_correct_index() {
    let (_td, repo) = seed_repo_with_history();
    let paths = vec!["a.rs".to_string(), "b.rs".to_string()];
    let cfg = SuggestConfig::default();
    let history = load_git_history(&repo, &paths, &cfg).unwrap();
    assert!(history.available);
    // a.rs should appear in 2 commits.
    let a_count = history.commits_by_path.get("a.rs").map_or(0, |s| s.len());
    assert_eq!(a_count, 2);
    // b.rs should appear in 1 commit.
    let b_count = history.commits_by_path.get("b.rs").map_or(0, |s| s.len());
    assert_eq!(b_count, 1);
}

#[test]
fn git_log_name_only_parity_with_subprocess_n20() {
    // Spike: run against the actual workspace repo to verify parity.
    use std::collections::BTreeSet;
    // Anchor on CARGO_MANIFEST_DIR so the test works under nextest's sandboxed cwd.
    // WORKSPACE_PATH still overrides for callers that want to point elsewhere.
    let repo_path =
        std::env::var("WORKSPACE_PATH").unwrap_or_else(|_| env!("CARGO_MANIFEST_DIR").to_string());
    let mut candidate = std::path::PathBuf::from(&repo_path);
    let repo = loop {
        if candidate.join(".git").exists() {
            break gix::open(&candidate).expect("gix open");
        }
        if !candidate.pop() {
            eprintln!("skipping parity test: no .git ancestor from {repo_path}");
            return;
        }
    };

    let n = 20usize;
    let gix_commits = git_mesh::git::git_log_name_only(&repo, n).unwrap();

    let out = Command::new("git")
        .current_dir(repo.workdir().unwrap())
        .args([
            "log",
            "--name-only",
            "--no-merges",
            "--no-renames",
            &format!("-n{n}"),
            "--pretty=format:COMMIT:%H",
        ])
        .output()
        .unwrap();
    let stdout = String::from_utf8(out.stdout).unwrap();

    let mut git_commits: Vec<(String, BTreeSet<String>)> = Vec::new();
    let mut cur_hash: Option<String> = None;
    let mut cur_files: BTreeSet<String> = BTreeSet::new();
    for line in stdout.lines() {
        if let Some(hash) = line.strip_prefix("COMMIT:") {
            if let Some(h) = cur_hash.take() {
                git_commits.push((h, std::mem::take(&mut cur_files)));
            }
            cur_hash = Some(hash.to_string());
        } else {
            let f = line.trim();
            if !f.is_empty() {
                cur_files.insert(f.to_string());
            }
        }
    }
    if let Some(h) = cur_hash {
        git_commits.push((h, cur_files));
    }

    assert!(
        !gix_commits.is_empty() && !git_commits.is_empty(),
        "both sides must produce commits"
    );
    let check = gix_commits.len().min(git_commits.len());
    for i in 0..check {
        let gix = &gix_commits[i];
        let (git_hash, git_files) = &git_commits[i];
        assert_eq!(gix.hash, *git_hash, "hash mismatch at position {i}");
        let gix_files: BTreeSet<String> = gix.changed_paths.iter().cloned().collect();
        assert_eq!(
            gix_files, *git_files,
            "file set mismatch at commit {} (position {i})",
            gix.hash
        );
    }
}

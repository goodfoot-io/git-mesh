//! Integration test for the candidate-path commit-skipping optimization
//! in `ResolveSession`. Builds a realistic repo with many commits where
//! only a handful touch the anchored path, then resolves the mesh and
//! asserts:
//!
//! 1. The resolution outcome (status, current path/extent) matches what
//!    we'd get without any optimization (correctness baseline).
//! 2. The session counters confirm most commits were skipped.

mod support;

use anyhow::Result;
use git_mesh::types::{AnchorStatus, EngineOptions, LayerSet};
use git_mesh::{append_add, commit_mesh, resolve_mesh, set_why};
use support::TestRepo;

#[test]
fn most_commits_skipped_when_path_untouched() -> Result<()> {
    let repo = TestRepo::new()?;
    // Anchor file with 20 lines.
    let mut content = String::new();
    for i in 1..=20 {
        content.push_str(&format!("anchored_{i}\n"));
    }
    repo.write_file("anchored.txt", &content)?;
    repo.commit_all("init anchored")?;
    let anchor_sha = repo.head_sha()?;

    // 50 unrelated commits that don't touch anchored.txt.
    for i in 0..50 {
        repo.write_file(&format!("noise/{i}.txt"), &format!("n{i}\n"))?;
        repo.commit_all(&format!("noise {i}"))?;
    }

    // 2 commits that *do* touch anchored.txt: a non-overlapping edit
    // (lines 15-16) followed by a touch outside the anchored range.
    repo.write_file(
        "anchored.txt",
        &content.replace("anchored_15\n", "anchored_15_edited\n"),
    )?;
    repo.commit_all("edit anchored.txt outside anchor range")?;
    let head_after_edit = repo.head_sha()?;
    let _ = head_after_edit;

    // Stage and commit a mesh with one anchor at lines 1-5.
    let gix = repo.gix_repo()?;
    append_add(&gix, "demo/mesh", "anchored.txt", 1, 5, Some(&anchor_sha))?;
    set_why(&gix, "demo/mesh", "demo")?;
    commit_mesh(&gix, "demo/mesh")?;

    let resolved = resolve_mesh(
        &gix,
        "demo/mesh",
        EngineOptions {
            layers: LayerSet {
                index: false,
                worktree: false,
                staged_mesh: false,
            },
            since: None,
            ignore_unavailable: false,
            needs_all_layers: false,
        },
    )?;

    assert_eq!(resolved.anchors.len(), 1);
    let anchor = &resolved.anchors[0];
    // The lines we anchored (1-5) were untouched — still at 1-5.
    assert_eq!(anchor.status, AnchorStatus::Fresh);

    // Now resolve again and check that the session-level skip counter
    // reports most commits skipped. We use the CLI with GIT_MESH_PERF=1
    // to read counters out of stderr, since the public API doesn't
    // expose them directly.
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_git-mesh"))
        .current_dir(repo.path())
        .args(["stale"])
        .env("GIT_MESH_PERF", "1")
        .output()?;
    let stderr = String::from_utf8_lossy(&out.stderr);
    let interesting = parse_counter(&stderr, "session.interesting-commits");
    let skipped = parse_counter(&stderr, "session.skipped-commits");
    assert!(
        skipped >= 45,
        "expected most of the 50 noise commits to be skipped, got skipped={} interesting={} stderr=\n{}",
        skipped,
        interesting,
        stderr
    );
    assert!(
        interesting <= 6,
        "expected only the few real edits + any copy-detection-widened commits, got interesting={} skipped={}",
        interesting,
        skipped,
    );
    Ok(())
}

fn parse_counter(stderr: &str, label: &str) -> u64 {
    for line in stderr.lines() {
        if let Some(rest) = line.strip_prefix("git-mesh perf: ")
            && let Some(value_str) = rest.strip_prefix(&format!("{label} "))
            && let Ok(v) = value_str.trim().parse::<u64>()
        {
            return v;
        }
    }
    0
}

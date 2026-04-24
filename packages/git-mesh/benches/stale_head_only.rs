//! Slice 8 performance gate: HEAD-only `stale` on a comparable fixture.
//!
//! `EngineOptions::committed_only()` short-circuits the diff-files /
//! diff-index calls and the staging-dir walk. The bench measures
//! `stale_meshes` on a small synthetic fixture seeded once per run
//! (criterion handles the iteration loop). See
//! `docs/stale-layers-plan.md` §"Performance gate" — the >10%
//! regression policy is a CI-gate concern, not enforced here.

use criterion::{Criterion, criterion_group, criterion_main};
use git_mesh::{EngineOptions, stale_meshes};
use std::process::Command;

struct Fixture {
    _dir: tempfile::TempDir,
    repo_path: std::path::PathBuf,
}

fn build_fixture() -> Fixture {
    let dir = tempfile::tempdir().expect("tempdir");
    let p = dir.path().to_path_buf();
    let git = |args: &[&str]| {
        let out = Command::new("git")
            .current_dir(&p)
            .args(args)
            .output()
            .expect("spawn git");
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    git(&["init", "--initial-branch=main"]);
    git(&["config", "user.name", "Bench"]);
    git(&["config", "user.email", "bench@example.com"]);
    git(&["config", "commit.gpgsign", "false"]);
    // 10 small files, each with 20 lines.
    for i in 0..10u32 {
        let body: String = (0..20).map(|n| format!("line{i}_{n}\n")).collect();
        std::fs::write(p.join(format!("file_{i}.txt")), body).expect("write");
    }
    git(&["add", "."]);
    git(&["commit", "-m", "seed"]);

    // Pin a small mesh on each file. Use the binary directly.
    let mesh_bin = env!("CARGO_BIN_EXE_git-mesh");
    for i in 0..10u32 {
        let addr = format!("file_{i}.txt#L1-L5");
        let out = Command::new(mesh_bin)
            .current_dir(&p)
            .args(["add", "bench", &addr])
            .output()
            .expect("git-mesh add");
        assert!(out.status.success(), "mesh add: {out:?}");
    }
    let out = Command::new(mesh_bin)
        .current_dir(&p)
        .args(["message", "bench", "-m", "seed"])
        .output()
        .expect("git-mesh message");
    assert!(out.status.success());
    let out = Command::new(mesh_bin)
        .current_dir(&p)
        .args(["commit", "bench"])
        .output()
        .expect("git-mesh commit");
    assert!(out.status.success(), "mesh commit: {out:?}");

    Fixture {
        _dir: dir,
        repo_path: p,
    }
}

fn bench_head_only(c: &mut Criterion) {
    let f = build_fixture();
    c.bench_function("stale_meshes_head_only", |b| {
        b.iter(|| {
            let repo = gix::open(&f.repo_path).expect("open repo");
            let out = stale_meshes(&repo, EngineOptions::committed_only()).expect("stale");
            std::hint::black_box(out);
        });
    });
}

criterion_group!(benches, bench_head_only);
criterion_main!(benches);

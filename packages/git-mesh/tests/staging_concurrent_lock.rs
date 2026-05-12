//! Per-mesh staging mutation lock (main-59-1).
//!
//! [`append_prepared_add`](../../src/staging.rs) is a read-modify-write:
//! it reads the ops file to compute `<N>`, writes the sidecar + meta
//! under `<N>`, then appends the ops line. Two writers on the same
//! mesh can interleave at the prospective-N read and silently clobber
//! each other's sidecar bytes — or worse, leave the ops line for A
//! pointing at sidecar bytes written by B.
//!
//! The per-mesh lock (`<staging>/<encoded-name>.lock`) serializes the
//! whole read → write sequence so concurrent writers on the **same**
//! mesh land cleanly, and verifies that concurrent writers on
//! **different** meshes are not mutually serialized.

mod support;

use anyhow::Result;
use git_mesh::cli::structural::{DoctorCode, Severity, doctor_run};
use git_mesh::{append_add, read_staging};
use std::path::PathBuf;
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::Instant;
use support::TestRepo;

/// Stage `count` line-anchor adds against `mesh` from `thread_count`
/// threads driven through a shared `Barrier`. Each (thread, i) writes
/// a distinct `(path, range)` so no add is superseded by another.
fn drive_concurrent_adds_same_mesh(
    path: PathBuf,
    mesh: &'static str,
    thread_count: usize,
    per_thread: u32,
) -> Result<()> {
    let barrier = Arc::new(Barrier::new(thread_count));
    let handles: Vec<_> = (0..thread_count)
        .map(|t| {
            let path = path.clone();
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || -> Result<()> {
                let gix = gix::open(&path)?;
                barrier.wait();
                for i in 0..per_thread {
                    let start = (t as u32) * per_thread + i + 1;
                    let end = start;
                    append_add(&gix, mesh, "file1.txt", start, end, None)?;
                }
                Ok(())
            })
        })
        .collect();
    for h in handles {
        h.join().expect("staging thread panicked")?;
    }
    Ok(())
}

#[test]
fn concurrent_same_mesh_writers_do_not_lose_adds_or_mismatch_pairs() -> Result<()> {
    let repo = TestRepo::seeded()?;
    // Seed a large enough file so each thread can claim a unique
    // line-anchor range without colliding on (path, extent).
    repo.write_file_lines("file1.txt", 200)?;
    repo.commit_all("widen file1")?;

    let thread_count = 4;
    let per_thread: u32 = 5;
    let expected: usize = thread_count * per_thread as usize;

    drive_concurrent_adds_same_mesh(repo.path().to_owned(), "m", thread_count, per_thread)?;

    let gix = repo.gix_repo()?;
    let staging = read_staging(&gix, "m")?;
    assert_eq!(
        staging.adds.len(),
        expected,
        "every concurrent add must land; lost-update would shrink this count. \
         adds = {:#?}",
        staging.adds
    );

    // Every staged add line must have a paired sidecar + meta under
    // the slot the parser assigned. A mismatched ops/sidecar pair
    // surfaces as a doctor StagingCorrupt Error.
    let staging_dir = repo.path().join(".git/mesh/staging");
    for n in 1..=expected as u32 {
        let sidecar = staging_dir.join(format!("m.{n}"));
        let meta = staging_dir.join(format!("m.{n}.meta"));
        assert!(sidecar.exists(), "missing sidecar m.{n} after concurrent stage");
        assert!(meta.exists(), "missing sidecar meta m.{n}.meta after concurrent stage");
    }

    let findings = doctor_run(&gix)?;
    let staging_errors: Vec<_> = findings
        .iter()
        .filter(|f| f.code == DoctorCode::StagingCorrupt && f.severity == Severity::Error)
        .collect();
    assert!(
        staging_errors.is_empty(),
        "doctor must report no StagingCorrupt Errors; findings = {findings:#?}"
    );

    // The lockfile is released on every writer's exit — no `.lock` file
    // should remain when staging is quiescent.
    let lockfile = staging_dir.join("m.lock");
    assert!(
        !lockfile.exists(),
        "per-mesh lockfile leaked: {} still present",
        lockfile.display()
    );

    Ok(())
}

#[test]
fn concurrent_different_mesh_writers_are_not_serialized() -> Result<()> {
    let repo = TestRepo::seeded()?;
    repo.write_file_lines("file1.txt", 400)?;
    repo.commit_all("widen file1")?;

    // Baseline: how long does serialized work on one mesh take? We use
    // the same per-mesh workload size for both meshes in the parallel
    // run, so this is a tight upper bound on what one mesh's serial
    // work costs.
    let per_mesh_adds: u32 = 40;
    let path = repo.path().to_owned();

    let serial_start = Instant::now();
    {
        let gix = gix::open(&path)?;
        for i in 1..=per_mesh_adds {
            append_add(&gix, "serial", "file1.txt", i, i, None)?;
        }
    }
    let serial_one_mesh = serial_start.elapsed();

    // Parallel: two threads, one per mesh. With per-mesh locking these
    // should not serialize against each other, so wall time should be
    // close to (not 2×) the single-mesh baseline.
    let barrier = Arc::new(Barrier::new(2));
    let parallel_start = Instant::now();
    let handles: Vec<_> = ["alpha", "bravo"]
        .into_iter()
        .map(|mesh| {
            let path = path.clone();
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || -> Result<()> {
                let gix = gix::open(&path)?;
                barrier.wait();
                for i in 1..=per_mesh_adds {
                    append_add(&gix, mesh, "file1.txt", i, i, None)?;
                }
                Ok(())
            })
        })
        .collect();
    for h in handles {
        h.join().expect("staging thread panicked")?;
    }
    let parallel_two_meshes = parallel_start.elapsed();

    // Both meshes must contain every add.
    let gix = repo.gix_repo()?;
    for mesh in ["alpha", "bravo"] {
        let staging = read_staging(&gix, mesh)?;
        assert_eq!(
            staging.adds.len(),
            per_mesh_adds as usize,
            "mesh `{mesh}` lost adds under parallel cross-mesh writes",
        );
    }

    // Parallel work on two independent meshes must complete in less
    // than 1.8× the single-mesh serial baseline. A global lock would
    // push this toward 2× (plus overhead); a correct per-mesh lock
    // keeps it near 1×. The bound is loose enough to absorb scheduler
    // jitter on small workloads.
    let bound = serial_one_mesh.mul_f64(1.8);
    assert!(
        parallel_two_meshes < bound,
        "cross-mesh writers are being serialized: parallel {parallel_two_meshes:?} \
         >= 1.8× serial {serial_one_mesh:?} (bound {bound:?})",
    );

    Ok(())
}

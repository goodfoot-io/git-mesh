//! Reproduction for the non-atomic ops/sidecar pairing defect in
//! `append_prepared_add`.
//!
//! `append_prepared_add` (src/staging.rs) writes the ops line first
//! (`append_line`), then the sidecar bytes, then the sidecar `.meta`
//! JSON. A crash between step 1 and step 2 leaves a durable `add` line
//! on disk with no paired sidecar. `git mesh doctor` flags this as a
//! `StagingCorrupt` ERROR with only a destructive remediation
//! (`git mesh restore <mesh>` + re-stage).
//!
//! CARD invariant: a writer interrupted mid-pair must leave the
//! staging area self-consistent — no `StagingCorrupt` ERROR for a
//! missing sidecar, and a clean re-read (auto-recovery acceptable).
//!
//! This test simulates the interrupted-mid-pair state by deleting the
//! sidecar (`.git/mesh/staging/<name>.<N>`) and its `.meta` companion
//! after a successful `append_add`, then asserts the doctor emits no
//! ERROR-severity `StagingCorrupt` finding for the missing sidecar.

mod support;

use anyhow::Result;
use git_mesh::append_add;
use git_mesh::cli::structural::{DoctorCode, Severity, doctor_run};
use std::fs;
use support::TestRepo;

#[test]
fn interrupted_mid_pair_write_does_not_yield_staging_corrupt_error() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let gix = repo.gix_repo()?;

    // Stage a single line-anchor add. After this call the ops file,
    // sidecar, and sidecar `.meta` all exist on disk (the happy path).
    append_add(&gix, "m", "file1.txt", 1, 5, None)?;

    // Simulate a crash *between* the ops-line append and the sidecar
    // write: the ops line is durable but the sidecar (and meta) are
    // absent. This is the exact intermediate state of `append_prepared_add`
    // after `append_line(..)` and before `fs::write(sidecar_path ..)`.
    let staging_dir = repo.path().join(".git/mesh/staging");
    let sidecar = staging_dir.join("m.1");
    let sidecar_meta = staging_dir.join("m.1.meta");
    assert!(
        sidecar.exists(),
        "precondition: append_add must have produced m.1 sidecar"
    );
    fs::remove_file(&sidecar)?;
    let _ = fs::remove_file(&sidecar_meta);

    // Sanity: ops file still has the `add` line — the durable
    // "step-1 happened, step-2 didn't" state.
    let ops_text = fs::read_to_string(staging_dir.join("m"))?;
    assert!(
        ops_text.lines().any(|l| l.starts_with("add ")),
        "ops file must still carry the add line; got: {ops_text:?}"
    );

    // Invariant: doctor must not raise an ERROR-severity StagingCorrupt
    // finding for the missing sidecar. A Warn-level orphan, or
    // auto-recovery to a clean read, is acceptable.
    let findings = doctor_run(&gix)?;
    let error_missing_sidecar: Vec<_> = findings
        .iter()
        .filter(|f| {
            f.code == DoctorCode::StagingCorrupt
                && f.severity == Severity::Error
                && f.message.contains("missing sidecar")
        })
        .collect();

    assert!(
        error_missing_sidecar.is_empty(),
        "doctor raised ERROR-level StagingCorrupt for missing sidecar after \
         an interrupted ops/sidecar pair write; this is the non-atomic-write \
         defect. findings = {findings:#?}"
    );

    Ok(())
}

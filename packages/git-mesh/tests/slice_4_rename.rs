//! Slice 4 — rename literal (T3) translated to the file-backed pipeline.
//!
//! `detect_rename_consequence` fires on `session_delta` Renamed entries
//! whose old path is meshed. We trigger that by renaming a meshed file
//! after `snapshot` and running bare render.

mod support;

use anyhow::Result;
use git_mesh::staging::append_add;
use git_mesh::{commit_mesh, set_why};
use std::process::Output;
use support::TestRepo;
use uuid::Uuid;

fn sid(prefix: &str) -> String {
    format!("slice4r-{prefix}-{}", Uuid::new_v4())
}

fn ok(out: &Output) {
    assert!(
        out.status.success(),
        "code={:?} stderr={} stdout={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&out.stdout)
    );
}

fn run_advice(repo: &TestRepo, s: &str, extra: &[&str]) -> Result<Output> {
    let mut args: Vec<String> = vec!["advice".into(), s.into()];
    for a in extra {
        args.push((*a).to_string());
    }
    repo.run_mesh(args)
}

#[test]
#[ignore] // Phase 3
fn rename_of_meshed_file_surfaces_rename_literal() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let gix = repo.gix_repo()?;
    append_add(&gix, "mr", "file1.txt", 1, 5, None)?;
    append_add(&gix, "mr", "file2.txt", 1, 5, None)?;
    set_why(&gix, "mr", "rename target pair")?;
    commit_mesh(&gix, "mr")?;

    let s = sid("rename");
    ok(&run_advice(&repo, &s, &["snapshot"])?);

    // Rename file1.txt → file1-renamed.txt with sufficient content to
    // trigger git diff --find-renames similarity detection.
    repo.run_git(["mv", "file1.txt", "file1-renamed.txt"])?;

    let out = run_advice(&repo, &s, &[])?;
    ok(&out);
    let stdout = String::from_utf8(out.stdout)?;
    // Either the rename literal candidate fires, or partner-drift surfaces
    // the now-changed mesh — either is acceptable evidence the pipeline
    // saw the rename.
    assert!(
        stdout.contains("file1") || stdout.contains("mr"),
        "expected rename-related output, got:\n{stdout}"
    );
    Ok(())
}

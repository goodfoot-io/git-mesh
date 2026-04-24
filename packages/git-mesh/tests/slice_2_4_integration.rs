//! Slices 2 + 4 of `docs/git-mesh-review-plan.md`: byte-safe whole-file
//! re-anchor ack and sidecar tamper detection.

mod support;

use anyhow::Result;
use support::TestRepo;

// ---------------------------------------------------------------------------
// Slice 2 — byte-safe whole-file re-anchor ack.
// ---------------------------------------------------------------------------

/// PNG-like binary asset with high bytes (0x80..=0xFF) that
/// `String::from_utf8_lossy` would have corrupted into U+FFFD. Whole-file
/// re-anchor must produce `(ack)` and exit 0.
#[test]
fn whole_file_binary_re_anchor_acks_with_high_bytes() -> Result<()> {
    let repo = TestRepo::seeded()?;
    // PNG signature + a couple of high bytes — every one of these would
    // be replaced by U+FFFD under `from_utf8_lossy`.
    let v1: Vec<u8> = vec![
        0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0xFF, 0xFE, 0xFD, 0xFC,
    ];
    std::fs::write(repo.path().join("hero.png"), &v1)?;
    repo.commit_all("add binary")?;

    let out = repo.run_mesh(["add", "m", "hero.png"])?;
    assert_eq!(out.status.code(), Some(0));
    repo.run_mesh(["why", "m", "-m", "seed"])?;
    let out = repo.run_mesh(["commit", "m"])?;
    assert_eq!(out.status.code(), Some(0));

    // Mutate to a different binary payload and `git add` it.
    let v2: Vec<u8> = vec![
        0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0xAA, 0xBB, 0xCC, 0xDD,
    ];
    std::fs::write(repo.path().join("hero.png"), &v2)?;
    repo.run_git(["add", "hero.png"])?;

    // Re-anchor with `git mesh add`. This captures the new bytes into a
    // fresh sidecar that should ack the now-stale range.
    let out = repo.run_mesh(["add", "m", "hero.png"])?;
    assert_eq!(out.status.code(), Some(0));

    let out = repo.run_mesh(["stale", "m"])?;
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        stdout.contains("(ack)"),
        "expected (ack) in stale output, got: {stdout}"
    );
    assert_eq!(
        out.status.code(),
        Some(0),
        "stale must exit 0 after ack: {stdout}"
    );
    Ok(())
}

/// Symlink retarget pin: the readlink-string compare doesn't go through
/// the byte normalizer, but we still need a regression test.
#[test]
#[cfg(unix)]
fn whole_file_symlink_re_anchor_acks() -> Result<()> {
    let repo = TestRepo::seeded()?;
    std::os::unix::fs::symlink("file1.txt", repo.path().join("link"))?;
    repo.commit_all("add symlink")?;
    repo.run_mesh(["add", "m", "link"])?;
    repo.run_mesh(["why", "m", "-m", "seed"])?;
    repo.run_mesh(["commit", "m"])?;

    // Retarget the symlink and stage the change.
    std::fs::remove_file(repo.path().join("link"))?;
    std::os::unix::fs::symlink("file2.txt", repo.path().join("link"))?;
    repo.run_git(["add", "link"])?;

    repo.run_mesh(["add", "m", "link"])?;
    let out = repo.run_mesh(["stale", "m"])?;
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(stdout.contains("(ack)"), "expected (ack), got: {stdout}");
    assert_eq!(out.status.code(), Some(0), "{stdout}");
    Ok(())
}

/// Text path with a normalization-stamp change mid-session: byte-identical
/// content (no CRLF in either side) must NOT register as drift on the
/// pending op. (No `(ack)` is required — there is no committed range
/// drift to acknowledge in this scenario; what we are guarding against
/// is the stamp-change path producing a spurious sidecar mismatch.)
#[test]
fn text_re_add_after_stamp_change_no_drift() -> Result<()> {
    let repo = TestRepo::seeded()?;
    std::fs::write(repo.path().join("note.txt"), "alpha\nbeta\ngamma\n")?;
    repo.commit_all("add text")?;

    repo.run_mesh(["add", "m", "note.txt"])?;
    repo.run_mesh(["why", "m", "-m", "seed"])?;
    repo.run_mesh(["commit", "m"])?;

    // Bump the normalization stamp by introducing a `.gitattributes`.
    std::fs::write(repo.path().join(".gitattributes"), "*.txt text\n")?;
    repo.commit_all("add gitattributes")?;

    // Live bytes byte-identical to the captured sidecar; re-add — the
    // pending op must not show `(drift: sidecar mismatch)`.
    repo.run_mesh(["add", "m", "note.txt"])?;
    let out = repo.run_mesh(["stale", "m"])?;
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        !stdout.contains("drift: sidecar mismatch"),
        "no spurious drift should be reported, got: {stdout}"
    );
    assert_eq!(out.status.code(), Some(0), "{stdout}");
    Ok(())
}

// ---------------------------------------------------------------------------
// Slice 4 — sidecar tamper detection.
// ---------------------------------------------------------------------------

fn tamper_sidecar(repo: &TestRepo, mesh: &str, n: u32) -> Result<()> {
    let p = repo
        .path()
        .join(".git")
        .join("mesh")
        .join("staging")
        .join(format!("{mesh}.{n}"));
    std::fs::write(p, b"GARBAGE BYTES NOT MATCHING THE ORIGINAL HASH")?;
    Ok(())
}

#[test]
fn commit_fails_on_tampered_sidecar() -> Result<()> {
    let repo = TestRepo::seeded()?;
    repo.run_mesh(["add", "m", "file1.txt#L1-L3"])?;
    repo.run_mesh(["why", "m", "-m", "seed"])?;
    tamper_sidecar(&repo, "m", 1)?;
    let out = repo.run_mesh(["commit", "m"])?;
    assert_ne!(
        out.status.code(),
        Some(0),
        "commit must fail on tampered sidecar"
    );
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(
        stderr.contains("tampered"),
        "expected tampered message, got: {stderr}"
    );
    Ok(())
}

#[test]
fn stale_surfaces_tampered_drift() -> Result<()> {
    let repo = TestRepo::seeded()?;
    // Establish an initial committed mesh so `stale m` has something to
    // resolve, then stage a fresh add and tamper its sidecar.
    repo.run_mesh(["add", "m", "file2.txt#L1-L2"])?;
    repo.run_mesh(["why", "m", "-m", "seed"])?;
    repo.run_mesh(["commit", "m"])?;
    repo.run_mesh(["add", "m", "file1.txt#L1-L3"])?;
    tamper_sidecar(&repo, "m", 1)?;
    let out = repo.run_mesh(["stale", "m"])?;
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        stdout.contains("tampered"),
        "expected tampered, got: {stdout}"
    );
    assert_ne!(
        out.status.code(),
        Some(0),
        "stale must exit non-zero on tamper"
    );
    Ok(())
}

#[test]
fn doctor_reports_tampered_sidecar() -> Result<()> {
    let repo = TestRepo::seeded()?;
    repo.run_mesh(["add", "m", "file1.txt#L1-L3"])?;
    tamper_sidecar(&repo, "m", 1)?;
    let out = repo.run_mesh(["doctor"])?;
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        stdout.contains("SidecarTampered"),
        "expected SidecarTampered finding, got: {stdout}"
    );
    Ok(())
}

#[test]
fn stale_json_distinguishes_tampered_from_mismatch() -> Result<()> {
    let repo = TestRepo::seeded()?;
    repo.run_mesh(["add", "m", "file2.txt#L1-L2"])?;
    repo.run_mesh(["why", "m", "-m", "seed"])?;
    repo.run_mesh(["commit", "m"])?;
    repo.run_mesh(["add", "m", "file1.txt#L1-L3"])?;
    tamper_sidecar(&repo, "m", 1)?;
    let out = repo.run_mesh(["stale", "m", "--format", "json"])?;
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        stdout.contains("SIDECAR_TAMPERED"),
        "expected SIDECAR_TAMPERED in JSON, got: {stdout}"
    );
    Ok(())
}

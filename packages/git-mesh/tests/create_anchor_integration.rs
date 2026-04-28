//! Library tests for `anchor::create_anchor`, `read_anchor`, and the
//! parse/serialize round-trip (§4.1, §6.1).

mod support;

use anyhow::Result;
use git_mesh::types::{Anchor, AnchorExtent};
use git_mesh::{anchor_ref_path, create_anchor, parse_anchor, read_anchor, serialize_anchor};
use support::TestRepo;

#[test]

fn anchor_ref_path_is_canonical() {
    assert_eq!(
        anchor_ref_path("0123456789abcdef"),
        "refs/anchors/v1/0123456789abcdef"
    );
}

#[test]

fn parse_serialize_round_trip() -> Result<()> {
    let original = Anchor {
        anchor_sha: "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef".into(),
        created_at: "2026-01-01T00:00:00Z".into(),
        path: "src/auth.ts".into(),
        extent: AnchorExtent::LineRange { start: 13, end: 34 },
        blob: "cafebabecafebabecafebabecafebabecafebabe".into(),
    };
    let text = serialize_anchor(&original);
    assert!(text.ends_with('\n'), "spec §4.1 mandates trailing newline");
    let round = parse_anchor(&text)?;
    assert_eq!(round, original);
    Ok(())
}

#[test]

fn parse_tolerates_unknown_headers() -> Result<()> {
    let text = "commit deadbeefdeadbeefdeadbeefdeadbeefdeadbeef\ncreated 2026-01-01T00:00:00Z\nfuture some-value\nextent 1 10 cafebabecafebabecafebabecafebabecafebabe\tsrc/x.rs\n";
    let r = parse_anchor(text)?;
    assert_eq!(r.path, "src/x.rs");
    assert_eq!(r.extent, AnchorExtent::LineRange { start: 1, end: 10 });
    Ok(())
}

#[test]

fn parse_handles_paths_with_spaces() -> Result<()> {
    let text = "commit deadbeefdeadbeefdeadbeefdeadbeefdeadbeef\ncreated 2026-01-01T00:00:00Z\nextent 1 5 cafebabecafebabecafebabecafebabecafebabe\tsrc/a b/c d.rs\n";
    let r = parse_anchor(text)?;
    assert_eq!(r.path, "src/a b/c d.rs");
    Ok(())
}

#[test]

fn create_anchor_writes_blob_and_ref() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let head = repo.head_sha()?;
    let id = create_anchor(&repo.gix_repo()?, &head, "file1.txt", 1, 5)?;
    assert!(repo.ref_exists(&anchor_ref_path(&id)));
    let r = read_anchor(&repo.gix_repo()?, &id)?;
    assert_eq!(r.path, "file1.txt");
    assert_eq!(r.extent, AnchorExtent::LineRange { start: 1, end: 5 });
    assert_eq!(r.anchor_sha, head);
    Ok(())
}

#[test]

fn create_anchor_rejects_path_not_in_tree() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let head = repo.head_sha()?;
    let err = create_anchor(&repo.gix_repo()?, &head, "no/such.txt", 1, 1).unwrap_err();
    matches!(err, git_mesh::Error::PathNotInTree { .. });
    Ok(())
}

#[test]

fn create_anchor_rejects_start_zero() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let head = repo.head_sha()?;
    let err = create_anchor(&repo.gix_repo()?, &head, "file1.txt", 0, 5).unwrap_err();
    assert!(matches!(err, git_mesh::Error::InvalidAnchor { .. }));
    Ok(())
}

#[test]

fn create_anchor_rejects_end_lt_start() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let head = repo.head_sha()?;
    let err = create_anchor(&repo.gix_repo()?, &head, "file1.txt", 8, 3).unwrap_err();
    assert!(matches!(err, git_mesh::Error::InvalidAnchor { .. }));
    Ok(())
}

#[test]

fn create_anchor_rejects_end_past_eof() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let head = repo.head_sha()?;
    let err = create_anchor(&repo.gix_repo()?, &head, "file1.txt", 1, 999).unwrap_err();
    assert!(matches!(err, git_mesh::Error::InvalidAnchor { .. }));
    Ok(())
}

#[test]

fn create_anchor_orphan_anchor_errors() -> Result<()> {
    let repo = TestRepo::seeded()?;
    let bogus = "0000000000000000000000000000000000000000";
    let err = create_anchor(&repo.gix_repo()?, bogus, "file1.txt", 1, 5).unwrap_err();
    assert!(matches!(
        err,
        git_mesh::Error::Unreachable { .. } | git_mesh::Error::PathNotInTree { .. }
    ));
    Ok(())
}

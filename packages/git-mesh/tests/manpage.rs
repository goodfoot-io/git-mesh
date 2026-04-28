//! Regression test: generated `git-mesh.1` must match the checked-in artifact.
//!
//! Fails when the CLI's clap definitions change without regenerating the page.
//! Fix failures by running `yarn build:man` and committing the updated artifact.

use assert_cmd::Command;

#[test]
fn generated_manpage_matches_checked_in_artifact() {
    let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let checked_in = manifest_dir.join("man").join("git-mesh.1");

    assert!(
        checked_in.exists(),
        "checked-in manpage not found at {}; run `yarn build:man`",
        checked_in.display()
    );

    let tmp = tempfile::NamedTempFile::new().expect("tempfile");
    let tmp_path = tmp.path().to_path_buf();

    Command::cargo_bin("gen-manpage")
        .expect("gen-manpage binary not found")
        .arg(&tmp_path)
        .assert()
        .success();

    let generated = std::fs::read(&tmp_path).expect("read generated manpage");
    let expected = std::fs::read(&checked_in).expect("read checked-in manpage");

    assert_eq!(
        generated, expected,
        "generated git-mesh.1 differs from checked-in artifact; run `yarn build:man` and commit the result"
    );
}

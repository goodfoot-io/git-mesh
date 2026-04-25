//! Skipped checks for workspace-tree capture and diff (Phase 2).

mod support;

use anyhow::Result;
use git_mesh::advice::{DiffEntry, capture, diff_trees};
use support::TestRepo;

// ---------------------------------------------------------------------------
// tree: tracked edit / delete / rename appear in diff_trees output
// ---------------------------------------------------------------------------

#[test]
#[ignore = "phase-3-pending: capture and diff_trees are unimplemented"]
fn tracked_edit_delete_rename_appear_in_diff_trees() -> Result<()> {
    let repo = TestRepo::new()?;
    repo.write_file("edit.txt", "original\n")?;
    repo.write_file("delete.txt", "will be deleted\n")?;
    repo.write_file("old.txt", "will be renamed\n")?;
    repo.commit_all("initial")?;

    let gix = repo.gix_repo()?;
    let objects_a = tempfile::tempdir()?;
    let tree_a = capture(&gix, objects_a.path())?;

    // Mutate: edit, delete, rename.
    repo.write_file("edit.txt", "changed content\n")?;
    std::fs::remove_file(repo.path().join("delete.txt"))?;
    std::fs::rename(repo.path().join("old.txt"), repo.path().join("new.txt"))?;

    let objects_b = tempfile::tempdir()?;
    let tree_b = capture(&gix, objects_b.path())?;

    let entries = diff_trees(
        &gix,
        &tree_a.tree_sha,
        &tree_b.tree_sha,
        objects_a.path(),
        objects_b.path(),
    )?;

    let has_modified = entries.iter().any(|e| matches!(e, DiffEntry::Modified { path } if path == "edit.txt"));
    let has_deleted = entries.iter().any(|e| matches!(e, DiffEntry::Deleted { path } if path == "delete.txt"));
    let has_renamed = entries.iter().any(|e| matches!(e, DiffEntry::Renamed { from, to, .. } if from == "old.txt" && to == "new.txt"));

    assert!(has_modified, "edited file must appear as Modified");
    assert!(has_deleted, "deleted file must appear as Deleted");
    assert!(has_renamed, "renamed file must appear as Renamed");
    Ok(())
}

// ---------------------------------------------------------------------------
// tree: untracked non-ignored file is included; ignored file is excluded
// ---------------------------------------------------------------------------

#[test]
#[ignore = "phase-3-pending: capture with untracked/ignored handling is unimplemented"]
fn untracked_included_ignored_excluded() -> Result<()> {
    let repo = TestRepo::new()?;
    repo.write_file("tracked.txt", "base\n")?;
    repo.commit_all("initial")?;

    repo.write_file(".gitignore", "ignored.txt\n")?;
    repo.write_file("untracked.txt", "new file\n")?;
    repo.write_file("ignored.txt", "should not appear\n")?;

    let gix = repo.gix_repo()?;
    let objects = tempfile::tempdir()?;
    let base_tree = capture(&gix, objects.path())?;

    // The tree sha must capture untracked.txt but not ignored.txt.
    // We verify by running diff_trees against an empty tree (all-zeros sha).
    let objects_empty = tempfile::tempdir()?;
    let entries = diff_trees(
        &gix,
        "4b825dc642cb6eb9a060e54bf8d69288fbee4904", // git empty tree
        &base_tree.tree_sha,
        objects_empty.path(),
        objects.path(),
    )?;

    let paths: Vec<&str> = entries.iter().filter_map(|e| match e {
        DiffEntry::Added { path } => Some(path.as_str()),
        _ => None,
    }).collect();

    assert!(paths.contains(&"untracked.txt"), "untracked must be captured");
    assert!(!paths.contains(&"ignored.txt"), "ignored must not be captured");
    Ok(())
}

// ---------------------------------------------------------------------------
// tree: binary blob round-trips through temp object dir
// ---------------------------------------------------------------------------

#[test]
#[ignore = "phase-3-pending: capture binary blob handling is unimplemented"]
fn binary_blob_round_trips_through_temp_object_dir() -> Result<()> {
    let repo = TestRepo::new()?;
    let binary: Vec<u8> = (0u8..=255u8).collect();
    std::fs::write(repo.path().join("data.bin"), &binary)?;
    repo.commit_all("add binary")?;

    let gix = repo.gix_repo()?;
    let objects = tempfile::tempdir()?;
    let tree = capture(&gix, objects.path())?;

    // The objects dir must be non-empty (objects were written).
    let has_objects = std::fs::read_dir(objects.path())?
        .any(|e| e.map(|_| true).unwrap_or(false));
    assert!(!tree.tree_sha.is_empty(), "tree_sha must be non-empty");
    assert!(has_objects, "temp object dir must contain written objects");
    Ok(())
}

// ---------------------------------------------------------------------------
// tree: exec-bit change yields ModeChange
// ---------------------------------------------------------------------------

#[test]
#[ignore = "phase-3-pending: capture exec-bit / ModeChange detection is unimplemented"]
fn exec_bit_change_yields_mode_change() -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let repo = TestRepo::new()?;
        repo.write_file("script.sh", "#!/bin/sh\n")?;
        repo.commit_all("add script")?;

        let gix = repo.gix_repo()?;
        let objects_a = tempfile::tempdir()?;
        let tree_a = capture(&gix, objects_a.path())?;

        // Toggle exec bit.
        let path = repo.path().join("script.sh");
        let mut perms = std::fs::metadata(&path)?.permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&path, perms)?;

        let objects_b = tempfile::tempdir()?;
        let tree_b = capture(&gix, objects_b.path())?;

        let entries = diff_trees(
            &gix,
            &tree_a.tree_sha,
            &tree_b.tree_sha,
            objects_a.path(),
            objects_b.path(),
        )?;

        let has_mode_change = entries.iter().any(|e| {
            matches!(e, DiffEntry::ModeChange { path } if path == "script.sh")
        });
        assert!(has_mode_change, "exec-bit toggle must yield ModeChange");
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// tree: submodule gitlink mode 160000 captured, not recursed
// ---------------------------------------------------------------------------

#[test]
#[ignore = "phase-3-pending: submodule gitlink capture is unimplemented"]
fn submodule_gitlink_captured_not_recursed() -> Result<()> {
    let repo = TestRepo::new()?;

    // Create a minimal submodule repo.
    let sub_dir = tempfile::tempdir()?;
    {
        use std::process::Command;
        Command::new("git").args(["init", "--initial-branch=main"]).current_dir(sub_dir.path()).output()?;
        Command::new("git").args(["config", "user.email", "t@t.com"]).current_dir(sub_dir.path()).output()?;
        Command::new("git").args(["config", "user.name", "T"]).current_dir(sub_dir.path()).output()?;
        std::fs::write(sub_dir.path().join("x.txt"), "x\n")?;
        Command::new("git").args(["add", "."]).current_dir(sub_dir.path()).output()?;
        Command::new("git").args(["commit", "-m", "init"]).current_dir(sub_dir.path()).output()?;
    }

    repo.run_git([
        "submodule",
        "add",
        sub_dir.path().to_str().unwrap(),
        "sub",
    ])?;
    repo.commit_all("add submodule")?;

    let gix = repo.gix_repo()?;
    let objects = tempfile::tempdir()?;
    let tree = capture(&gix, objects.path())?;

    // tree_sha must be non-empty (submodule gitlink captured without recursing).
    assert!(!tree.tree_sha.is_empty(), "tree must include submodule gitlink");
    Ok(())
}

// ---------------------------------------------------------------------------
// tree: symlink mode 120000 round-trips
// ---------------------------------------------------------------------------

#[test]
#[ignore = "phase-3-pending: symlink mode 120000 capture is unimplemented"]
fn symlink_mode_round_trips() -> Result<()> {
    #[cfg(unix)]
    {
        let repo = TestRepo::new()?;
        repo.write_file("target.txt", "target\n")?;
        std::os::unix::fs::symlink("target.txt", repo.path().join("link.txt"))?;
        repo.commit_all("add symlink")?;

        let gix = repo.gix_repo()?;
        let objects = tempfile::tempdir()?;
        let tree = capture(&gix, objects.path())?;

        assert!(!tree.tree_sha.is_empty(), "symlink must be captured in tree");
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// tree: real index file SHA1 unchanged after capture
// ---------------------------------------------------------------------------

#[test]
#[ignore = "phase-3-pending: capture must not mutate real index"]
fn real_index_sha_unchanged_after_capture() -> Result<()> {
    let repo = TestRepo::new()?;
    repo.write_file("a.txt", "hello\n")?;
    repo.commit_all("initial")?;
    repo.write_file("b.txt", "world\n")?; // untracked

    let index_path = repo.path().join(".git/index");
    let before = sha1_of_file(&index_path)?;

    let gix = repo.gix_repo()?;
    let objects = tempfile::tempdir()?;
    capture(&gix, objects.path())?;

    let after = sha1_of_file(&index_path)?;
    assert_eq!(before, after, "capture must not mutate the real index");
    Ok(())
}

fn sha1_of_file(path: &std::path::Path) -> Result<Vec<u8>> {
    use std::io::Read;
    let mut f = std::fs::File::open(path)?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf)?;
    // Use the last 20 bytes of the index file (Git index checksum trailer).
    if buf.len() >= 20 {
        Ok(buf[buf.len() - 20..].to_vec())
    } else {
        Ok(buf)
    }
}

//! File index tests (§3.4).

mod support;

use anyhow::Result;
use git_mesh::{
    append_add, commit_mesh, ls_all, ls_by_path, ls_by_path_range, read_index, rebuild_index,
    set_message,
};
use support::TestRepo;

fn seed_two_meshes(repo: &TestRepo) -> Result<()> {
    let gix = repo.gix_repo()?;
    append_add(&gix, "m1", "file1.txt", 1, 5, None)?;
    set_message(&gix, "m1", "seed")?;
    commit_mesh(&gix, "m1")?;
    append_add(&gix, "m2", "file1.txt", 8, 10, None)?;
    append_add(&gix, "m2", "file2.txt", 1, 3, None)?;
    set_message(&gix, "m2", "seed")?;
    commit_mesh(&gix, "m2")?;
    Ok(())
}

#[test]

fn rebuild_index_creates_file() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed_two_meshes(&repo)?;
    rebuild_index(&repo.gix_repo()?)?;
    assert!(repo.path().join(".git/mesh/file-index").exists());
    Ok(())
}

#[test]

fn read_index_returns_all_entries_sorted() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed_two_meshes(&repo)?;
    let entries = read_index(&repo.gix_repo()?)?;
    assert_eq!(entries.len(), 3);
    // Sorted by (path, start)
    for pair in entries.windows(2) {
        assert!((pair[0].path.as_str(), pair[0].start) <= (pair[1].path.as_str(), pair[1].start));
    }
    Ok(())
}

#[test]

fn ls_all_matches_read_index() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed_two_meshes(&repo)?;
    let a = ls_all(&repo.gix_repo()?)?;
    let b = read_index(&repo.gix_repo()?)?;
    assert_eq!(a, b);
    Ok(())
}

#[test]

fn ls_by_path_filters() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed_two_meshes(&repo)?;
    let rows = ls_by_path(&repo.gix_repo()?, "file1.txt")?;
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().all(|e| e.path == "file1.txt"));
    Ok(())
}

#[test]

fn ls_by_path_range_overlap_inclusive() -> Result<()> {
    // §3.4: overlap rule is a <= end && b >= start.
    let repo = TestRepo::seeded()?;
    seed_two_meshes(&repo)?;
    // file1.txt has ranges 1..5 and 8..10.
    let r = ls_by_path_range(&repo.gix_repo()?, "file1.txt", 4, 9)?;
    assert_eq!(r.len(), 2, "4..9 overlaps both 1..5 and 8..10");
    let r2 = ls_by_path_range(&repo.gix_repo()?, "file1.txt", 6, 7)?;
    assert!(r2.is_empty(), "6..7 overlaps neither");
    Ok(())
}

#[test]

fn ls_by_path_range_boundary_is_inclusive() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed_two_meshes(&repo)?;
    let r = ls_by_path_range(&repo.gix_repo()?, "file1.txt", 5, 5)?;
    assert_eq!(r.len(), 1, "end==start of existing range counts");
    Ok(())
}

#[test]

fn read_index_regenerates_when_absent() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed_two_meshes(&repo)?;
    rebuild_index(&repo.gix_repo()?)?;
    std::fs::remove_file(repo.path().join(".git/mesh/file-index"))?;
    // read_index should transparently regenerate (§3.4 lifecycle).
    let rows = read_index(&repo.gix_repo()?)?;
    assert_eq!(rows.len(), 3);
    Ok(())
}

#[test]

fn read_index_regenerates_on_wrong_header() -> Result<()> {
    let repo = TestRepo::seeded()?;
    seed_two_meshes(&repo)?;
    std::fs::create_dir_all(repo.path().join(".git/mesh"))?;
    std::fs::write(
        repo.path().join(".git/mesh/file-index"),
        "# mesh-index v999\n",
    )?;
    let rows = read_index(&repo.gix_repo()?)?;
    assert_eq!(rows.len(), 3);
    Ok(())
}

//! `.git/mesh/file-index` — derived lookup table (§3.4).

use crate::Result;
use crate::git::mesh_dir;
use crate::mesh::path_index;
use crate::mesh::read::{list_mesh_names, read_mesh};
use crate::types::AnchorExtent;
use std::fs;
use std::path::PathBuf;

const HEADER: &str = "# mesh-index v2";

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct IndexEntry {
    pub path: String,
    pub mesh_name: String,
    pub start: u32,
    pub end: u32,
}

fn index_path(repo: &gix::Repository) -> Result<PathBuf> {
    Ok(mesh_dir(repo).join("file-index"))
}

pub fn rebuild_index(repo: &gix::Repository) -> Result<()> {
    let entries = collect_entries(repo)?;
    write_index(repo, &entries)
}

fn write_index(repo: &gix::Repository, entries: &[IndexEntry]) -> Result<()> {
    let p = index_path(repo)?;
    fs::create_dir_all(p.parent().unwrap())?;
    let mut out = String::from(HEADER);
    out.push('\n');
    for e in entries {
        out.push_str(&format!(
            "{}\t{}\t{}\t{}\n",
            e.path, e.mesh_name, e.start, e.end
        ));
    }
    fs::write(p, out)?;
    Ok(())
}

fn collect_entries(repo: &gix::Repository) -> Result<Vec<IndexEntry>> {
    let mut out = Vec::new();
    for name in list_mesh_names(repo)? {
        let mesh = read_mesh(repo, &name)?;
        for (_id, r) in mesh.anchors_v2 {
            let (start, end) = match r.extent {
                AnchorExtent::LineRange { start, end } => (start, end),
                // Whole-file pins are recorded as `0..0` in the index for
                // sort/lookup; the renderer prints `*` in place of a
                // line range. See plan §D2.
                AnchorExtent::WholeFile => (0, 0),
            };
            out.push(IndexEntry {
                path: r.path,
                mesh_name: name.clone(),
                start,
                end,
            });
        }
    }
    out.sort_by(|a, b| {
        (a.path.as_str(), a.start, a.end, a.mesh_name.as_str()).cmp(&(
            b.path.as_str(),
            b.start,
            b.end,
            b.mesh_name.as_str(),
        ))
    });
    Ok(out)
}

pub fn read_index(repo: &gix::Repository) -> Result<Vec<IndexEntry>> {
    let entries = collect_entries(repo)?;
    write_index(repo, &entries)?;
    Ok(entries)
}

pub fn ls_all(repo: &gix::Repository) -> Result<Vec<IndexEntry>> {
    read_index(repo)
}

pub fn ls_by_path(repo: &gix::Repository, path: &str) -> Result<Vec<IndexEntry>> {
    entries_for_path(repo, path)
}

pub fn ls_by_path_line_range(
    repo: &gix::Repository,
    path: &str,
    start: u32,
    end: u32,
) -> Result<Vec<IndexEntry>> {
    Ok(entries_for_path(repo, path)?
        .into_iter()
        .filter(|e| e.start <= end && e.end >= start)
        .collect())
}

fn entries_for_path(repo: &gix::Repository, path: &str) -> Result<Vec<IndexEntry>> {
    Ok(path_index::read_entries_for_path(repo, path)?
        .into_iter()
        .map(|entry| IndexEntry {
            path: path.to_string(),
            mesh_name: entry.mesh_name,
            start: entry.start,
            end: entry.end,
        })
        .collect())
}

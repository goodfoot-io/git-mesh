//! `.git/mesh/file-index` — derived lookup table (§3.4).

use crate::git::mesh_dir;
use crate::mesh::read::{list_mesh_names, read_mesh};
use crate::types::AnchorExtent;
use crate::{Error, Result};
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
    let p = index_path(repo)?;
    let regenerate = !p.exists() || {
        let text = fs::read_to_string(&p).unwrap_or_default();
        !text.starts_with(HEADER)
    };
    if regenerate {
        let entries = collect_entries(repo)?;
        write_index(repo, &entries)?;
        return Ok(entries);
    }
    let text = fs::read_to_string(&p)?;
    let mut entries = Vec::new();
    for (i, line) in text.lines().enumerate() {
        if i == 0 {
            continue;
        }
        if line.is_empty() {
            continue;
        }
        let fields: Vec<&str> = line.split('\t').collect();
        if fields.len() != 4 {
            return Err(Error::Parse(format!("malformed file-index line `{line}`")));
        }
        entries.push(IndexEntry {
            path: fields[0].into(),
            mesh_name: fields[1].into(),
            start: fields[2]
                .parse()
                .map_err(|_| Error::Parse("bad start".into()))?,
            end: fields[3]
                .parse()
                .map_err(|_| Error::Parse("bad end".into()))?,
        });
    }
    Ok(entries)
}

pub fn ls_all(repo: &gix::Repository) -> Result<Vec<IndexEntry>> {
    read_index(repo)
}

pub fn ls_by_path(repo: &gix::Repository, path: &str) -> Result<Vec<IndexEntry>> {
    Ok(read_index(repo)?
        .into_iter()
        .filter(|e| e.path == path)
        .collect())
}

pub fn ls_by_path_line_range(
    repo: &gix::Repository,
    path: &str,
    start: u32,
    end: u32,
) -> Result<Vec<IndexEntry>> {
    Ok(read_index(repo)?
        .into_iter()
        .filter(|e| e.path == path && e.start <= end && e.end >= start)
        .collect())
}

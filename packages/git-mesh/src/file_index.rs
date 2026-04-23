//! `.git/mesh/file-index` — derived lookup table (§3.4).

use crate::git::work_dir;
use crate::mesh::read::{list_mesh_names, read_mesh};
use crate::range::read_range;
use crate::{Error, Result};
use std::fs;
use std::path::PathBuf;

const HEADER: &str = "# mesh-index v1";

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct IndexEntry {
    pub path: String,
    pub mesh_name: String,
    pub range_id: String,
    pub start: u32,
    pub end: u32,
    pub anchor_short: String,
}

fn index_path(repo: &gix::Repository) -> Result<PathBuf> {
    let wd = work_dir(repo)?;
    Ok(wd.join(".git").join("mesh").join("file-index"))
}

fn short(sha: &str) -> String {
    sha[..sha.len().min(8)].to_string()
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
            "{}\t{}\t{}\t{}\t{}\t{}\n",
            e.path, e.mesh_name, e.range_id, e.start, e.end, e.anchor_short
        ));
    }
    fs::write(p, out)?;
    Ok(())
}

fn collect_entries(repo: &gix::Repository) -> Result<Vec<IndexEntry>> {
    let mut out = Vec::new();
    for name in list_mesh_names(repo)? {
        let mesh = read_mesh(repo, &name)?;
        for id in mesh.ranges {
            let r = read_range(repo, &id)?;
            out.push(IndexEntry {
                path: r.path,
                mesh_name: name.clone(),
                range_id: id,
                start: r.start,
                end: r.end,
                anchor_short: short(&r.anchor_sha),
            });
        }
    }
    out.sort_by(|a, b| {
        (a.path.as_str(), a.start, a.end, a.mesh_name.as_str(), a.range_id.as_str())
            .cmp(&(b.path.as_str(), b.start, b.end, b.mesh_name.as_str(), b.range_id.as_str()))
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
        if fields.len() != 6 {
            return Err(Error::Parse(format!("malformed file-index line `{line}`")));
        }
        entries.push(IndexEntry {
            path: fields[0].into(),
            mesh_name: fields[1].into(),
            range_id: fields[2].into(),
            start: fields[3].parse().map_err(|_| Error::Parse("bad start".into()))?,
            end: fields[4].parse().map_err(|_| Error::Parse("bad end".into()))?,
            anchor_short: fields[5].into(),
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

pub fn ls_by_path_range(
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

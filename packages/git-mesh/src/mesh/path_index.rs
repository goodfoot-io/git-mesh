use crate::git::{self, RefUpdate};
use crate::types::{Anchor, AnchorExtent};
use crate::{Error, Result};
use sha2::{Digest, Sha256};

const REF_PREFIX: &str = "refs/meshes-index/v1/path";
const HEADER: &str = "# git-mesh path-index v1";

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PathIndexEntry {
    pub mesh_name: String,
    pub start: u32,
    pub end: u32,
}

pub(crate) fn ref_name_for_path(path: &str) -> String {
    let hash = Sha256::digest(path.as_bytes());
    let hex = format!("{hash:x}");
    format!("{REF_PREFIX}/{}/{}", &hex[..2], hex)
}

pub(crate) fn read_entries_for_path(
    repo: &gix::Repository,
    path: &str,
) -> Result<Vec<PathIndexEntry>> {
    let Some(blob_oid) = git::resolve_ref_oid_optional_repo(repo, &ref_name_for_path(path))? else {
        return Ok(Vec::new());
    };
    let text = git::read_git_text(repo, &blob_oid)?;
    parse_index_blob(&text)
}

pub(crate) fn matching_mesh_names(
    repo: &gix::Repository,
    path: &str,
    range: Option<(u32, u32)>,
) -> Result<Vec<String>> {
    let mut names = Vec::new();
    let mut last = None::<String>;
    for entry in read_entries_for_path(repo, path)? {
        let matches = match range {
            Some((start, end)) => {
                (entry.start == 0 && entry.end == 0) || (entry.start <= end && entry.end >= start)
            }
            None => true,
        };
        if matches && last.as_deref() != Some(entry.mesh_name.as_str()) {
            last = Some(entry.mesh_name.clone());
            names.push(entry.mesh_name);
        }
    }
    Ok(names)
}

pub(crate) fn ref_updates_for_mesh(
    repo: &gix::Repository,
    mesh_name: &str,
    old_anchors: &[(String, Anchor)],
    new_anchors: &[(String, Anchor)],
) -> Result<Vec<RefUpdate>> {
    let mut paths: Vec<String> = old_anchors
        .iter()
        .chain(new_anchors.iter())
        .map(|(_, anchor)| anchor.path.clone())
        .collect();
    paths.sort();
    paths.dedup();

    let mut updates = Vec::new();
    for path in paths {
        let ref_name = ref_name_for_path(&path);
        let old_oid = git::resolve_ref_oid_optional_repo(repo, &ref_name)?;
        let mut entries = match old_oid.as_deref() {
            Some(oid) => parse_index_blob(&git::read_git_text(repo, oid)?)?,
            None => Vec::new(),
        };
        entries.retain(|entry| entry.mesh_name != mesh_name);
        for (_id, anchor) in new_anchors.iter().filter(|(_, anchor)| anchor.path == path) {
            let (start, end) = extent_index_range(anchor.extent);
            entries.push(PathIndexEntry {
                mesh_name: mesh_name.to_string(),
                start,
                end,
            });
        }
        entries.sort_by(|a, b| {
            (a.mesh_name.as_str(), a.start, a.end).cmp(&(b.mesh_name.as_str(), b.start, b.end))
        });

        match (old_oid, entries.is_empty()) {
            (Some(expected_old_oid), true) => updates.push(RefUpdate::Delete {
                name: ref_name,
                expected_old_oid,
            }),
            (old_oid, false) => {
                let blob_oid =
                    git::write_blob_bytes(repo, serialize_index_blob(&entries).as_bytes())?;
                match old_oid {
                    Some(expected_old_oid) => updates.push(RefUpdate::Update {
                        name: ref_name,
                        new_oid: blob_oid,
                        expected_old_oid,
                    }),
                    None => updates.push(RefUpdate::Create {
                        name: ref_name,
                        new_oid: blob_oid,
                    }),
                }
            }
            (None, true) => {}
        }
    }
    Ok(updates)
}

pub(crate) fn ref_updates_for_rename(
    repo: &gix::Repository,
    old_name: &str,
    new_name: &str,
    anchors: &[(String, Anchor)],
) -> Result<Vec<RefUpdate>> {
    let mut paths: Vec<String> = anchors
        .iter()
        .map(|(_, anchor)| anchor.path.clone())
        .collect();
    paths.sort();
    paths.dedup();

    let mut updates = Vec::new();
    for path in paths {
        let ref_name = ref_name_for_path(&path);
        let old_oid = git::resolve_ref_oid_optional_repo(repo, &ref_name)?;
        let mut entries = match old_oid.as_deref() {
            Some(oid) => parse_index_blob(&git::read_git_text(repo, oid)?)?,
            None => Vec::new(),
        };
        entries.retain(|entry| entry.mesh_name != old_name && entry.mesh_name != new_name);
        for (_id, anchor) in anchors.iter().filter(|(_, anchor)| anchor.path == path) {
            let (start, end) = extent_index_range(anchor.extent);
            entries.push(PathIndexEntry {
                mesh_name: new_name.to_string(),
                start,
                end,
            });
        }
        entries.sort_by(|a, b| {
            (a.mesh_name.as_str(), a.start, a.end).cmp(&(b.mesh_name.as_str(), b.start, b.end))
        });
        let blob_oid = git::write_blob_bytes(repo, serialize_index_blob(&entries).as_bytes())?;
        match old_oid {
            Some(expected_old_oid) => updates.push(RefUpdate::Update {
                name: ref_name,
                new_oid: blob_oid,
                expected_old_oid,
            }),
            None => updates.push(RefUpdate::Create {
                name: ref_name,
                new_oid: blob_oid,
            }),
        }
    }
    Ok(updates)
}

fn extent_index_range(extent: AnchorExtent) -> (u32, u32) {
    match extent {
        AnchorExtent::LineRange { start, end } => (start, end),
        AnchorExtent::WholeFile => (0, 0),
    }
}

fn parse_index_blob(text: &str) -> Result<Vec<PathIndexEntry>> {
    let mut lines = text.lines();
    match lines.next() {
        Some(HEADER) => {}
        _ => return Err(Error::Parse("malformed path-index header".into())),
    }
    let mut entries = Vec::new();
    for line in lines {
        if line.is_empty() {
            continue;
        }
        let fields: Vec<&str> = line.split('\t').collect();
        if fields.len() != 3 {
            return Err(Error::Parse(format!("malformed path-index line `{line}`")));
        }
        entries.push(PathIndexEntry {
            mesh_name: fields[0].to_string(),
            start: fields[1]
                .parse()
                .map_err(|_| Error::Parse(format!("bad path-index start `{}`", fields[1])))?,
            end: fields[2]
                .parse()
                .map_err(|_| Error::Parse(format!("bad path-index end `{}`", fields[2])))?,
        });
    }
    Ok(entries)
}

fn serialize_index_blob(entries: &[PathIndexEntry]) -> String {
    let mut out = String::from(HEADER);
    out.push('\n');
    for entry in entries {
        out.push_str(&entry.mesh_name);
        out.push('\t');
        out.push_str(&entry.start.to_string());
        out.push('\t');
        out.push_str(&entry.end.to_string());
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_ref_is_deterministic_and_sharded() {
        let ref_name = ref_name_for_path("src/module_001.rs");
        assert!(ref_name.starts_with("refs/meshes-index/v1/path/"));
        assert_eq!(ref_name.rsplit('/').next().unwrap().len(), 64);
    }

    #[test]
    fn index_blob_round_trips() {
        let entries = vec![
            PathIndexEntry {
                mesh_name: "alpha".to_string(),
                start: 0,
                end: 0,
            },
            PathIndexEntry {
                mesh_name: "beta".to_string(),
                start: 10,
                end: 20,
            },
        ];
        assert_eq!(
            parse_index_blob(&serialize_index_blob(&entries)).unwrap(),
            entries
        );
    }
}

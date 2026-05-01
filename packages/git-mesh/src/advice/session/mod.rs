//! File-backed session store for `git mesh advice`.

pub mod state;
pub mod store;

use std::fs::{DirBuilder, File, OpenOptions};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result};

/// Apply a Unix mode to a `DirBuilder`. No-op on non-Unix targets.
fn dir_with_mode(b: &mut DirBuilder, mode: u32) -> &mut DirBuilder {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        b.mode(mode);
    }
    #[cfg(not(unix))]
    {
        let _ = mode;
    }
    b
}

/// Apply a Unix mode to `OpenOptions`. No-op on non-Unix targets.
fn open_with_mode(opts: &mut OpenOptions, mode: u32) -> &mut OpenOptions {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(mode);
    }
    #[cfg(not(unix))]
    {
        let _ = mode;
    }
    opts
}

use state::{ReadRecord, SessionFlags, TouchInterval};
use store::{LockGuard, LockTimeout};

pub const SCHEMA_VERSION: u32 = 2;

const JSONL_FILES: &[&str] = &[
    "reads.jsonl",
    "touches.jsonl",
    "advice-seen.jsonl",
    "docs-seen.jsonl",
    "meshes-seen.jsonl",
    "mesh-candidates.jsonl",
    "pending_touches.jsonl",
];

const SNAPSHOTS_SUBDIR: &str = "snapshots";

/// Facade over the per-session directory.
pub struct SessionStore {
    dir: PathBuf,
    lock: LockGuard,
}

impl SessionStore {
    /// Open (and create if absent) the session directory for `session_id`.
    pub fn open(repo_root: &Path, git_dir: &Path, session_id: &str) -> Result<Self> {
        let dir = store::session_dir(repo_root, git_dir, session_id);
        if let Some(parent) = dir.parent() {
            dir_with_mode(DirBuilder::new().recursive(true), 0o700)
                .create(parent)
                .with_context(|| format!("mkdir `{}`", parent.display()))?;
        }
        dir_with_mode(DirBuilder::new().recursive(true), 0o700)
            .create(&dir)
            .with_context(|| format!("mkdir `{}`", dir.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&dir)?.permissions();
            if perms.mode() & 0o777 != 0o700 {
                perms.set_mode(0o700);
                std::fs::set_permissions(&dir, perms).ok();
            }
        }
        let lock = store::acquire_lock(&dir, LockTimeout::Blocking)?;
        Ok(Self { dir, lock })
    }

    /// Touch the JSONL files so the session directory is well-formed for
    /// the first `mark` that lands.
    pub fn ensure_initialized(&self) -> Result<()> {
        for name in JSONL_FILES {
            let path = self.dir.join(name);
            if !path.exists() {
                open_with_mode(
                    OpenOptions::new().create(true).write(true).truncate(false),
                    0o600,
                )
                .open(&path)
                .with_context(|| format!("touch `{}`", path.display()))?;
            }
        }
        Ok(())
    }

    /// Path to the snapshots subdirectory, creating it if absent.
    pub fn snapshots_dir(&self) -> Result<PathBuf> {
        let p = self.dir.join(SNAPSHOTS_SUBDIR);
        dir_with_mode(DirBuilder::new().recursive(true), 0o700)
            .create(&p)
            .with_context(|| format!("mkdir `{}`", p.display()))?;
        Ok(p)
    }

    pub fn snapshot_index_path(&self, id: &str) -> PathBuf {
        self.dir.join(SNAPSHOTS_SUBDIR).join(format!("{id}.index"))
    }

    pub fn snapshot_untracked_path(&self, id: &str) -> PathBuf {
        self.dir
            .join(SNAPSHOTS_SUBDIR)
            .join(format!("{id}.untracked"))
    }

    pub fn snapshot_exists(&self, id: &str) -> bool {
        self.snapshot_index_path(id).exists()
    }

    pub fn discard_snapshot(&self, id: &str) {
        let _ = std::fs::remove_file(self.snapshot_index_path(id));
        let _ = std::fs::remove_file(self.snapshot_untracked_path(id));
    }

    /// Drop snapshot artefacts older than `max_age`. Called from
    /// `SessionStart` so a `mark` without its `flush` doesn't leak forever.
    pub fn sweep_orphan_snapshots(&self, max_age: Duration) -> Result<()> {
        let dir = self.dir.join(SNAPSHOTS_SUBDIR);
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e).with_context(|| format!("read_dir `{}`", dir.display())),
        };
        let now = SystemTime::now();
        for entry in entries {
            let entry = entry?;
            let meta = match entry.metadata() {
                Ok(m) => m,
                Err(_) => continue,
            };
            let modified = meta.modified().unwrap_or(now);
            let age = now.duration_since(modified).unwrap_or_default();
            if age > max_age {
                let _ = std::fs::remove_file(entry.path());
            }
        }
        Ok(())
    }

    /// Remove every entry under `snapshots/` unconditionally.
    pub fn sweep_all_snapshots(&self) {
        let dir = self.dir.join(SNAPSHOTS_SUBDIR);
        let Ok(entries) = std::fs::read_dir(&dir) else {
            return;
        };
        for entry in entries.flatten() {
            let _ = std::fs::remove_file(entry.path());
        }
    }

    /// Read `flags.state`. Returns `SessionFlags::default()` when absent.
    pub fn read_flags(&self) -> Result<SessionFlags> {
        let path = self.dir.join("flags.state");
        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(SessionFlags::default());
            }
            Err(e) => return Err(e).with_context(|| format!("read `{}`", path.display())),
        };
        let flags: SessionFlags = serde_json::from_slice(&bytes)
            .map_err(|e| anyhow::anyhow!("parse `{}`: {e}", path.display()))?;
        Ok(flags)
    }

    pub fn write_flags(&self, flags: &SessionFlags) -> Result<()> {
        let bytes = serde_json::to_vec(flags).with_context(|| "serialize SessionFlags")?;
        store::atomic_write(&self.dir.join("flags.state"), &bytes)
    }

    pub fn append_read(&self, record: &ReadRecord, _timeout: LockTimeout) -> Result<()> {
        let line = serde_json::to_string(record).with_context(|| "serialize ReadRecord")?;
        store::append_jsonl_line(&self.dir.join("reads.jsonl"), &self.lock, &line)
    }

    pub fn all_reads(&self) -> Result<Vec<ReadRecord>> {
        read_jsonl_lines(&self.dir.join("reads.jsonl"))
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn append_touch(&self, t: &TouchInterval) -> Result<()> {
        let line = serde_json::to_string(t).with_context(|| "serialize TouchInterval")?;
        store::append_jsonl_line(&self.dir.join("touches.jsonl"), &self.lock, &line)
    }

    pub fn all_touch_intervals(&self) -> Result<Vec<TouchInterval>> {
        read_jsonl_lines(&self.dir.join("touches.jsonl"))
    }

    pub fn append_advice_seen(&self, fingerprints: &[String]) -> Result<()> {
        for fp in fingerprints {
            let line = serde_json::to_string(fp).with_context(|| "serialize fingerprint")?;
            store::append_jsonl_line(&self.dir.join("advice-seen.jsonl"), &self.lock, &line)?;
        }
        Ok(())
    }

    pub fn advice_seen_set(&self) -> Result<std::collections::HashSet<String>> {
        load_string_set(&self.dir.join("advice-seen.jsonl"))
    }

    pub fn append_docs_seen(&self, topics: &[String]) -> Result<()> {
        for t in topics {
            let line = serde_json::to_string(t).with_context(|| "serialize topic")?;
            store::append_jsonl_line(&self.dir.join("docs-seen.jsonl"), &self.lock, &line)?;
        }
        Ok(())
    }

    pub fn append_meshes_seen(&self, names: &[String]) -> Result<()> {
        for n in names {
            let line = serde_json::to_string(n).with_context(|| "serialize mesh name")?;
            store::append_jsonl_line(&self.dir.join("meshes-seen.jsonl"), &self.lock, &line)?;
        }
        Ok(())
    }

    pub fn meshes_seen_set(&self) -> Result<std::collections::HashSet<String>> {
        load_string_set(&self.dir.join("meshes-seen.jsonl"))
    }

    pub fn append_mesh_candidates(&self, names: &[String]) -> Result<()> {
        for n in names {
            let line = serde_json::to_string(n).with_context(|| "serialize mesh candidate name")?;
            store::append_jsonl_line(&self.dir.join("mesh-candidates.jsonl"), &self.lock, &line)?;
        }
        Ok(())
    }

    pub fn mesh_candidates_set(&self) -> Result<std::collections::HashSet<String>> {
        load_string_set(&self.dir.join("mesh-candidates.jsonl"))
    }

    pub fn docs_seen_set(&self) -> Result<std::collections::HashSet<String>> {
        load_string_set(&self.dir.join("docs-seen.jsonl"))
    }

    /// Append a single [`TouchInterval`] row to `pending_touches.jsonl`.
    pub fn append_pending_touch(&self, t: &TouchInterval) -> Result<()> {
        let line = serde_json::to_string(t).with_context(|| "serialize TouchInterval (pending)")?;
        store::append_jsonl_line(&self.dir.join("pending_touches.jsonl"), &self.lock, &line)
    }

    /// Collect the distinct `path` values from `reads.jsonl`.
    pub fn reads_seen_paths(&self) -> Result<std::collections::HashSet<String>> {
        let reads: Vec<state::ReadRecord> = read_jsonl_lines(&self.dir.join("reads.jsonl"))?;
        Ok(reads.into_iter().map(|r| r.path).collect())
    }

    /// Read `pending_touches.jsonl`, extract rows matching `path`, atomically
    /// rewrite the file with the remaining rows, and return the drained rows.
    /// The session lock held by `open()` makes this read-modify-write race-free.
    pub fn drain_pending_touches_for_path(&self, path: &str) -> Result<Vec<TouchInterval>> {
        let pending_path = self.dir.join("pending_touches.jsonl");
        let all: Vec<TouchInterval> = read_jsonl_lines(&pending_path)?;
        let mut drained = Vec::new();
        let mut survivors = Vec::new();
        for t in all {
            if t.path == path {
                drained.push(t);
            } else {
                survivors.push(t);
            }
        }
        if drained.is_empty() {
            return Ok(drained);
        }
        // Atomically rewrite the file with the survivors.
        let mut bytes = Vec::new();
        for t in &survivors {
            let line = serde_json::to_string(t)
                .with_context(|| "serialize TouchInterval (pending rewrite)")?;
            bytes.extend_from_slice(line.as_bytes());
            bytes.push(b'\n');
        }
        store::atomic_write(&pending_path, &bytes)?;
        Ok(drained)
    }
}

fn read_jsonl_lines<T: serde::de::DeserializeOwned>(path: &Path) -> Result<Vec<T>> {
    let f = match File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e).with_context(|| format!("open `{}`", path.display())),
    };
    let mut out = Vec::new();
    for (idx, line) in BufReader::new(f).lines().enumerate() {
        let line = line.with_context(|| format!("read `{}`", path.display()))?;
        if line.is_empty() {
            continue;
        }
        let v: T = serde_json::from_str(&line)
            .map_err(|e| anyhow::anyhow!("parse `{}` line {}: {e}", path.display(), idx + 1))?;
        out.push(v);
    }
    Ok(out)
}

fn load_string_set(path: &Path) -> Result<std::collections::HashSet<String>> {
    let f = match File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(std::collections::HashSet::new());
        }
        Err(e) => return Err(e).with_context(|| format!("open `{}`", path.display())),
    };
    let mut out = std::collections::HashSet::new();
    for (idx, line) in BufReader::new(f).lines().enumerate() {
        let line = line.with_context(|| format!("read `{}`", path.display()))?;
        if line.is_empty() {
            continue;
        }
        let s: String = serde_json::from_str(&line)
            .map_err(|e| anyhow::anyhow!("parse `{}` line {}: {e}", path.display(), idx + 1))?;
        out.insert(s);
    }
    Ok(out)
}

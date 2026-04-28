//! File-backed session store for `git mesh advice`.

pub mod state;
pub mod store;

use std::fs::{DirBuilder, File, OpenOptions};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

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

use state::{BaselineState, LastFlushState, ReadRecord, SessionFlags, TouchInterval};
use store::{LockGuard, LockTimeout};

pub const SCHEMA_VERSION: u32 = 1;

/// Names of the JSONL files reset on snapshot.
const JSONL_FILES: &[&str] = &[
    "reads.jsonl",
    "touches.jsonl",
    "advice-seen.jsonl",
    "docs-seen.jsonl",
    "meshes-seen.jsonl",
    "mesh-candidates.jsonl",
];

/// Facade over the per-session directory.
pub struct SessionStore {
    dir: PathBuf,
    lock: LockGuard,
}

impl SessionStore {
    /// Open (and create if absent) the session directory for `session_id`.
    pub fn open(repo_root: &Path, git_dir: &Path, session_id: &str) -> Result<Self> {
        let dir = store::session_dir(repo_root, git_dir, session_id);
        // Create parent (repo_key dir) and the session dir, both 0700.
        if let Some(parent) = dir.parent() {
            dir_with_mode(DirBuilder::new().recursive(true), 0o700)
                .create(parent)
                .with_context(|| format!("mkdir `{}`", parent.display()))?;
        }
        dir_with_mode(DirBuilder::new().recursive(true), 0o700)
            .create(&dir)
            .with_context(|| format!("mkdir `{}`", dir.display()))?;
        // Ensure the existing directory is 0700 (recursive=true skips existing).
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

    /// Reset the session: truncate all JSONL files, remove any prior
    /// `*.objects/` directories, and remove `flags.state` so that
    /// per-session print gates are cleared for the new snapshot.
    pub fn reset(&mut self) -> Result<()> {
        // Truncate JSONL files.
        for name in JSONL_FILES {
            let path = self.dir.join(name);
            // Truncate (or create empty).
            open_with_mode(
                OpenOptions::new().create(true).write(true).truncate(true),
                0o600,
            )
            .open(&path)
            .with_context(|| format!("truncate `{}`", path.display()))?;
        }
        // Remove flags.state so print gates reset for the new session.
        let flags_path = self.dir.join("flags.state");
        if flags_path.exists() {
            std::fs::remove_file(&flags_path)
                .with_context(|| format!("remove `{}`", flags_path.display()))?;
        }
        // Remove existing *.objects directories AND any leftover
        // current.objects-<uuid>/ scratch dirs from sessions that crashed
        // mid-render before the rename to last-flush.objects/ landed
        // (defense in depth for finding 6 — render now also cleans up
        // these on its own error paths via an RAII guard).
        for entry in std::fs::read_dir(&self.dir)
            .with_context(|| format!("read_dir `{}`", self.dir.display()))?
        {
            let entry = entry?;
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            let is_objects_dir =
                name_str.ends_with(".objects") || name_str.starts_with("current.objects-");
            if is_objects_dir && entry.file_type()?.is_dir() {
                std::fs::remove_dir_all(entry.path())
                    .with_context(|| format!("remove_dir_all `{}`", entry.path().display()))?;
            }
        }
        Ok(())
    }

    /// Read `flags.state`. Returns `SessionFlags::default()` when the file is
    /// absent (i.e. first call after a fresh `snapshot`).
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

    /// Write `flags.state` atomically under the held session lock.
    pub fn write_flags(&self, flags: &SessionFlags) -> Result<()> {
        let bytes = serde_json::to_vec(flags).with_context(|| "serialize SessionFlags")?;
        store::atomic_write(&self.dir.join("flags.state"), &bytes)
    }

    /// Read `baseline.state`. Returns an error if the file is absent or invalid.
    pub fn read_baseline(&self) -> Result<BaselineState> {
        read_state(&self.dir.join("baseline.state"))
    }

    /// Write `baseline.state` atomically.
    pub fn write_baseline(&self, st: &BaselineState) -> Result<()> {
        write_state(&self.dir.join("baseline.state"), st)
    }

    /// Read `last-flush.state`. Returns an error if the file is absent or invalid.
    pub fn read_last_flush(&self) -> Result<LastFlushState> {
        read_state(&self.dir.join("last-flush.state"))
    }

    /// Write `last-flush.state` atomically.
    pub fn write_last_flush(&self, st: &LastFlushState) -> Result<()> {
        write_state(&self.dir.join("last-flush.state"), st)
    }

    /// Append a `ReadRecord` to `reads.jsonl` under the advisory lock.
    pub fn append_read(&self, record: &ReadRecord, _timeout: LockTimeout) -> Result<()> {
        let line = serde_json::to_string(record).with_context(|| "serialize ReadRecord")?;
        store::append_jsonl_line(&self.dir.join("reads.jsonl"), &self.lock, &line)
    }

    /// Return all `ReadRecord` entries appended after byte offset `cursor`.
    ///
    /// Torn-tail recovery (finding 5): if the FINAL line of `reads.jsonl`
    /// fails to parse, we assume a `read` invocation was interrupted
    /// mid-`write_all` (SIGKILL, OOM, …) and skip that line with a stderr
    /// warning. Earlier parse failures remain hard errors — those indicate
    /// real corruption, not an interrupted append.
    pub fn reads_since_cursor(&self, cursor: u64) -> Result<Vec<ReadRecord>> {
        let path = self.dir.join("reads.jsonl");
        let f = match File::open(&path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e).with_context(|| format!("open `{}`", path.display())),
        };
        use std::io::Seek;
        let mut reader = BufReader::new(f);
        reader
            .seek(std::io::SeekFrom::Start(cursor))
            .with_context(|| format!("seek `{}`", path.display()))?;
        // Collect (line_no, line) pairs first so we can know which is final.
        let mut lines: Vec<(u32, String)> = Vec::new();
        for (idx, line) in reader.lines().enumerate() {
            let line_no = (idx as u32) + 1;
            let line = line.with_context(|| format!("read `{}`", path.display()))?;
            lines.push((line_no, line));
        }
        let last_idx = lines.len().saturating_sub(1);
        let mut out = Vec::new();
        for (i, (line_no, line)) in lines.iter().enumerate() {
            if line.is_empty() {
                continue;
            }
            match serde_json::from_str::<ReadRecord>(line) {
                Ok(rec) => out.push(rec),
                Err(e) if i == last_idx => {
                    eprintln!(
                        "git mesh advice: reads.jsonl: torn final line at line {line_no} (offset >= {cursor}), skipping: {e}"
                    );
                }
                Err(e) => {
                    return Err(anyhow::anyhow!(
                        "parse `reads.jsonl` at `{}` line {line_no}: {e}",
                        path.display()
                    ));
                }
            }
        }
        Ok(out)
    }

    /// Return the path to the `baseline.objects/` directory.
    pub fn baseline_objects_dir(&self) -> PathBuf {
        self.dir.join("baseline.objects")
    }

    /// Return the path to the `last-flush.objects/` directory.
    pub fn last_flush_objects_dir(&self) -> PathBuf {
        self.dir.join("last-flush.objects")
    }

    /// Return the session directory.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Append a touch interval to `touches.jsonl` under the held lock.
    pub fn append_touch(&self, t: &TouchInterval) -> Result<()> {
        let line = serde_json::to_string(t).with_context(|| "serialize TouchInterval")?;
        store::append_jsonl_line(&self.dir.join("touches.jsonl"), &self.lock, &line)
    }

    /// Return all touch intervals previously appended to `touches.jsonl`.
    pub fn all_touch_intervals(&self) -> Result<Vec<TouchInterval>> {
        read_jsonl_lines(&self.dir.join("touches.jsonl"))
    }

    /// Append fingerprints to `advice-seen.jsonl` (one per line, JSON string).
    pub fn append_advice_seen(&self, fingerprints: &[String]) -> Result<()> {
        for fp in fingerprints {
            let line = serde_json::to_string(fp).with_context(|| "serialize fingerprint")?;
            store::append_jsonl_line(&self.dir.join("advice-seen.jsonl"), &self.lock, &line)?;
        }
        Ok(())
    }

    /// Load all fingerprints previously appended to `advice-seen.jsonl`.
    pub fn advice_seen_set(&self) -> Result<std::collections::HashSet<String>> {
        let path = self.dir.join("advice-seen.jsonl");
        let f = match File::open(&path) {
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
            let s: String = serde_json::from_str(&line).map_err(|e| {
                anyhow::anyhow!(
                    "parse `advice-seen.jsonl` at `{}` line {}: {e}",
                    path.display(),
                    idx + 1
                )
            })?;
            out.insert(s);
        }
        Ok(out)
    }

    /// Append topics to `docs-seen.jsonl` (one per line, JSON string).
    pub fn append_docs_seen(&self, topics: &[String]) -> Result<()> {
        for t in topics {
            let line = serde_json::to_string(t).with_context(|| "serialize topic")?;
            store::append_jsonl_line(&self.dir.join("docs-seen.jsonl"), &self.lock, &line)?;
        }
        Ok(())
    }

    /// Append mesh names to `meshes-seen.jsonl` (one per line, JSON string).
    /// Used to surface each mesh at most once per advice session.
    pub fn append_meshes_seen(&self, names: &[String]) -> Result<()> {
        for n in names {
            let line = serde_json::to_string(n).with_context(|| "serialize mesh name")?;
            store::append_jsonl_line(&self.dir.join("meshes-seen.jsonl"), &self.lock, &line)?;
        }
        Ok(())
    }

    /// Load all mesh names previously appended to `meshes-seen.jsonl`.
    pub fn meshes_seen_set(&self) -> Result<std::collections::HashSet<String>> {
        let path = self.dir.join("meshes-seen.jsonl");
        let f = match File::open(&path) {
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
            let s: String = serde_json::from_str(&line).map_err(|e| {
                anyhow::anyhow!(
                    "parse `meshes-seen.jsonl` at `{}` line {}: {e}",
                    path.display(),
                    idx + 1
                )
            })?;
            out.insert(s);
        }
        Ok(out)
    }

    /// Append mesh names to `mesh-candidates.jsonl` (one per line, JSON string).
    /// Tracks meshes that may need modification based on activity this session.
    pub fn append_mesh_candidates(&self, names: &[String]) -> Result<()> {
        for n in names {
            let line = serde_json::to_string(n).with_context(|| "serialize mesh candidate name")?;
            store::append_jsonl_line(&self.dir.join("mesh-candidates.jsonl"), &self.lock, &line)?;
        }
        Ok(())
    }

    /// Load all mesh names previously appended to `mesh-candidates.jsonl`.
    pub fn mesh_candidates_set(&self) -> Result<std::collections::HashSet<String>> {
        let path = self.dir.join("mesh-candidates.jsonl");
        let f = match std::fs::File::open(&path) {
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
            let s: String = serde_json::from_str(&line).map_err(|e| {
                anyhow::anyhow!(
                    "parse `mesh-candidates.jsonl` at `{}` line {}: {e}",
                    path.display(),
                    idx + 1
                )
            })?;
            out.insert(s);
        }
        Ok(out)
    }

    /// Load all topics previously appended to `docs-seen.jsonl`.
    pub fn docs_seen_set(&self) -> Result<std::collections::HashSet<String>> {
        let path = self.dir.join("docs-seen.jsonl");
        let f = match File::open(&path) {
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
            let s: String = serde_json::from_str(&line).map_err(|e| {
                anyhow::anyhow!(
                    "parse `docs-seen.jsonl` at `{}` line {}: {e}",
                    path.display(),
                    idx + 1
                )
            })?;
            out.insert(s);
        }
        Ok(out)
    }

    /// Current byte length of `reads.jsonl`. Absent file = 0.
    pub fn reads_byte_len(&self) -> Result<u64> {
        let path = self.dir.join("reads.jsonl");
        match std::fs::metadata(&path) {
            Ok(m) => Ok(m.len()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(0),
            Err(e) => Err(e).with_context(|| format!("metadata `{}`", path.display())),
        }
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

fn read_state(path: &Path) -> Result<BaselineState> {
    let bytes = std::fs::read(path).with_context(|| format!("read `{}`", path.display()))?;
    let st: BaselineState = serde_json::from_slice(&bytes)
        .map_err(|e| anyhow::anyhow!("parse state file `{}`: {e}", path.display()))?;
    if st.schema_version != SCHEMA_VERSION {
        bail!(
            "state file `{}` has unknown schema_version {} (expected {})",
            path.display(),
            st.schema_version,
            SCHEMA_VERSION
        );
    }
    Ok(st)
}

fn write_state(path: &Path, st: &BaselineState) -> Result<()> {
    let bytes = serde_json::to_vec(st)
        .with_context(|| format!("serialize state for `{}`", path.display()))?;
    store::atomic_write(path, &bytes)
}

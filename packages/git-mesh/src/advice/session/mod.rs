//! File-backed session store for `git mesh advice`.

pub mod state;
pub mod store;

use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use state::{BaselineState, LastFlushState, ReadRecord};
use store::{LockGuard, LockTimeout};

pub const SCHEMA_VERSION: u32 = 1;

/// Names of the four JSONL files reset on snapshot.
const JSONL_FILES: &[&str] = &[
    "reads.jsonl",
    "touches.jsonl",
    "advice-seen.jsonl",
    "docs-seen.jsonl",
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
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(parent)
                .with_context(|| format!("mkdir `{}`", parent.display()))?;
        }
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
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

    /// Reset the session: truncate all four JSONL files and remove any prior
    /// `*.objects/` directories. The caller writes new state files separately.
    pub fn reset(&mut self) -> Result<()> {
        // Truncate JSONL files.
        for name in JSONL_FILES {
            let path = self.dir.join(name);
            // Truncate (or create empty).
            OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .mode(0o600)
                .open(&path)
                .with_context(|| format!("truncate `{}`", path.display()))?;
        }
        // Remove existing *.objects directories.
        for entry in std::fs::read_dir(&self.dir)
            .with_context(|| format!("read_dir `{}`", self.dir.display()))?
        {
            let entry = entry?;
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            if name_str.ends_with(".objects") && entry.file_type()?.is_dir() {
                std::fs::remove_dir_all(entry.path())
                    .with_context(|| format!("remove_dir_all `{}`", entry.path().display()))?;
            }
        }
        Ok(())
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
        let line = serde_json::to_string(record)
            .with_context(|| "serialize ReadRecord")?;
        store::append_jsonl_line(&self.dir.join("reads.jsonl"), &self.lock, &line)
    }

    /// Return all `ReadRecord` entries appended after byte offset `cursor`.
    pub fn reads_since_cursor(&self, cursor: u64) -> Result<Vec<ReadRecord>> {
        let path = self.dir.join("reads.jsonl");
        let f = match File::open(&path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e).with_context(|| format!("open `{}`", path.display())),
        };
        use std::io::Seek;
        let mut reader = BufReader::new(f);
        reader.seek(std::io::SeekFrom::Start(cursor))
            .with_context(|| format!("seek `{}`", path.display()))?;
        let mut out = Vec::new();
        for (idx, line) in reader.lines().enumerate() {
            let line_no = (idx as u32) + 1;
            let line = line.with_context(|| format!("read `{}`", path.display()))?;
            if line.is_empty() {
                continue;
            }
            let rec: ReadRecord = serde_json::from_str(&line).map_err(|e| {
                anyhow::anyhow!(
                    "parse `reads.jsonl` at `{}` line {line_no}: {e}",
                    path.display()
                )
            })?;
            out.push(rec);
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
}

fn read_state(path: &Path) -> Result<BaselineState> {
    let bytes = std::fs::read(path)
        .with_context(|| format!("read `{}`", path.display()))?;
    let st: BaselineState = serde_json::from_slice(&bytes).map_err(|e| {
        anyhow::anyhow!("parse state file `{}`: {e}", path.display())
    })?;
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

//! File-backed session store for `git mesh advice`.

pub mod state;
pub mod store;

use std::path::PathBuf;

use anyhow::Result;

use state::{BaselineState, LastFlushState, ReadRecord};
use store::LockTimeout;

/// Facade over the per-session directory.
#[allow(dead_code)]
pub struct SessionStore {
    dir: PathBuf,
}

#[allow(dead_code)]
impl SessionStore {
    /// Open (and create if absent) the session directory for `session_id`.
    pub fn open(
        _repo_root: &std::path::Path,
        _git_dir: &std::path::Path,
        _session_id: &str,
    ) -> Result<Self> {
        unimplemented!()
    }

    /// Reset the session: truncate all four JSONL files and remove any prior
    /// `*.objects/` directories, then write fresh `baseline.state` and
    /// `last-flush.state`.
    pub fn reset(&mut self, _baseline: &BaselineState) -> Result<()> {
        unimplemented!()
    }

    /// Read `baseline.state`. Returns an error if the file is absent or invalid.
    pub fn read_baseline(&self) -> Result<BaselineState> {
        unimplemented!()
    }

    /// Read `last-flush.state`. Returns an error if the file is absent or invalid.
    pub fn read_last_flush(&self) -> Result<LastFlushState> {
        unimplemented!()
    }

    /// Append a `ReadRecord` to `reads.jsonl` under the advisory lock.
    pub fn append_read(&self, _record: &ReadRecord, _timeout: LockTimeout) -> Result<()> {
        unimplemented!()
    }

    /// Return all `ReadRecord` entries appended after byte offset `cursor`.
    pub fn reads_since_cursor(&self, _cursor: u64) -> Result<Vec<ReadRecord>> {
        unimplemented!()
    }

    /// Return the path to the `baseline.objects/` directory.
    pub fn baseline_objects_dir(&self) -> PathBuf {
        unimplemented!()
    }

    /// Return the path to the `last-flush.objects/` directory.
    pub fn last_flush_objects_dir(&self) -> PathBuf {
        unimplemented!()
    }
}

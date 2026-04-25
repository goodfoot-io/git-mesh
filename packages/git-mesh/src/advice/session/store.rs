//! Session directory layout, advisory lock, and atomic write helpers.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::Result;

/// Controls how `acquire_lock` behaves when the lock is already held.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub enum LockTimeout {
    /// Block indefinitely until the lock is released.
    Blocking,
    /// Return an error if the lock is not acquired within the given duration.
    Bounded(Duration),
}

/// RAII guard that releases the advisory lock on drop.
#[allow(dead_code)]
pub struct LockGuard {
    _fd: std::fs::File,
}

/// Return the base advice directory, honouring `GIT_MESH_ADVICE_DIR`.
#[allow(dead_code)]
pub fn advice_base_dir() -> PathBuf {
    unimplemented!()
}

/// Compute the per-repo directory key as lower-hex FNV-64 of `"{repo_root}\n{git_dir}"`.
#[allow(dead_code)]
pub fn repo_key(_repo_root: &std::path::Path, _git_dir: &std::path::Path) -> String {
    unimplemented!()
}

/// Return `<advice_base>/<repo_key>/<session_id>/`.
#[allow(dead_code)]
pub fn session_dir(_repo_root: &std::path::Path, _git_dir: &std::path::Path, _session_id: &str) -> PathBuf {
    unimplemented!()
}

/// Acquire the advisory lock for `dir/lock`, blocking or timing out per `timeout`.
#[allow(dead_code)]
pub fn acquire_lock(_dir: &std::path::Path, _timeout: LockTimeout) -> Result<LockGuard> {
    unimplemented!()
}

/// Write `contents` to `dest` atomically via a `.tmp` sibling and `rename`.
#[allow(dead_code)]
pub fn atomic_write(_dest: &std::path::Path, _contents: &[u8]) -> Result<()> {
    unimplemented!()
}

/// Append a single JSONL line under an already-held lock guard.
#[allow(dead_code)]
pub fn append_jsonl_line(_path: &std::path::Path, _guard: &LockGuard, _line: &str) -> Result<()> {
    unimplemented!()
}

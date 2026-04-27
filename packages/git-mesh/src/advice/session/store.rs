//! Session directory layout, advisory lock, and atomic write helpers.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use fs4::fs_std::FileExt;

/// Apply a Unix file mode to `OpenOptions`. No-op on non-Unix targets.
fn with_mode(opts: &mut OpenOptions, mode: u32) -> &mut OpenOptions {
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

/// Controls how `acquire_lock` behaves when the lock is already held.
#[derive(Debug, Clone)]
pub enum LockTimeout {
    /// Block indefinitely until the lock is released.
    Blocking,
    /// Return an error if the lock is not acquired within the given duration.
    Bounded(Duration),
}

/// RAII guard that releases the advisory lock on drop.
pub struct LockGuard {
    _fd: File,
}

/// Return the base advice directory, honouring `GIT_MESH_ADVICE_DIR`.
pub fn advice_base_dir() -> PathBuf {
    if let Ok(v) = std::env::var("GIT_MESH_ADVICE_DIR")
        && !v.is_empty()
    {
        return PathBuf::from(v);
    }
    std::env::temp_dir().join("git-mesh").join("advice")
}

/// FNV-64 hash, lower-hex.
fn fnv64_hex(input: &[u8]) -> String {
    let mut hash: u64 = 0xcbf29ce484222325;
    for b in input {
        hash ^= *b as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}

/// Compute the per-repo directory key as lower-hex FNV-64 of `"{repo_root}\n{git_dir}"`.
/// Paths are canonicalized when possible so a relative `.` and an absolute path
/// for the same directory hash to the same key.
pub fn repo_key(repo_root: &Path, git_dir: &Path) -> String {
    let r = std::fs::canonicalize(repo_root).unwrap_or_else(|_| repo_root.to_path_buf());
    let g = std::fs::canonicalize(git_dir).unwrap_or_else(|_| git_dir.to_path_buf());
    let mut s = String::new();
    s.push_str(&r.to_string_lossy());
    s.push('\n');
    s.push_str(&g.to_string_lossy());
    fnv64_hex(s.as_bytes())
}

/// Return `<advice_base>/<repo_key>/<session_id>/`.
pub fn session_dir(repo_root: &Path, git_dir: &Path, session_id: &str) -> PathBuf {
    advice_base_dir()
        .join(repo_key(repo_root, git_dir))
        .join(session_id)
}

/// Acquire the advisory lock for `dir/lock`, blocking or timing out per `timeout`.
pub fn acquire_lock(dir: &Path, timeout: LockTimeout) -> Result<LockGuard> {
    let lock_path = dir.join("lock");
    let f = with_mode(
        OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false),
        0o600,
    )
    .open(&lock_path)
    .with_context(|| format!("open lock file `{}`", lock_path.display()))?;
    match timeout {
        LockTimeout::Blocking => {
            FileExt::lock_exclusive(&f)
                .with_context(|| format!("lock_exclusive on `{}`", lock_path.display()))?;
        }
        LockTimeout::Bounded(dur) => {
            let deadline = Instant::now() + dur;
            loop {
                match FileExt::try_lock_exclusive(&f) {
                    Ok(true) => break,
                    Ok(false) => {}
                    Err(e) => {
                        return Err(e).with_context(|| {
                            format!("try_lock_exclusive on `{}`", lock_path.display())
                        });
                    }
                }
                if Instant::now() >= deadline {
                    bail!(
                        "could not acquire session lock; another advice command may be running (waited {:?} on `{}`)",
                        dur,
                        lock_path.display()
                    );
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    }
    Ok(LockGuard { _fd: f })
}

/// Write `contents` to `dest` atomically via a `.tmp` sibling and `rename`.
pub fn atomic_write(dest: &Path, contents: &[u8]) -> Result<()> {
    let parent = dest
        .parent()
        .ok_or_else(|| anyhow::anyhow!("atomic_write: dest `{}` has no parent", dest.display()))?;
    let file_name = dest
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("atomic_write: dest `{}` has no filename", dest.display()))?;
    let mut tmp_name = file_name.to_os_string();
    tmp_name.push(".tmp");
    let tmp = parent.join(&tmp_name);
    {
        let mut f = with_mode(
            OpenOptions::new().create(true).write(true).truncate(true),
            0o600,
        )
        .open(&tmp)
        .with_context(|| format!("open tmp `{}`", tmp.display()))?;
        f.write_all(contents)
            .with_context(|| format!("write tmp `{}`", tmp.display()))?;
        f.sync_all().ok();
    }
    std::fs::rename(&tmp, dest)
        .with_context(|| format!("rename `{}` -> `{}`", tmp.display(), dest.display()))?;
    Ok(())
}

/// Append a single JSONL line under an already-held lock guard.
pub fn append_jsonl_line(path: &Path, _guard: &LockGuard, line: &str) -> Result<()> {
    let mut f = with_mode(OpenOptions::new().create(true).append(true), 0o600)
        .open(path)
        .with_context(|| format!("open jsonl `{}`", path.display()))?;
    f.write_all(line.as_bytes())
        .with_context(|| format!("append jsonl `{}`", path.display()))?;
    if !line.ends_with('\n') {
        f.write_all(b"\n")?;
    }
    Ok(())
}

//! Long-running `git filter-process` protocol orchestration. Used by
//! both the LFS reader and the custom-driver dispatch. Each `FilterProcess`
//! owns a child subprocess + stdio handles for the duration of a `stale` run.

use crate::git;
use std::collections::HashMap;
use std::io::{BufReader, Read, Write};
use std::process::{Child, ChildStdin, ChildStdout, Stdio};

/// Why a `git filter-process`-protocol spawn failed. Cached on the
/// engine so subsequent reads in the same run return the same terminal
/// state without re-attempting the spawn.
#[derive(Clone, Debug)]
pub(crate) enum FilterSpawnError {
    NotInstalled,
    HandshakeFailed,
}

/// Owned long-running filter-process subprocess.
pub(crate) struct FilterProcess {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: BufReader<ChildStdout>,
}

impl FilterProcess {
    fn stdin_mut(&mut self) -> &mut ChildStdin {
        self.stdin.as_mut().expect("stdin only None during Drop")
    }
}

impl Drop for FilterProcess {
    fn drop(&mut self) {
        if let Some(mut s) = self.stdin.take() {
            let _ = s.flush();
        }
        let _ = self.child.wait();
    }
}

fn finalize_filter_process(
    mut child: Child,
) -> std::result::Result<FilterProcess, FilterSpawnError> {
    let stdin = child
        .stdin
        .take()
        .ok_or(FilterSpawnError::HandshakeFailed)?;
    let stdout = BufReader::new(
        child
            .stdout
            .take()
            .ok_or(FilterSpawnError::HandshakeFailed)?,
    );
    let mut p = FilterProcess {
        child,
        stdin: Some(stdin),
        stdout,
    };
    if filter_handshake(&mut p).is_err() {
        return Err(FilterSpawnError::HandshakeFailed);
    }
    Ok(p)
}

pub(crate) fn spawn_lfs_process(
    workdir: &std::path::Path,
) -> std::result::Result<FilterProcess, FilterSpawnError> {
    let child = std::process::Command::new("git-lfs")
        .arg("filter-process")
        .current_dir(workdir)
        .env("GIT_LFS_SKIP_SMUDGE", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| FilterSpawnError::NotInstalled)?;
    finalize_filter_process(child)
}

fn spawn_custom_filter_process(
    workdir: &std::path::Path,
    cmd: &str,
) -> std::result::Result<FilterProcess, FilterSpawnError> {
    let child = std::process::Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .current_dir(workdir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| FilterSpawnError::NotInstalled)?;
    finalize_filter_process(child)
}

fn filter_handshake(p: &mut FilterProcess) -> std::io::Result<()> {
    pkt_write_text(p.stdin_mut(), "git-filter-client\n")?;
    pkt_write_text(p.stdin_mut(), "version=2\n")?;
    pkt_flush(p.stdin_mut())?;
    p.stdin_mut().flush()?;
    let welcome = pkt_read_text(&mut p.stdout)?;
    if !welcome.starts_with("git-filter-server") {
        return Err(std::io::Error::other(format!("bad welcome: {welcome:?}")));
    }
    while pkt_read(&mut p.stdout)?.is_some() {}
    pkt_write_text(p.stdin_mut(), "capability=clean\n")?;
    pkt_write_text(p.stdin_mut(), "capability=smudge\n")?;
    pkt_flush(p.stdin_mut())?;
    p.stdin_mut().flush()?;
    while pkt_read(&mut p.stdout)?.is_some() {}
    Ok(())
}

pub(crate) fn filter_smudge(
    p: &mut FilterProcess,
    pathname: &str,
    input_bytes: &[u8],
) -> std::io::Result<Vec<u8>> {
    pkt_write_text(p.stdin_mut(), "command=smudge\n")?;
    pkt_write_text(p.stdin_mut(), &format!("pathname={pathname}\n"))?;
    pkt_flush(p.stdin_mut())?;
    p.stdin_mut().flush()?;
    for chunk in input_bytes.chunks(65516) {
        pkt_write_bytes(p.stdin_mut(), chunk)?;
    }
    pkt_flush(p.stdin_mut())?;
    p.stdin_mut().flush()?;
    let status1 = read_status_block(&mut p.stdout)?;
    if !status1.iter().any(|s| s.starts_with("status=success")) {
        return Err(std::io::Error::other(format!("smudge status: {status1:?}")));
    }
    let mut out = Vec::new();
    loop {
        match pkt_read(&mut p.stdout)? {
            None => break,
            Some(b) => out.extend_from_slice(&b),
        }
    }
    let _final = read_status_block(&mut p.stdout)?;
    Ok(out)
}

fn read_status_block(r: &mut BufReader<ChildStdout>) -> std::io::Result<Vec<String>> {
    let mut out = Vec::new();
    loop {
        match pkt_read(r)? {
            None => return Ok(out),
            Some(b) => out.push(String::from_utf8_lossy(&b).into_owned()),
        }
    }
}

// ---- pkt-line framing ----------------------------------------------------

fn pkt_write_text(w: &mut ChildStdin, s: &str) -> std::io::Result<()> {
    pkt_write_bytes(w, s.as_bytes())
}

fn pkt_write_bytes(w: &mut ChildStdin, bytes: &[u8]) -> std::io::Result<()> {
    let len = bytes.len() + 4;
    if len > 65520 {
        return Err(std::io::Error::other("pkt too large"));
    }
    let hdr = format!("{len:04x}");
    w.write_all(hdr.as_bytes())?;
    w.write_all(bytes)?;
    Ok(())
}

fn pkt_flush(w: &mut ChildStdin) -> std::io::Result<()> {
    w.write_all(b"0000")
}

fn pkt_read(r: &mut BufReader<ChildStdout>) -> std::io::Result<Option<Vec<u8>>> {
    let mut hdr = [0u8; 4];
    r.read_exact(&mut hdr)?;
    let hex = std::str::from_utf8(&hdr).map_err(|e| std::io::Error::other(format!("hdr: {e}")))?;
    let len =
        u32::from_str_radix(hex, 16).map_err(|e| std::io::Error::other(format!("hdr len: {e}")))?;
    if len == 0 {
        return Ok(None);
    }
    if len < 4 {
        return Err(std::io::Error::other(format!("bad pkt len: {len}")));
    }
    let body_len = (len - 4) as usize;
    let mut buf = vec![0u8; body_len];
    r.read_exact(&mut buf)?;
    Ok(Some(buf))
}

fn pkt_read_text(r: &mut BufReader<ChildStdout>) -> std::io::Result<String> {
    match pkt_read(r)? {
        None => Ok(String::new()),
        Some(b) => Ok(String::from_utf8_lossy(&b).into_owned()),
    }
}

// ---- Custom filter dispatch -----------------------------------------------

pub(crate) enum CustomFilterOutcome {
    Bytes(Vec<u8>),
    FilterFailed,
}

fn lookup_custom_filter_process_command(workdir: &std::path::Path, name: &str) -> Option<String> {
    let key = format!("filter.{name}.process");
    let repo = gix::open(workdir).ok()?;
    let v = git::config_string(&repo, &key)?;
    let trimmed = v.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

pub(crate) fn is_custom_filter_configured(repo: &gix::Repository, name: &str) -> bool {
    let Ok(workdir) = git::work_dir(repo) else {
        return false;
    };
    lookup_custom_filter_process_command(workdir, name).is_some()
}

pub(crate) type CustomFilters =
    HashMap<String, std::result::Result<FilterProcess, FilterSpawnError>>;

pub(crate) fn custom_filter_smudge(
    custom_filters: &mut CustomFilters,
    workdir: &std::path::Path,
    name: &str,
    pathname: &str,
    input_bytes: &[u8],
) -> CustomFilterOutcome {
    if !custom_filters.contains_key(name) {
        let spawned = match lookup_custom_filter_process_command(workdir, name) {
            None => return CustomFilterOutcome::FilterFailed,
            Some(cmd) => spawn_custom_filter_process(workdir, &cmd),
        };
        custom_filters.insert(name.to_string(), spawned);
    }
    match custom_filters.get_mut(name).expect("just inserted") {
        Err(_) => CustomFilterOutcome::FilterFailed,
        Ok(p) => match filter_smudge(p, pathname, input_bytes) {
            Ok(b) => CustomFilterOutcome::Bytes(b),
            Err(_) => CustomFilterOutcome::FilterFailed,
        },
    }
}

//! Shared test fixtures for git-mesh integration tests.
//!
//! Each `tests/*.rs` file is compiled as a separate crate, so items in
//! this module that are unused by a particular crate would normally
//! warn. We `#[allow(dead_code)]` at item granularity per CLAUDE.md
//! "right way over easy way" — no blanket module-level allow.

use anyhow::Result;
use std::fs;
use std::path::Path;
use std::process::{Command, Output};

/// A scratch git repository, owned by a tempdir that's cleaned up on
/// drop. Set up with `user.name` / `user.email` so commits work without
/// global config.
#[allow(dead_code)]
pub struct TestRepo {
    pub dir: tempfile::TempDir,
}

#[allow(dead_code)]
impl TestRepo {
    /// New empty repo: `git init`, identity configured, no commits yet.
    pub fn new() -> Result<Self> {
        let dir = tempfile::tempdir()?;
        let me = Self { dir };
        me.run_git(["init", "--initial-branch=main"])?;
        me.run_git(["config", "user.name", "Test User"])?;
        me.run_git(["config", "user.email", "test@example.com"])?;
        me.run_git(["config", "commit.gpgsign", "false"])?;
        Ok(me)
    }

    /// New repo seeded with a single initial commit containing a
    /// 10-line `file1.txt` and a 16-line `file2.txt`. Convenient for
    /// staging-add tests that need a real anchor.
    pub fn seeded() -> Result<Self> {
        let me = Self::new()?;
        me.write_file(
            "file1.txt",
            "line1\nline2\nline3\nline4\nline5\nline6\nline7\nline8\nline9\nline10\n",
        )?;
        me.write_file(
            "file2.txt",
            "line1\nline2\nline3\nline4\nline5\nline6\nline7\nline8\nline9\nline10\nline11\nline12\nline13\nline14\nline15\nline16\n",
        )?;
        me.commit_all("initial commit")?;
        Ok(me)
    }

    pub fn path(&self) -> &Path {
        self.dir.path()
    }

    /// Open the repo via `gix` for direct library calls.
    pub fn gix_repo(&self) -> Result<gix::Repository> {
        Ok(gix::open(self.dir.path())?)
    }

    pub fn write_file(&self, rel: &str, contents: &str) -> Result<()> {
        let p = self.dir.path().join(rel);
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(p, contents)?;
        Ok(())
    }

    pub fn write_file_lines(&self, rel: &str, n: u32) -> Result<()> {
        let mut buf = String::with_capacity((n as usize) * 8);
        for i in 1..=n {
            buf.push_str(&format!("line{i}\n"));
        }
        self.write_file(rel, &buf)
    }

    /// `git add . && git commit -m <msg>`; returns the new HEAD sha.
    pub fn commit_all(&self, msg: &str) -> Result<String> {
        self.run_git(["add", "-A"])?;
        self.run_git(["commit", "-m", msg])?;
        self.head_sha()
    }

    /// Stage and commit a file in one shot, returning the new HEAD sha.
    pub fn commit_file(&self, rel: &str, contents: &str, msg: &str) -> Result<String> {
        self.write_file(rel, contents)?;
        self.commit_all(msg)
    }

    pub fn head_sha(&self) -> Result<String> {
        self.git_stdout(["rev-parse", "HEAD"])
    }

    pub fn run_git<I, S>(&self, args: I) -> Result<Output>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut cmd = Command::new("git");
        cmd.current_dir(self.dir.path());
        for a in args {
            cmd.arg(a.as_ref());
        }
        let out = cmd.output()?;
        anyhow::ensure!(
            out.status.success(),
            "git failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        Ok(out)
    }

    pub fn git_stdout<I, S>(&self, args: I) -> Result<String>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let out = self.run_git(args)?;
        Ok(String::from_utf8(out.stdout)?.trim().to_string())
    }

    /// `git for-each-ref --format=%(refname) <prefix>`.
    pub fn list_refs(&self, prefix: &str) -> Result<Vec<String>> {
        Ok(self
            .git_stdout(["for-each-ref", "--format=%(refname)", prefix])?
            .lines()
            .filter(|l| !l.is_empty())
            .map(str::to_string)
            .collect())
    }

    pub fn ref_exists(&self, name: &str) -> bool {
        self.git_stdout(["rev-parse", "--verify", "--quiet", name])
            .is_ok()
    }

    pub fn add_remote(&self, name: &str, path: &Path) -> Result<()> {
        self.run_git(["remote", "add", name, &path.to_string_lossy()])?;
        Ok(())
    }

    /// Run the `git-mesh` binary in this repo's directory.
    pub fn run_mesh<I, S>(&self, args: I) -> Result<Output>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_git-mesh"));
        cmd.current_dir(self.dir.path());
        for a in args {
            cmd.arg(a.as_ref());
        }
        Ok(cmd.output()?)
    }

    pub fn mesh_stdout<I, S>(&self, args: I) -> Result<String>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let out = self.run_mesh(args)?;
        anyhow::ensure!(
            out.status.success(),
            "git-mesh failed (code {:?}): {}",
            out.status.code(),
            String::from_utf8_lossy(&out.stderr)
        );
        Ok(String::from_utf8(out.stdout)?)
    }
}

/// Stage an `add` line via the library directly (skips CLI parsing).
#[allow(dead_code)]
pub fn add_range(
    repo: &gix::Repository,
    mesh: &str,
    path: &str,
    start: u32,
    end: u32,
    anchor: Option<&str>,
) -> git_mesh::Result<()> {
    git_mesh::staging::append_add(repo, mesh, path, start, end, anchor)
}

/// Commit a mesh via the library directly. Returns the new tip OID.
#[allow(dead_code)]
pub fn commit_mesh(repo: &gix::Repository, mesh: &str) -> git_mesh::Result<String> {
    git_mesh::mesh::commit_mesh(repo, mesh)
}

/// Bare upstream repo, for `fetch`/`push` round-trips.
#[allow(dead_code)]
pub struct BareRepo {
    pub dir: tempfile::TempDir,
}

#[allow(dead_code)]
impl BareRepo {
    pub fn new() -> Result<Self> {
        let dir = tempfile::tempdir()?;
        let out = Command::new("git")
            .args(["init", "--bare"])
            .arg(dir.path())
            .output()?;
        anyhow::ensure!(
            out.status.success(),
            "git init --bare failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        Ok(Self { dir })
    }

    pub fn path(&self) -> &Path {
        self.dir.path()
    }
}

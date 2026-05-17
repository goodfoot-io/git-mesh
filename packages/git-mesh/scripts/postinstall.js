#!/usr/bin/env node

const fs = require('fs');
const path = require('path');
const { spawnSync } = require('child_process');

const PLATFORM_MAP = {
  linux: 'linux',
  darwin: 'darwin',
  win32: 'win32'
};

const ARCH_MAP = {
  x64: 'x64',
  arm64: 'arm64'
};

function fail(message, error) {
  console.error(message);
  if (error) {
    console.error(error.message);
  }
  process.exit(1);
}

function buildFromSource(destBinary, binaryName) {
  const cargoToml = path.join(__dirname, '..', 'Cargo.toml');
  if (!fs.existsSync(cargoToml)) {
    fail(`@goodfoot/git-mesh: Binary not found at ${destBinary} and no Cargo.toml available to build from source.`);
  }

  console.log(`@goodfoot/git-mesh: Prebuilt binary missing; building from source via cargo...`);

  const targetDir = path.join(__dirname, '..', 'target-cache', 'build');
  const result = spawnSync('cargo', ['build', '--release', '--manifest-path', cargoToml], {
    stdio: 'inherit',
    env: { ...process.env, CARGO_BUILD_JOBS: '1', CARGO_TARGET_DIR: targetDir }
  });

  if (result.error || result.status !== 0) {
    fail(
      `@goodfoot/git-mesh: Failed to build binary from source. Install Rust/cargo or publish the platform package.`,
      result.error
    );
  }

  const builtBinary = path.join(targetDir, 'release', binaryName);
  if (!fs.existsSync(builtBinary)) {
    fail(`@goodfoot/git-mesh: cargo build succeeded but binary not found at ${builtBinary}.`);
  }

  fs.mkdirSync(path.dirname(destBinary), { recursive: true });
  fs.copyFileSync(builtBinary, destBinary);
  fs.chmodSync(destBinary, 0o755);
}

function isInsideSourceTree() {
  // Walk up from this script looking for a `.git` entry. If found, this is
  // the @goodfoot/git-mesh source repo (developer doing `yarn install`), not
  // a consumer install under node_modules. Mutating the tracked `bin/git-mesh`
  // shim in that case dirties the working tree and — if committed — ships a
  // dangling symlink in the published tarball (see git-mesh-v1.0.25 regression).
  let dir = path.resolve(__dirname, '..');
  for (let i = 0; i < 16; i++) {
    if (fs.existsSync(path.join(dir, '.git'))) return true;
    const parent = path.dirname(dir);
    if (parent === dir) return false;
    if (path.basename(parent) === 'node_modules') return false;
    dir = parent;
  }
  return false;
}

function main() {
  if (isInsideSourceTree()) {
    console.log('@goodfoot/git-mesh: postinstall skipped (running inside source tree).');
    return;
  }

  const platform = PLATFORM_MAP[process.platform];
  const arch = ARCH_MAP[process.arch];

  if (!platform || !arch) {
    fail(`@goodfoot/git-mesh: No prebuilt binary available for ${process.platform}-${process.arch}.`);
  }

  const packageName = `@goodfoot/git-mesh-${platform}-${arch}`;
  const sourceBinaryName = process.platform === 'win32' ? 'git-mesh.exe' : 'git-mesh';

  let packageDir;
  try {
    packageDir = path.dirname(require.resolve(`${packageName}/package.json`));
  } catch (error) {
    fail(`@goodfoot/git-mesh: Required platform package ${packageName} not found.`, error);
  }

  const sourceBinary = path.join(packageDir, 'bin', sourceBinaryName);
  if (!fs.existsSync(sourceBinary)) {
    buildFromSource(sourceBinary, sourceBinaryName);
  }

  // Always write to bin/git-mesh.exe — the package.json `bin` field points
  // here on every platform. The `.exe` extension plus a no-shebang stub makes
  // npm's cmd-shim (generated at install time, before this postinstall) emit
  // a direct exec on Windows; Unix executes by mode bit + ELF/Mach-O header
  // and ignores the `.exe` suffix entirely. No resident Node process — the
  // `git-mesh` command execs the native binary directly. Same pattern as
  // Bun's and Claude Code's npm packages.
  const binGitMesh = path.join(__dirname, '..', 'bin', 'git-mesh.exe');

  // Hardlink first (instant, zero extra disk for the ~16MB binary; src and
  // dest are both under node_modules so same-filesystem is the common case),
  // then fall back to copy across devices / on link-permission errors.
  try {
    fs.linkSync(sourceBinary, binGitMesh);
  } catch (linkError) {
    if (linkError.code === 'EEXIST') {
      try {
        fs.unlinkSync(binGitMesh);
        fs.linkSync(sourceBinary, binGitMesh);
      } catch {
        fs.copyFileSync(sourceBinary, binGitMesh);
      }
    } else if (linkError.code === 'EXDEV' || linkError.code === 'EPERM') {
      fs.copyFileSync(sourceBinary, binGitMesh);
    } else {
      try {
        fs.copyFileSync(sourceBinary, binGitMesh);
      } catch (copyError) {
        fail(
          `@goodfoot/git-mesh: Could not install binary from ${sourceBinary} to ${binGitMesh}.`,
          new Error(`link failed: ${linkError.message}\ncopy failed: ${copyError.message}`)
        );
      }
    }
  }
  if (process.platform !== 'win32') {
    fs.chmodSync(binGitMesh, 0o755);
  }

  console.log(`@goodfoot/git-mesh: Installed git-mesh from ${packageName}`);
}

main();

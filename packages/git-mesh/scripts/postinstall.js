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

  const targetDir = path.join(__dirname, '..', 'target', 'build');
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

function main() {
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

  const binGitMesh = path.join(__dirname, '..', 'bin', 'git-mesh');

  try {
    fs.unlinkSync(binGitMesh);
  } catch (error) {
    if (error.code !== 'ENOENT') {
      fail(`@goodfoot/git-mesh: Could not remove existing binary at ${binGitMesh}.`, error);
    }
  }

  // Try symlink first, fall back to copy
  try {
    fs.symlinkSync(sourceBinary, binGitMesh);
  } catch (symlinkError) {
    try {
      fs.copyFileSync(sourceBinary, binGitMesh);
      fs.chmodSync(binGitMesh, 0o755);
    } catch (copyError) {
      fail(
        `@goodfoot/git-mesh: Could not install binary from ${sourceBinary} to ${binGitMesh}.`,
        new Error(`symlink failed: ${symlinkError.message}\ncopy failed: ${copyError.message}`)
      );
    }
  }

  console.log(`@goodfoot/git-mesh: Installed git-mesh from ${packageName}`);
}

main();

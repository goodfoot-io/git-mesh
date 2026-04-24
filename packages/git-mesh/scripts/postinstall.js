#!/usr/bin/env node

const fs = require('fs');
const path = require('path');

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
    fail(`@goodfoot/git-mesh: Binary not found in ${packageName}. The package may not have been published correctly.`);
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

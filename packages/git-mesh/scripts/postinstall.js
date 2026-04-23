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

function main() {
  const platform = PLATFORM_MAP[process.platform];
  const arch = ARCH_MAP[process.arch];

  if (!platform || !arch) {
    console.log(
      `@goodfoot/git-mesh: No prebuilt binary available for ${process.platform}-${process.arch}. Build from source with "cargo build --release".`
    );
    process.exit(0);
  }

  const packageName = `@goodfoot/git-mesh-${platform}-${arch}`;
  const sourceBinaryName = process.platform === 'win32' ? 'git-mesh.exe' : 'git-mesh';

  let packageDir;
  try {
    packageDir = path.dirname(require.resolve(`${packageName}/package.json`));
  } catch {
    // Platform package not installed -- user may have built from source
    console.log(`@goodfoot/git-mesh: Optional package ${packageName} not found. Skipping binary setup.`);
    process.exit(0);
  }

  const sourceBinary = path.join(packageDir, 'bin', sourceBinaryName);
  if (!fs.existsSync(sourceBinary)) {
    console.log(`@goodfoot/git-mesh: Binary not found in ${packageName}. The package may not have been published yet.`);
    process.exit(0);
  }

  const binGitMesh = path.join(__dirname, '..', 'bin', 'git-mesh');

  try {
    fs.unlinkSync(binGitMesh);
  } catch {
    // Ignore if it doesn't exist
  }

  // Try symlink first, fall back to copy
  try {
    fs.symlinkSync(sourceBinary, binGitMesh);
  } catch {
    fs.copyFileSync(sourceBinary, binGitMesh);
    fs.chmodSync(binGitMesh, 0o755);
  }

  console.log(`@goodfoot/git-mesh: Installed git-mesh from ${packageName}`);
}

main();

/**
 * Platform detection helpers for the managed Git Mesh CLI installer.
 *
 * Defines the supported release matrix and the storage layout used by the
 * extension's binary manager.
 *
 * @summary Platform detection helpers for the managed Git Mesh CLI installer.
 */

import * as path from 'node:path';

export interface GitMeshPlatformTarget {
  platform: NodeJS.Platform;
  arch: NodeJS.Architecture;
  assetKey: string;
  assetName: string;
  executableName: string;
  storageKey: string;
}

const SUPPORTED_TARGETS: ReadonlyArray<GitMeshPlatformTarget> = [
  {
    platform: 'linux',
    arch: 'x64',
    assetKey: 'linux-x64',
    assetName: 'git-mesh-linux-x64',
    executableName: 'git-mesh',
    storageKey: 'linux-x64'
  },
  {
    platform: 'linux',
    arch: 'arm64',
    assetKey: 'linux-arm64',
    assetName: 'git-mesh-linux-arm64',
    executableName: 'git-mesh',
    storageKey: 'linux-arm64'
  },
  {
    platform: 'darwin',
    arch: 'x64',
    assetKey: 'darwin-x64',
    assetName: 'git-mesh-darwin-x64',
    executableName: 'git-mesh',
    storageKey: 'darwin-x64'
  },
  {
    platform: 'darwin',
    arch: 'arm64',
    assetKey: 'darwin-arm64',
    assetName: 'git-mesh-darwin-arm64',
    executableName: 'git-mesh',
    storageKey: 'darwin-arm64'
  },
  {
    platform: 'win32',
    arch: 'x64',
    assetKey: 'win32-x64',
    assetName: 'git-mesh-win32-x64.exe',
    executableName: 'git-mesh.exe',
    storageKey: 'win32-x64'
  }
];

/**
 * Resolve the current host platform and architecture against the supported matrix.
 *
 * @param platform - Host platform to resolve.
 * @param arch - Host architecture to resolve.
 * @returns Matching supported target, or null when unsupported.
 */
export function resolveGitMeshPlatform(
  platform: NodeJS.Platform = process.platform,
  arch: NodeJS.Architecture = process.arch
): GitMeshPlatformTarget | null {
  return SUPPORTED_TARGETS.find((target) => target.platform === platform && target.arch === arch) ?? null;
}

/**
 * Return the release tag used to publish CLI assets for a given extension version.
 *
 * @param version - Extension version that must match the CLI release version.
 * @returns Git tag used for the CLI release.
 */
export function getGitMeshReleaseTag(version: string): string {
  return `git-mesh-v${version}`;
}

/**
 * Return the fixed release asset name for the checksum manifest.
 *
 * @returns Release asset filename.
 */
export function getGitMeshChecksumsAssetName(): string {
  return 'git-mesh-cli-checksums.json';
}

/**
 * Compute the managed binary and manifest paths for a specific release target.
 *
 * @param storageRoot - Extension global storage directory.
 * @param version - Extension/CLI version.
 * @param target - Supported platform target.
 * @returns Managed binary directory, binary path, and manifest path.
 */
export function getManagedBinaryPaths(
  storageRoot: string,
  version: string,
  target: GitMeshPlatformTarget
): {
  binaryDirectory: string;
  binaryPath: string;
  manifestDirectory: string;
  manifestPath: string;
} {
  const binaryDirectory = path.join(storageRoot, 'bin', version, target.storageKey);
  return {
    binaryDirectory,
    binaryPath: path.join(binaryDirectory, target.executableName),
    manifestDirectory: path.join(storageRoot, 'manifests', version),
    manifestPath: path.join(storageRoot, 'manifests', version, `${target.storageKey}.json`)
  };
}

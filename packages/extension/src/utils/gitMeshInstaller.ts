/**
 * VS Code-facing Git Mesh CLI manager.
 *
 * Bridges extension activation, managed binary installation, terminal PATH
 * injection, and explicit development PATH fallback.
 *
 * @summary VS Code-facing Git Mesh CLI manager.
 */

import * as path from 'node:path';
import * as vscode from 'vscode';
import {
  GitMeshBinaryError,
  type GitMeshBinaryHandle,
  getGitMeshBinaryErrorMessage,
  type InstallManagedGitMeshBinaryResult,
  installManagedGitMeshBinary,
  resolveGitMeshBinaryOnPath,
  resolveManagedGitMeshBinary
} from './gitMeshBinary.js';
import { resolveGitMeshPlatform } from './gitMeshPlatform.js';

interface GitMeshBinaryReadyResult {
  handle: GitMeshBinaryHandle;
  installed: boolean;
}

const DEFAULT_RELEASE_BASE_URL = 'https://github.com/goodfoot-io/git-mesh/releases/download';

export class GitMeshBinaryManager {
  private readyPromise: Promise<GitMeshBinaryReadyResult> | null = null;

  constructor(private readonly context: vscode.ExtensionContext) {}

  start(): Promise<GitMeshBinaryReadyResult> {
    this.readyPromise ??= this.ensureReady();
    return this.readyPromise;
  }

  async ready(): Promise<GitMeshBinaryHandle> {
    return (await this.start()).handle;
  }

  retry(): Promise<GitMeshBinaryReadyResult> {
    this.readyPromise = null;
    return this.start();
  }

  formatFailure(error: unknown): string {
    return `${getGitMeshBinaryErrorMessage(error)} Run "Git Mesh: Retry CLI Install" and try again.`;
  }

  private async ensureReady(): Promise<GitMeshBinaryReadyResult> {
    const version = this.extensionVersion();
    const releaseBaseUrl = this.releaseBaseUrl();
    const storageRoot = this.context.globalStorageUri.fsPath;

    const managed = await resolveManagedGitMeshBinary({ storageRoot, version, releaseBaseUrl });
    if (managed != null) {
      this.configureTerminalPath(path.dirname(managed.path));
      return { handle: managed, installed: false };
    }

    if (this.shouldUsePathFallback()) {
      const pathBinary = await resolveGitMeshBinaryOnPath();
      if (pathBinary != null) {
        return { handle: pathBinary, installed: false };
      }
    }

    const target = resolveGitMeshPlatform();
    if (target == null) {
      throw new GitMeshBinaryError(
        `git-mesh is not available for ${process.platform}-${process.arch} in this release.`
      );
    }

    const installed = await installManagedGitMeshBinary({
      storageRoot,
      version,
      releaseBaseUrl,
      platform: target.platform,
      arch: target.arch
    });
    this.configureTerminalPath(path.dirname(installed.handle.path));
    return installed;
  }

  private extensionVersion(): string {
    const pkg = this.context.extension.packageJSON as { version?: string };
    const version = pkg.version;
    if (typeof version !== 'string' || version.length === 0) {
      throw new GitMeshBinaryError('Extension package.json is missing a version.');
    }
    return version;
  }

  private releaseBaseUrl(): string {
    const envOverride = process.env['GIT_MESH_EXTENSION_RELEASE_BASE_URL'];
    if (envOverride != null && envOverride.length > 0) {
      return envOverride;
    }
    return vscode.workspace.getConfiguration('gitMesh').get<string>('binary.releaseBaseUrl', DEFAULT_RELEASE_BASE_URL);
  }

  private shouldUsePathFallback(): boolean {
    const envOverride = process.env['GIT_MESH_EXTENSION_USE_PATH_FALLBACK'];
    if (envOverride != null) {
      return envOverride === '1' || envOverride.toLowerCase() === 'true';
    }
    if (this.context.extensionMode === vscode.ExtensionMode.Development) {
      return true;
    }
    return vscode.workspace.getConfiguration('gitMesh').get<boolean>('binary.usePathFallback', false);
  }

  private configureTerminalPath(binDirectory: string): void {
    const collection = this.context.environmentVariableCollection;
    collection.clear();
    collection.description = 'Adds the managed Git Mesh CLI to integrated terminal PATH.';
    collection.persistent = true;
    collection.prepend('PATH', `${binDirectory}${path.delimiter}`);
  }
}

/**
 * Return true when a binary-manager resolution performed a fresh managed install.
 *
 * @param result - Binary-manager start or install result.
 * @returns Whether a new managed binary was installed.
 */
export function wasManagedInstall(result: InstallManagedGitMeshBinaryResult | GitMeshBinaryReadyResult): boolean {
  return result.handle.source === 'managed' && result.installed;
}

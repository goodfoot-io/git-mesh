/**
 * VS Code extension entry point for the Git Mesh CLI.
 *
 * Resolves `git-mesh` from PATH on demand. No managed install -- the binary
 * must be installed independently (npm, Homebrew, or direct download).
 *
 * @summary VS Code extension entry point for the Git Mesh CLI.
 */

import * as vscode from 'vscode';
import {
  GitMeshBinaryError,
  getGitMeshBinaryErrorMessage,
  resolveGitMeshBinaryOnPath,
  runGitMeshCommand
} from './utils/gitMeshBinary.js';

const MISSING_GIT_MESH_MESSAGE =
  'git-mesh is not on PATH. Install it via npm (`npm install -g git-mesh`), Homebrew (`brew install goodfoot-io/tap/git-mesh`), or download from https://github.com/goodfoot-io/git-mesh/releases.';

/**
 * Called by VS Code when the extension is activated.
 *
 * @param context - The VS Code extension context providing subscriptions and URIs.
 */
export function activate(context: vscode.ExtensionContext): void {
  // Drop any PATH entry persisted by a prior extension version that used a
  // managed install. New terminals will inherit the ambient PATH.
  context.environmentVariableCollection.clear();

  context.subscriptions.push(
    vscode.commands.registerCommand('gitMesh.showVersion', async () => {
      try {
        const binaryPath = await resolveGitMeshBinaryOnPath();
        if (binaryPath == null) {
          throw new GitMeshBinaryError(MISSING_GIT_MESH_MESSAGE);
        }
        const result = await runGitMeshCommand(binaryPath, ['--version']);
        if (result.exitCode !== 0) {
          throw new Error(result.stderr.trim() || `git-mesh --version exited with code ${result.exitCode}.`);
        }
        void vscode.window.showInformationMessage(result.stdout.trim());
      } catch (error) {
        void vscode.window.showErrorMessage(`Git Mesh: ${getGitMeshBinaryErrorMessage(error)}`);
      }
    }),

    vscode.commands.registerCommand('gitMesh.openTerminal', async () => {
      try {
        const binaryPath = await resolveGitMeshBinaryOnPath();
        if (binaryPath == null) {
          throw new GitMeshBinaryError(MISSING_GIT_MESH_MESSAGE);
        }
        const terminal = vscode.window.createTerminal({ name: 'Git Mesh' });
        terminal.show();
        terminal.sendText(`"${binaryPath}" --help`);
      } catch (error) {
        void vscode.window.showErrorMessage(`Git Mesh: ${getGitMeshBinaryErrorMessage(error)}`);
      }
    })
  );
}

/**
 * Called by VS Code when the extension is deactivated.
 */
export function deactivate(): void {
  // No-op.
}

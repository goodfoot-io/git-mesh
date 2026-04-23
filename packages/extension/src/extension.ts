/**
 * VS Code extension entry point for the Git Mesh binary manager.
 *
 * @summary VS Code extension entry point for the Git Mesh binary manager.
 */

import * as vscode from 'vscode';
import { runGitMeshCommand } from './utils/gitMeshBinary.js';
import { GitMeshBinaryManager, wasManagedInstall } from './utils/gitMeshInstaller.js';

/**
 * Called by VS Code when the extension is activated.
 *
 * @param context - The VS Code extension context providing subscriptions and URIs.
 */
export function activate(context: vscode.ExtensionContext): void {
  const binaryManager = new GitMeshBinaryManager(context);

  void binaryManager
    .start()
    .then((result) => {
      if (wasManagedInstall(result)) {
        void vscode.window.showInformationMessage(
          '`git-mesh` is installed for this extension. New integrated terminals will have it on PATH.'
        );
      }
    })
    .catch((error) => {
      console.error('[git-mesh] Failed to prepare managed Git Mesh CLI:', error);
    });

  context.subscriptions.push(
    vscode.commands.registerCommand('gitMesh.retryInstall', async () => {
      try {
        const result = await vscode.window.withProgress(
          { location: vscode.ProgressLocation.Notification, title: 'Installing Git Mesh CLI...' },
          () => binaryManager.retry()
        );
        if (wasManagedInstall(result)) {
          void vscode.window.showInformationMessage(
            '`git-mesh` is installed for this extension. New integrated terminals will have it on PATH.'
          );
        }
      } catch (error) {
        void vscode.window.showErrorMessage(`Git Mesh: ${binaryManager.formatFailure(error)}`);
      }
    }),

    vscode.commands.registerCommand('gitMesh.showVersion', async () => {
      try {
        const handle = await binaryManager.ready();
        const result = await runGitMeshCommand(handle.path, ['--version']);
        if (result.exitCode !== 0) {
          throw new Error(result.stderr.trim() || `git-mesh --version exited with code ${result.exitCode}.`);
        }
        void vscode.window.showInformationMessage(result.stdout.trim());
      } catch (error) {
        void vscode.window.showErrorMessage(`Git Mesh: ${binaryManager.formatFailure(error)}`);
      }
    }),

    vscode.commands.registerCommand('gitMesh.openTerminal', async () => {
      try {
        const handle = await binaryManager.ready();
        const terminal = vscode.window.createTerminal({ name: 'Git Mesh' });
        terminal.show();
        terminal.sendText(`"${handle.path}" --help`);
      } catch (error) {
        void vscode.window.showErrorMessage(`Git Mesh: ${binaryManager.formatFailure(error)}`);
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

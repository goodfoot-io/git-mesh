/**
 * Integration-style tests for managed Git Mesh binary resolution helpers.
 *
 * Covers PATH fallback discovery plus checksum-verified managed installs.
 *
 * @summary Managed Git Mesh binary helper tests.
 * @module test/suite/gitMeshBinary.test
 */

import * as assert from 'node:assert';
import { createHash } from 'node:crypto';
import * as fs from 'node:fs';
import { createServer } from 'node:http';
import * as os from 'node:os';
import * as path from 'node:path';
import {
  installManagedGitMeshBinary,
  resolveGitMeshBinaryOnPath,
  resolveManagedGitMeshBinary,
  runGitMeshCommand
} from '../../src/utils/gitMeshBinary.js';
import {
  getGitMeshChecksumsAssetName,
  getGitMeshReleaseTag,
  resolveGitMeshPlatform
} from '../../src/utils/gitMeshPlatform.js';

describe('gitMeshBinary', () => {
  it('resolves git-mesh on PATH when present', async () => {
    const tempDir = fs.mkdtempSync(path.join(os.tmpdir(), 'git-mesh-path-'));
    try {
      writeFixtureBinary(tempDir);
      const resolved = await resolveGitMeshBinaryOnPath(
        process.platform,
        `${tempDir}${path.delimiter}${process.env['PATH'] ?? ''}`
      );
      assert.ok(resolved, 'Expected Git Mesh binary to resolve from PATH');
      assert.strictEqual(resolved?.source, 'path');
    } finally {
      fs.rmSync(tempDir, { recursive: true, force: true });
    }
  });

  it('downloads, verifies, and resolves the managed Git Mesh binary', async function () {
    if (process.platform === 'win32') {
      this.skip();
    }

    const target = resolveGitMeshPlatform();
    assert.ok(target, 'Expected the current platform to be supported');

    const storageRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'git-mesh-managed-'));
    const releaseRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'git-mesh-release-'));
    const version = '9.9.9-test';
    const tag = getGitMeshReleaseTag(version);
    const fixtureBinaryPath = writeFixtureBinary(releaseRoot, target?.assetName ?? 'git-mesh');
    const assetBytes = fs.readFileSync(fixtureBinaryPath);
    const sha256 = createHash('sha256').update(assetBytes).digest('hex');

    const server = createServer((request, response) => {
      if (request.url === `/${tag}/${getGitMeshChecksumsAssetName()}`) {
        response.writeHead(200, { 'content-type': 'application/json' });
        response.end(
          JSON.stringify({
            version,
            assets: {
              [target?.assetKey ?? 'unknown']: {
                name: target?.assetName,
                sha256
              }
            }
          })
        );
        return;
      }

      if (request.url === `/${tag}/${target?.assetName}`) {
        response.writeHead(200, { 'content-type': 'application/octet-stream' });
        response.end(assetBytes);
        return;
      }

      response.writeHead(404);
      response.end();
    });

    await new Promise<void>((resolve) => server.listen(0, '127.0.0.1', () => resolve()));
    const address = server.address();
    assert.ok(address && typeof address === 'object', 'Expected HTTP server address');
    const releaseBaseUrl = `http://127.0.0.1:${address.port}`;

    try {
      const installed = await installManagedGitMeshBinary({ storageRoot, version, releaseBaseUrl });
      assert.strictEqual(installed.installed, true);
      assert.strictEqual(installed.handle.source, 'managed');

      const resolved = await resolveManagedGitMeshBinary({ storageRoot, version, releaseBaseUrl });
      assert.deepStrictEqual(resolved, installed.handle);

      const commandResult = await runGitMeshCommand(installed.handle.path, ['list', '--format', 'json']);
      assert.strictEqual(commandResult.exitCode, 0);
      assert.strictEqual(commandResult.stdout.trim(), '[]');
    } finally {
      server.close();
      fs.rmSync(storageRoot, { recursive: true, force: true });
      fs.rmSync(releaseRoot, { recursive: true, force: true });
    }
  });
});

function writeFixtureBinary(
  directory: string,
  fileName = process.platform === 'win32' ? 'git-mesh.cmd' : 'git-mesh'
): string {
  const scriptPath = path.join(directory, fileName);

  if (process.platform === 'win32') {
    fs.writeFileSync(
      scriptPath,
      "@echo off\r\nnode -e \"const args=process.argv.slice(1);if(args[0]==='list'){process.stdout.write('[]');process.exit(0)}process.stdout.write('[]');\"\r\n"
    );
    return scriptPath;
  }

  fs.writeFileSync(
    scriptPath,
    `#!/usr/bin/env node
const args = process.argv.slice(2);
if (args[0] === 'list') {
  process.stdout.write('[]');
  process.exit(0);
}
process.stdout.write('[]');
`,
    { mode: 0o755 }
  );
  return scriptPath;
}

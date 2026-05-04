/**
 * Tests for Git Mesh CLI PATH resolution helper.
 *
 * @summary Git Mesh CLI PATH resolution tests.
 * @module test/suite/gitMeshBinary.test
 */

import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { resolveGitMeshBinaryOnPath } from '../../src/utils/gitMeshBinary.js';

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
      assert.strictEqual(typeof resolved, 'string');
      assert.ok(resolved.length > 0);
    } finally {
      fs.rmSync(tempDir, { recursive: true, force: true });
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

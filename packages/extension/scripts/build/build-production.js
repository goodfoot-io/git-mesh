/**
 * Production build script for the Git Mesh extension.
 *
 * Invoked via the `vscode:prepublish` lifecycle hook before `vsce package`.
 * Outputs into dist/ relative to the extension root so package.json's
 * `"main": "./dist/bundle.cjs"` resolves correctly.
 */

import * as esbuild from 'esbuild';
import * as fs from 'node:fs';
import * as path from 'node:path';
import { fileURLToPath } from 'node:url';

const EXTENSION_ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../..');
const DIST = path.join(EXTENSION_ROOT, 'dist');

fs.mkdirSync(DIST, { recursive: true });

// Extension host — runs in Node.js inside VS Code.
await esbuild.build({
  entryPoints: [path.join(EXTENSION_ROOT, 'src/extension.ts')],
  bundle: true,
  outfile: path.join(DIST, 'bundle.cjs'),
  format: 'cjs',
  platform: 'node',
  target: 'node22',
  external: ['vscode'],
  sourcemap: true,
  minify: true
});

console.log('[build-production] Done.');

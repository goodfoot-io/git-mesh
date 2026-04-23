/**
 * Discovers compiled test files and runs them with Mocha.
 *
 * Simplified runner with no caching infrastructure — git-mesh-extension has a small
 * test suite that does not benefit from fingerprint-based caching.
 *
 * Supports the TEST_PATTERN environment variable for focused runs.
 *
 * @summary Mocha test runner entry point for the git-mesh-extension test suite.
 * @module test/suite/index
 */

import * as path from 'node:path';
import { glob } from 'glob';
import Mocha from 'mocha';

// Mocha registers process-level error listeners for each test file; raise the
// limit to avoid spurious MaxListenersExceededWarning output.
process.setMaxListeners(0);

/**
 * Runs all test suites discovered under the compiled test directory.
 *
 * Called by @vscode/test-electron after the Extension Host has loaded the extension.
 * Must export a `run` function — that is the contract required by the test runner.
 *
 * @returns Promise that resolves when all tests pass, rejects on any failure.
 * @throws Error with failure count when one or more tests fail.
 */
export async function run(): Promise<void> {
  // __dirname resolves to the directory containing this compiled .cjs file, which
  // is {TEST_DIST_PATH}/test/suite/. One level up is {TEST_DIST_PATH}/test/.
  const testsRoot = path.resolve(__dirname, '..');

  const testPattern = process.env['TEST_PATTERN'];
  let globPattern = '**/*.test.cjs';

  if (testPattern) {
    const cleanPattern = testPattern.replace(/^test\//, '').replace(/\.tsx?$/, '.cjs');
    globPattern = cleanPattern.endsWith('.cjs') ? cleanPattern : `${cleanPattern}.cjs`;

    console.log('[suite] Running tests matching:', globPattern);
  }

  const mocha = new Mocha({ ui: 'bdd', color: true, timeout: 10000 });

  const files = await glob(globPattern, { cwd: testsRoot });
  for (const f of files) {
    mocha.addFile(path.resolve(testsRoot, f));
  }

  return new Promise<void>((resolve, reject) => {
    mocha.run((failures: number) => {
      if (failures > 0) {
        reject(new Error(`${failures} tests failed.`));
      } else {
        resolve();
      }
    });
  });
}

/**
 * RFC 011 packaging regression: the retired TypeScript parser worker must not
 * reappear in the production SDK distribution.
 */

import { test, describe } from 'node:test';
import assert from 'node:assert/strict';
import path from 'node:path';
import { existsSync, readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';

const distDir = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../../../dist');
const distIndex = path.join(distDir, 'index.js');
const distWorker = path.join(distDir, 'parse-worker.js');

describe('published worker script', () => {
  test('dist omits the repository-only parser worker', (t) => {
    if (!existsSync(distIndex)) {
      t.skip('dist/ not built — run `pnpm --filter @vibecook/spaghetti-sdk build` first');
      return;
    }
    assert.equal(existsSync(distWorker), false, `${distWorker} must not ship in the Rust-only production SDK`);
  });

  test('the worker is not inlined as a data: URL', (t) => {
    if (!existsSync(distIndex)) {
      t.skip('dist/ not built');
      return;
    }
    const bundle = readFileSync(distIndex, 'utf-8');
    // The inlined form is a `data:text/javascript;base64,` literal sitting
    // where the worker path should be. Its presence means the bundler
    // reclaimed the path expression as an asset reference again.
    assert.ok(
      !bundle.includes('data:text/javascript;base64,'),
      'dist/index.js inlines a script as a data: URL — the worker path was rewritten by the bundler, ' +
        'and fileURLToPath will throw ERR_INVALID_URL_SCHEME on it at runtime',
    );
  });
});

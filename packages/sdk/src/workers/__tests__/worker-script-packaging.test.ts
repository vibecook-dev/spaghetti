/**
 * WorkerPool — packaging regression coverage.
 *
 * The pool spawns `parse-worker.js` from beside whichever module is
 * running. That resolution is a *packaging* property, so the unit tests
 * (which run from src, where the file has always been present) could not
 * see it break — and it was broken for the entire life of the published
 * package in two compounding ways:
 *
 *   1. `vite build` emitted no worker script into dist at all, because
 *      only `index` and `react` were declared as library entries.
 *   2. The path was written `new URL('./parse-worker.js', import.meta.url)`,
 *      which is bundler syntax for an asset reference. In library mode vite
 *      resolved it by inlining the entire worker as a
 *      `data:text/javascript;base64,...` URL, and `fileURLToPath` throws
 *      ERR_INVALID_URL_SCHEME on a non-file scheme — so `createWorkerPool()`
 *      threw in its own constructor before any worker was spawned.
 *
 * Cold start catches that throw and falls back to sequential parsing
 * (see `LifecycleOwner.coldStartParallel`), which is why it stayed
 * invisible: correct output, quietly single-threaded, on every install.
 *
 * These assertions run against `dist/`, so they need a build. CI always
 * builds before `test:packages`; locally they skip with a note rather than
 * failing an unbuilt tree.
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
  test('dist ships a worker script beside the entry that looks for it', (t) => {
    if (!existsSync(distIndex)) {
      t.skip('dist/ not built — run `pnpm --filter @vibecook/spaghetti-sdk build` first');
      return;
    }
    assert.ok(
      existsSync(distWorker),
      `expected ${distWorker} to exist — WorkerPool resolves ./parse-worker.js relative to dist/index.js, ` +
        'so without this entry every parallel cold start falls back to sequential',
    );
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

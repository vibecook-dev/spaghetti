import { test, describe } from 'node:test';
import assert from 'node:assert';
import { initFailureHint } from '../index.js';

describe('initFailureHint', () => {
  // The regression: every init failure used to get "Install a supported agent",
  // including the one where the agent is installed and the native binding is
  // not. `npm i @vibecook/spaghetti` on npm 12 lands here, because npm 12
  // blocks install scripts by default and better-sqlite3 fetches its binding
  // from a postinstall.
  const bindingsError = [
    'Could not locate the bindings file. Tried:',
    ' → /app/node_modules/better-sqlite3/build/better_sqlite3.node',
    ' → /app/node_modules/better-sqlite3/build/Release/better_sqlite3.node',
  ].join('\n');

  test('missing native binding explains install scripts, not agents', () => {
    const hint = initFailureHint(bindingsError);
    assert.match(hint, /better-sqlite3/);
    assert.match(hint, /install script/i);
    assert.match(hint, /--allow-scripts=better-sqlite3/);
    assert.doesNotMatch(hint, /Install a supported agent/);
  });

  test('missing native binding names a pnpm remedy too', () => {
    assert.match(initFailureHint(bindingsError), /onlyBuiltDependencies/);
  });

  test('a genuinely missing corpus still points at the data roots', () => {
    const hint = initFailureHint('agent root dir not found or not a directory: /home/u/.claude');
    assert.match(hint, /Install a supported agent/);
    assert.match(hint, /~\/\.claude/);
    assert.doesNotMatch(hint, /better-sqlite3/);
  });

  test('an unrecognised failure suggests --verbose rather than guessing', () => {
    const hint = initFailureHint('SQLITE_CORRUPT: database disk image is malformed');
    assert.match(hint, /--verbose/);
    assert.doesNotMatch(hint, /Install a supported agent/);
    assert.doesNotMatch(hint, /better-sqlite3/);
  });
});

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

  // RFC 010 removed better-sqlite3, so the "bindings file" failure it used to
  // produce can no longer come from this package. The remedies `0.6.2`/`0.6.3`
  // printed are retired with it rather than left naming a dependency that is
  // gone — stale-but-plausible instructions are worse than none.
  test('a native binding failure no longer prescribes better-sqlite3 remedies', () => {
    const hint = initFailureHint(bindingsError);
    assert.match(hint, /native module failed to load/i);
    assert.doesNotMatch(hint, /better-sqlite3/, 'the dependency is gone; do not name it');
    assert.doesNotMatch(hint, /--allow-scripts/, 'no remedy for a dependency we no longer ship');
    assert.doesNotMatch(hint, /Install a supported agent/);
  });

  test('it does not speculate about a cause it has not confirmed', () => {
    // 0.6.2 shipped remedies that were never run and did not work. The lesson
    // generalises: say what happened, point at --verbose, and stop.
    const hint = initFailureHint(bindingsError);
    assert.match(hint, /--verbose/);
    assert.doesNotMatch(hint, /npm install|npm rebuild|allow-scripts/, 'no unverified remedy');
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

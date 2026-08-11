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
    assert.doesNotMatch(hint, /Install a supported agent/);
  });

  test('missing native binding names a pnpm remedy too', () => {
    assert.match(initFailureHint(bindingsError), /onlyBuiltDependencies/);
  });

  // `0.6.2` shipped two commands that had never been run. Both failed in ways
  // that resemble success — `--allow-scripts` on a project install is a hard
  // `EALLOWSCRIPTS` error, and `install-scripts approve` prints "Approved"
  // while running nothing. These assertions pin the forms that were actually
  // executed against a published build.
  test('the approve step is paired with the rebuild that executes it', () => {
    const hint = initFailureHint(bindingsError);
    const approve = hint.indexOf('npm install-scripts approve better-sqlite3');
    const rebuild = hint.indexOf('npm rebuild better-sqlite3');
    assert.ok(approve !== -1, 'must mention approve');
    assert.ok(rebuild !== -1, 'approve alone is a no-op — rebuild must be named');
    assert.ok(approve < rebuild, 'approve grants, rebuild executes — order matters');
  });

  test('the --allow-scripts form is only offered for a global install', () => {
    const hint = initFailureHint(bindingsError);
    // Project-scoped installs reject the flag outright, so any occurrence of it
    // has to carry `-g`.
    for (const line of hint.split('\n').filter((l) => l.includes('--allow-scripts'))) {
      assert.match(line, /npm install -g /, `flag offered without -g: ${line.trim()}`);
    }
  });

  test('project-scoped installs are told to use the manifest field', () => {
    assert.match(initFailureHint(bindingsError), /"allowScripts"/);
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

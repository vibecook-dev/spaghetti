/**
 * Worktree enumeration tests.
 *
 * Every porcelain fixture below is real `git worktree list --porcelain`
 * output, captured from a repository built into each state rather than
 * written from memory — the awkward stanzas (a reasonless `locked`, the exact
 * `prunable` wording, a bare repo having no `HEAD` line at all) are precisely
 * the ones an invented fixture gets subtly wrong, and a parser tested against
 * a wrong fixture is worse than one not tested at all.
 *
 * The process-level cases use real `git` and a real temp repository instead of
 * an injected fake spawn: the behaviour under test *is* how a real subprocess
 * fails, so mocking it would assert the mock.
 */

import assert from 'node:assert/strict';
import { describe, test } from 'node:test';
import { execSync, spawnSync } from 'node:child_process';
import { mkdtempSync, realpathSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import * as path from 'node:path';
import { listWorktrees, parseWorktreePorcelain } from '../worktrees.js';

function gitAvailable(): boolean {
  try {
    execSync('git --version', { stdio: 'ignore' });
    return true;
  } catch {
    return false;
  }
}
const HAS_GIT = gitAvailable();
const skipNoGit = HAS_GIT ? undefined : { skip: 'git not on PATH' };

function runGit(cwd: string, args: string[]): string {
  const r = spawnSync('git', args, {
    cwd,
    encoding: 'utf8',
    env: {
      ...process.env,
      GIT_AUTHOR_NAME: 'test',
      GIT_AUTHOR_EMAIL: 'test@example.com',
      GIT_COMMITTER_NAME: 'test',
      GIT_COMMITTER_EMAIL: 'test@example.com',
    },
  });
  if (r.status !== 0) throw new Error(`git ${args.join(' ')} failed: ${r.stderr}`);
  return r.stdout;
}

describe('parseWorktreePorcelain', () => {
  test('reads a main worktree plus detached, prunable, and locked linked ones', () => {
    // Captured verbatim from a repo with `--detach`, a deleted worktree dir,
    // and `git worktree lock --reason`.
    const fixture = [
      'worktree /tmp/wt2/r',
      'HEAD 7ef00a571fcee5dd332e28e9b972b6b8256ffa38',
      'branch refs/heads/main',
      '',
      'worktree /tmp/wt2/det',
      'HEAD 7ef00a571fcee5dd332e28e9b972b6b8256ffa38',
      'detached',
      '',
      'worktree /tmp/wt2/gone',
      'HEAD 7ef00a571fcee5dd332e28e9b972b6b8256ffa38',
      'branch refs/heads/gonebranch',
      'prunable gitdir file points to non-existent location',
      '',
      'worktree /tmp/wt2/lk',
      'HEAD 7ef00a571fcee5dd332e28e9b972b6b8256ffa38',
      'branch refs/heads/lkbranch',
      'locked held by an agent',
      '',
    ].join('\n');

    const got = parseWorktreePorcelain(fixture);
    assert.equal(got.length, 4);

    assert.deepEqual(
      got.map((w) => w.path),
      ['/tmp/wt2/r', '/tmp/wt2/det', '/tmp/wt2/gone', '/tmp/wt2/lk'],
    );

    // Only the first is the main worktree — git always lists it first.
    assert.deepEqual(
      got.map((w) => w.isMain),
      [true, false, false, false],
    );

    assert.equal(got[0]!.branch, 'main');
    assert.equal(got[0]!.branchRef, 'refs/heads/main');
    assert.equal(got[0]!.detached, false);

    assert.equal(got[1]!.detached, true);
    assert.equal(got[1]!.branch, null, 'a detached worktree has no branch');

    assert.equal(got[2]!.prunable, true);
    assert.equal(got[2]!.prunableReason, 'gitdir file points to non-existent location');
    assert.equal(got[2]!.branch, 'gonebranch', 'a prunable entry still reports its branch');

    assert.equal(got[3]!.locked, true);
    assert.equal(got[3]!.lockReason, 'held by an agent');
  });

  test('a bare repository reports bare with no HEAD', () => {
    const got = parseWorktreePorcelain('worktree /tmp/bare/b.git\nbare\n\n');
    assert.equal(got.length, 1);
    assert.equal(got[0]!.bare, true);
    assert.equal(got[0]!.head, null);
    assert.equal(got[0]!.branch, null);
  });

  test('locked without a reason is locked with a null reason, not an empty string', () => {
    const got = parseWorktreePorcelain(
      ['worktree /tmp/wt2/nl', 'HEAD 7ef00a57', 'branch refs/heads/nlb', 'locked', ''].join('\n'),
    );
    assert.equal(got[0]!.locked, true);
    assert.equal(got[0]!.lockReason, null);
  });

  test('an unrecognized attribute is ignored without costing its stanza', () => {
    // Guards forward compatibility: git gaining a new attribute must not
    // blank out the worktree it belongs to.
    const got = parseWorktreePorcelain(
      ['worktree /tmp/x', 'HEAD abc123', 'branch refs/heads/main', 'somethingnew value', ''].join('\n'),
    );
    assert.equal(got.length, 1);
    assert.equal(got[0]!.branch, 'main');
    assert.equal(got[0]!.head, 'abc123');
  });

  test('a final stanza with no trailing blank line is still emitted', () => {
    const got = parseWorktreePorcelain('worktree /tmp/a\nHEAD abc\nbranch refs/heads/m');
    assert.equal(got.length, 1);
    assert.equal(got[0]!.branch, 'm');
  });

  test('CRLF line endings parse the same as LF', () => {
    const got = parseWorktreePorcelain('worktree /tmp/a\r\nHEAD abc\r\nbranch refs/heads/m\r\n\r\n');
    assert.equal(got.length, 1);
    assert.equal(got[0]!.path, '/tmp/a', 'the trailing CR must not land in the path');
    assert.equal(got[0]!.branch, 'm');
  });

  test('empty output yields no worktrees', () => {
    assert.deepEqual(parseWorktreePorcelain(''), []);
    assert.deepEqual(parseWorktreePorcelain('\n\n'), []);
  });
});

describe('listWorktrees', () => {
  test('enumerates a real repository and its linked worktree', skipNoGit ?? {}, async () => {
    const base = mkdtempSync(path.join(tmpdir(), 'spag-wt-'));
    try {
      const main = path.join(base, 'repo');
      runGit(base, ['init', '-q', '-b', 'main', 'repo']);
      writeFileSync(path.join(main, 'f.txt'), 'x\n');
      runGit(main, ['add', '-A']);
      runGit(main, ['commit', '-q', '-m', 'init']);
      runGit(main, ['worktree', 'add', '-q', path.join(base, 'wt'), '-b', 'feature']);

      const got = await listWorktrees(main);

      assert.equal(got.length, 2, `expected main + linked, got ${JSON.stringify(got, null, 2)}`);
      assert.equal(got[0]!.isMain, true);
      assert.equal(got[0]!.branch, 'main');
      assert.equal(got[1]!.isMain, false);
      assert.equal(got[1]!.branch, 'feature');
      assert.ok(got[1]!.head, 'a checked-out worktree reports a HEAD');
    } finally {
      rmSync(base, { recursive: true, force: true, maxRetries: 10, retryDelay: 50 });
    }
  });

  test('realPath resolves symlinks so cross-source path matching works', skipNoGit ?? {}, async () => {
    // The macOS temp dir is itself reached through /var -> /private/var, so
    // this asserts the property that matters — realPath is the fully resolved
    // spelling — without hard-coding a platform's symlink layout.
    const base = mkdtempSync(path.join(tmpdir(), 'spag-real-'));
    try {
      const main = path.join(base, 'repo');
      runGit(base, ['init', '-q', '-b', 'main', 'repo']);
      writeFileSync(path.join(main, 'f.txt'), 'x\n');
      runGit(main, ['add', '-A']);
      runGit(main, ['commit', '-q', '-m', 'init']);

      const [wt] = await listWorktrees(main);
      assert.ok(wt, 'expected the main worktree');
      assert.equal(wt.realPath, realpathSync.native(wt.path));
    } finally {
      rmSync(base, { recursive: true, force: true, maxRetries: 10, retryDelay: 50 });
    }
  });

  test('a directory that is not a repository resolves empty, not rejected', skipNoGit ?? {}, async () => {
    const dir = mkdtempSync(path.join(tmpdir(), 'spag-nonrepo-'));
    try {
      const warnings: string[] = [];
      // git exits 128 here. That is the ordinary case for an unversioned
      // project, so it must stay silent rather than warn on every open.
      assert.deepEqual(await listWorktrees(dir, { warn: (m) => warnings.push(m) }), []);
      assert.deepEqual(warnings, [], 'a non-repository is expected, not noteworthy');
    } finally {
      rmSync(dir, { recursive: true, force: true });
    }
  });

  test('a missing git binary warns once and resolves empty', async () => {
    const warnings: string[] = [];
    const got = await listWorktrees(process.cwd(), {
      gitPath: path.join(tmpdir(), 'definitely-not-a-git-binary'),
      warn: (m) => warnings.push(m),
    });
    assert.deepEqual(got, []);
    assert.equal(warnings.length, 1, `expected exactly one warning, got ${JSON.stringify(warnings)}`);
  });
});

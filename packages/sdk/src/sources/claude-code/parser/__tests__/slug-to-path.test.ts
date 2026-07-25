/**
 * slug-to-path.test.ts — Decoding `~/.claude/projects/<slug>` back into
 * the absolute cwd it was derived from.
 *
 * Claude Code encodes the project cwd by replacing every path separator
 * with `-`, which is lossy in two ways:
 *
 *   1. Hyphens that belong to a directory name are indistinguishable
 *      from hyphens that encode a separator (`p100-app` vs `p100/app`).
 *      Resolved by probing the filesystem, longest-match-first.
 *   2. On Windows the drive colon is folded too, so `D:\Projects\app`
 *      becomes `D--Projects-app`. Older Claude Code builds — and the
 *      Codex/Grok readers today — keep the colon (`D:-Projects-app`).
 *      Both spellings must decode to `D:\Projects\app`.
 *
 * Getting (2) wrong is not cosmetic: `spag sessions .` matches
 * `project.absolutePath` against `process.cwd()` verbatim, so a mangled
 * path makes every cwd-scoped command fail to find its own project.
 *
 * These assertions are platform-independent by design — a synced
 * `~/.claude` must decode identically on any host — so the Windows cases
 * are expected to hold on POSIX CI runners too. Keep in sync with the
 * `slug_to_path_*` tests in
 * `crates/spaghetti-napi/src/claude/project_parser.rs`.
 *
 * Run with `pnpm --filter @vibecook/spaghetti-sdk test`.
 */

import { test, describe, before, after } from 'node:test';
import assert from 'node:assert/strict';
import os from 'node:os';
import path from 'node:path';
import { mkdtempSync, rmSync, mkdirSync } from 'node:fs';

import { createFileService } from '../../../../io/file-service.js';
import { createProjectParser, slugShape } from '../project-parser.js';

// ═══════════════════════════════════════════════════════════════════════════
// slugShape — the pure prefix/separator classifier
// ═══════════════════════════════════════════════════════════════════════════

describe('slugShape', () => {
  test('reads a leading dash as a POSIX absolute path', () => {
    assert.deepEqual(slugShape('-Users-me-app'), { prefix: '/', sep: '/', rest: 'Users-me-app' });
  });

  test('reads <letter>-- as a Windows drive (colon folded to dash)', () => {
    assert.deepEqual(slugShape('D--Projects-app'), { prefix: 'D:\\', sep: '\\', rest: 'Projects-app' });
  });

  test('reads <letter>:- as a Windows drive (colon preserved)', () => {
    assert.deepEqual(slugShape('D:-Projects-app'), { prefix: 'D:\\', sep: '\\', rest: 'Projects-app' });
  });

  test('normalizes the drive letter to uppercase', () => {
    assert.equal(slugShape('d--Projects-app').prefix, 'D:\\');
  });

  test('treats a bare drive root as prefix-only', () => {
    assert.deepEqual(slugShape('D--'), { prefix: 'D:\\', sep: '\\', rest: '' });
    assert.deepEqual(slugShape('D:-'), { prefix: 'D:\\', sep: '\\', rest: '' });
  });

  test('lets a leading dash win over a drive-shaped tail', () => {
    // `/d--Projects/app` is a legal POSIX path; it must not be mistaken
    // for drive D.
    assert.deepEqual(slugShape('-d--Projects-app'), { prefix: '/', sep: '/', rest: 'd--Projects-app' });
  });

  test('leaves an unrecognized slug relative', () => {
    assert.deepEqual(slugShape('Users-me'), { prefix: '', sep: '/', rest: 'Users-me' });
  });
});

// ═══════════════════════════════════════════════════════════════════════════
// Full decode via the parser
// ═══════════════════════════════════════════════════════════════════════════

describe('ProjectParser originalPath', () => {
  let tmpRoot: string;

  before(() => {
    tmpRoot = mkdtempSync(path.join(os.tmpdir(), 'spag-slug-'));
  });

  after(() => {
    rmSync(tmpRoot, { recursive: true, force: true });
  });

  /**
   * Decode a slug through the real parser.
   *
   * No project directory is created: `parseProject` tolerates a missing
   * one, and the colon spelling *cannot* be a directory on Windows (NTFS
   * forbids `:` in names). That spelling only ever reaches the decoder as
   * a logical slug — the Codex and Grok readers derive theirs from the
   * rollout's recorded `cwd` rather than from a directory listing.
   */
  function decode(slug: string): string {
    const parser = createProjectParser(createFileService());
    const project = parser.parseProject(tmpRoot, slug);
    assert.ok(project, `parseProject returned null for ${slug}`);
    return project.originalPath;
  }

  // No sessions-index.json is written, so `originalPath` falls through to
  // the slug decoder — which is the path under test. The encoded dirs
  // don't exist on this host either, so every probe misses and we get the
  // naive one-segment-per-dash decode.
  test('decodes a POSIX slug', () => {
    assert.equal(decode('-Users-me-Projects-app'), '/Users/me/Projects/app');
  });

  test('decodes a Windows slug with the colon folded', () => {
    assert.equal(decode('D--Projects-p100-spaghetti'), 'D:\\Projects\\p100\\spaghetti');
  });

  test('decodes a Windows slug with the colon preserved', () => {
    assert.equal(decode('D:-I3T-WordplayAR'), 'D:\\I3T\\WordplayAR');
  });

  test('decodes both Windows spellings of one cwd identically', () => {
    // The regression that split a single repo into two projects: Claude
    // writes `C--Users-me`, the Codex/Grok readers write `C:-Users-me`.
    assert.equal(decode('C--Users-me'), decode('C:-Users-me'));
  });

  test('decodes a bare Windows drive root', () => {
    assert.equal(decode('D--'), 'D:\\');
  });

  test('probes the filesystem to keep hyphenated directory names intact', () => {
    // Build a real nested dir whose name contains a hyphen, re-encode it
    // the way Claude Code would, and expect a round trip. Without the
    // probe, `p100-app` would split into `p100/app`.
    const nested = path.join(tmpRoot, 'p100-app', 'sub');
    mkdirSync(nested, { recursive: true });
    const slug = nested.replaceAll(path.sep, '-').replace(':-', '--');
    assert.equal(decode(slug), nested);
  });
});

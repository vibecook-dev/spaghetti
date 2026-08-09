/**
 * fingerprint-source-ownership.test.ts — the v16 `(source_id, path)` key.
 *
 * Sibling of `composite-source-pk.test.ts`, which covers the same class of bug
 * for `projects`. `source_files` stored `source_id` from the start but never
 * queried it, so with a `path` primary key:
 *
 *   - two sources holding the same absolute path shared one row, and
 *   - `deleteFingerprint` from one source removed the other's, and
 *   - `getAllFingerprints` handed each source every other source's files.
 *
 * That last one is not theoretical: the Codex and Grok lifecycle owners iterate
 * `getAllFingerprints()` as "the files I ingested", and the Rust warm-unchanged
 * check did the same.
 *
 * Run with `pnpm --filter @vibecook/spaghetti-sdk test`.
 */

import { test, describe, before, after } from 'node:test';
import assert from 'node:assert/strict';
import os from 'node:os';
import path from 'node:path';
import { mkdtempSync, rmSync } from 'node:fs';

import { createSqliteService } from '../../io/sqlite-service.js';
import { createIngestService } from '../ingest-service.js';
import { initializeSchema } from '../schema.js';
import type { SqliteService } from '../../io/index.js';
import type { IngestService } from '../ingest-service.js';
import type { SourceFingerprint } from '../segment-types.js';

let tmpDir: string;
let dbPath: string;
let db: SqliteService;
let claude: IngestService;
let codex: IngestService;
let grok: IngestService;

/** All three services share one connection, exactly as the real app does. */
function serviceFor(sourceId: string): IngestService {
  const svc = createIngestService(() => db, { sourceId });
  svc.open(dbPath);
  return svc;
}

before(() => {
  tmpDir = mkdtempSync(path.join(os.tmpdir(), 'spag-fp-owner-'));
  dbPath = path.join(tmpDir, 'index.db');
  db = createSqliteService();
  db.open({ path: dbPath });
  initializeSchema(db);

  claude = serviceFor('claude-code');
  codex = serviceFor('codex');
  grok = serviceFor('grok');
});

after(() => {
  try {
    db.close();
  } catch {
    /* already closed */
  }
  rmSync(tmpDir, { recursive: true, force: true });
});

function fp(p: string, size: number): SourceFingerprint {
  return { path: p, mtimeMs: 1_700_000_000_000, size };
}

describe('source_files ownership is per (source_id, path)', () => {
  test('the same path under two sources is two rows, not one', () => {
    // A path both agents could plausibly hold — a shared repo checkout.
    const shared = '/repo/shared/transcript.jsonl';
    claude.upsertFingerprint(fp(shared, 100));
    codex.upsertFingerprint(fp(shared, 999));

    const fromClaude = claude.getFingerprint(shared);
    const fromCodex = codex.getFingerprint(shared);

    assert.equal(fromClaude?.size, 100, 'claude must see its own row');
    assert.equal(fromCodex?.size, 999, 'codex must see its own row');
  });

  test('deleting from one source leaves the other intact', () => {
    const shared = '/repo/shared/delete-me.jsonl';
    claude.upsertFingerprint(fp(shared, 10));
    codex.upsertFingerprint(fp(shared, 20));

    claude.deleteFingerprint(shared);

    assert.equal(claude.getFingerprint(shared), null);
    assert.equal(codex.getFingerprint(shared)?.size, 20, "codex's row must survive claude's delete");
  });

  test('getAllFingerprints returns only the calling source', () => {
    // Paths unique to this test, so the assertion is about scoping and not
    // about what earlier tests happened to leave behind.
    const claudeOnly = '/scoped/claude-only.jsonl';
    const codexOnly = '/scoped/codex-only.jsonl';
    const grokOnly = '/scoped/grok-only.jsonl';

    claude.upsertFingerprint(fp(claudeOnly, 1));
    codex.upsertFingerprint(fp(codexOnly, 2));
    grok.upsertFingerprint(fp(grokOnly, 3));

    const seenBy = (svc: IngestService): Set<string> => new Set(svc.getAllFingerprints().map((f) => f.path));
    const byClaude = seenBy(claude);
    const byCodex = seenBy(codex);
    const byGrok = seenBy(grok);

    // Each source sees its own row and neither of the others'.
    assert.ok(byClaude.has(claudeOnly));
    assert.ok(!byClaude.has(codexOnly) && !byClaude.has(grokOnly));

    assert.ok(byCodex.has(codexOnly));
    assert.ok(!byCodex.has(claudeOnly) && !byCodex.has(grokOnly));

    assert.ok(byGrok.has(grokOnly));
    assert.ok(!byGrok.has(claudeOnly) && !byGrok.has(codexOnly));

    // And the three views sum to the whole table — nothing is invisible to
    // every source, which a wrong scope predicate could otherwise cause.
    const total = db.get<{ count: number }>('SELECT COUNT(*) AS count FROM source_files');
    assert.equal(byClaude.size + byCodex.size + byGrok.size, total?.count);
  });

  test('an upsert updates its own row rather than inserting a duplicate', () => {
    const p = '/repo/shared/upsert.jsonl';
    claude.upsertFingerprint(fp(p, 1));
    claude.upsertFingerprint(fp(p, 2));
    claude.upsertFingerprint(fp(p, 3));

    assert.equal(claude.getFingerprint(p)?.size, 3);
    const row = db.get<{ count: number }>(
      'SELECT COUNT(*) AS count FROM source_files WHERE source_id = ? AND path = ?',
      'claude-code',
      p,
    );
    assert.equal(row?.count, 1);
  });

  test('roots whose names are string prefixes do not bleed into each other', () => {
    // The failure mode a `starts_with(root)` ownership scheme would have: with
    // path-prefix matching, `/agents/a.jsonl` looks like it belongs to the
    // `/agent` root. Keying by source_id makes prefixes irrelevant, which is
    // the point — this asserts we never regress to path-derived ownership.
    codex.upsertFingerprint(fp('/agent/a.jsonl', 1));
    grok.upsertFingerprint(fp('/agent-old/a.jsonl', 2));
    claude.upsertFingerprint(fp('/agents/a.jsonl', 3));

    assert.equal(codex.getFingerprint('/agent/a.jsonl')?.size, 1);
    assert.equal(codex.getFingerprint('/agent-old/a.jsonl'), null);
    assert.equal(codex.getFingerprint('/agents/a.jsonl'), null);
    assert.equal(grok.getFingerprint('/agent-old/a.jsonl')?.size, 2);
    assert.equal(claude.getFingerprint('/agents/a.jsonl')?.size, 3);
  });

  test('paths differing only by separator or case stay distinct rows', () => {
    // Not a claim that these are the same file — the opposite. Whatever the
    // caller stored is what it gets back, so normalisation stays the caller's
    // job and this layer never silently merges two paths.
    const back = 'C:\\repo\\Shared\\a.jsonl';
    const fwd = 'C:/repo/Shared/a.jsonl';
    const lower = 'c:\\repo\\shared\\a.jsonl';

    claude.upsertFingerprint(fp(back, 11));
    claude.upsertFingerprint(fp(fwd, 22));
    claude.upsertFingerprint(fp(lower, 33));

    assert.equal(claude.getFingerprint(back)?.size, 11);
    assert.equal(claude.getFingerprint(fwd)?.size, 22);
    assert.equal(claude.getFingerprint(lower)?.size, 33);
  });

  test('clearing one source drops only its fingerprints', () => {
    const c2 = serviceFor('claude-code');
    claude.upsertFingerprint(fp('/clear/claude.jsonl', 1));
    codex.upsertFingerprint(fp('/clear/codex.jsonl', 2));

    c2.clearSourceData();

    assert.equal(claude.getFingerprint('/clear/claude.jsonl'), null);
    assert.equal(codex.getFingerprint('/clear/codex.jsonl')?.size, 2, 'codex survives a claude clear');
  });
});

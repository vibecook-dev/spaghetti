/**
 * ingest-error-report.test.ts — what a user is told when an ingest partly fails.
 *
 * The addon caps its error list at 100 but keeps counting, so the summary has
 * one job it must not get wrong: never present the capped list as the whole
 * story. "3 issues" and "3 shown of 4,000" call for different reactions, and
 * conflating them is precisely the under-reporting the cap exists to avoid.
 *
 * Run with `pnpm --filter @vibecook/spaghetti-sdk test`.
 */

import { test, describe } from 'node:test';
import assert from 'node:assert/strict';

import { reportIngestErrors, summarizeIngestErrors } from '../ingest-error-report.js';
import type { ErrorSink, ErrorContext } from '../../io/error-sink.js';
import type { NativeIngestError, NativeIngestErrorReport } from '../../legacy-native.js';

function err(path: string, severity: NativeIngestError['severity'], slug?: string): NativeIngestError {
  return { path, severity, message: 'boom', ...(slug === undefined ? {} : { slug }) };
}

function report(errors: NativeIngestError[], total = errors.length): NativeIngestErrorReport {
  return { errors, errorCount: total, errorsTruncated: total > errors.length };
}

function recordingSink(): { sink: ErrorSink; calls: Array<{ err: Error; ctx?: ErrorContext }> } {
  const calls: Array<{ err: Error; ctx?: ErrorContext }> = [];
  return {
    calls,
    sink: {
      error(e, ctx) {
        calls.push({ err: e, ctx });
      },
    },
  };
}

describe('summarizeIngestErrors', () => {
  test('a clean report produces nothing to say', () => {
    assert.equal(summarizeIngestErrors('claude-code', report([])), null);
  });

  test('names the affected files', () => {
    const s = summarizeIngestErrors('claude-code', report([err('/a/one.jsonl', 'record-skip', 'p')]));
    assert.match(s!, /\/a\/one\.jsonl/);
    assert.match(s!, /claude-code/);
  });

  test('reports the uncapped total, not the length of the capped list', () => {
    // The failure mode this guards: 100 shown out of 4,000 reading as "100".
    const shown = Array.from({ length: 100 }, (_, i) => err(`/f/${i}.jsonl`, 'record-skip', 'p'));
    const s = summarizeIngestErrors('claude-code', report(shown, 4000))!;
    assert.match(s, /4000 ingest issue\(s\)/);
    assert.doesNotMatch(s, /\b100 ingest issue/);
  });

  test('collapses the unnamed remainder into a count', () => {
    const shown = Array.from({ length: 10 }, (_, i) => err(`/f/${i}.jsonl`, 'record-skip', 'p'));
    const s = summarizeIngestErrors('claude-code', report(shown, 10))!;
    assert.match(s, /\(\+5 more\)/, 'names five, counts the rest');
  });

  test('distinguishes the severities, since they mean different damage', () => {
    const s = summarizeIngestErrors(
      'claude-code',
      report([err('/a.jsonl', 'record-skip', 'p'), err('/b', 'project-fatal', 'p'), err('/c', 'source')]),
    )!;
    assert.match(s, /1 project\(s\) rolled back/);
    assert.match(s, /1 unreadable location\(s\)/);
    assert.match(s, /1 record\(s\) skipped/);
  });

  test('says the affected inputs will retry, because they will', () => {
    // Not cosmetic: the fingerprint for a failed input is withheld precisely
    // so the next run re-reads it. Telling the user to intervene would be wrong.
    const s = summarizeIngestErrors('grok', report([err('/a', 'source')]))!;
    assert.match(s, /retried on the next run/);
  });
});

describe('reportIngestErrors', () => {
  test('a clean run does not touch the sink', () => {
    const { sink, calls } = recordingSink();
    reportIngestErrors('claude-code', report([]), sink);
    assert.equal(calls.length, 0);
  });

  test('routes the summary with structured context', () => {
    const { sink, calls } = recordingSink();
    reportIngestErrors('codex', report([err('/a.jsonl', 'record-skip', 'p')], 7), sink);

    assert.equal(calls.length, 1);
    assert.equal(calls[0].ctx?.component, 'NativeIngest');
    assert.equal(calls[0].ctx?.sourceId, 'codex');
    assert.equal(calls[0].ctx?.errorCount, 7);
    assert.equal(calls[0].ctx?.errorsTruncated, true);
    assert.deepEqual(calls[0].ctx?.errors, [err('/a.jsonl', 'record-skip', 'p')]);
  });

  test('a missing sink is survivable, not a crash', () => {
    // Ingest already succeeded by the time this runs; failing to report must
    // never turn a partial success into a thrown error.
    assert.doesNotThrow(() => reportIngestErrors('grok', report([err('/a', 'source')]), undefined));
  });
});

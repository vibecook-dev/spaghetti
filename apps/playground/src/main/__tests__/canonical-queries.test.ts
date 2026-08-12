import assert from 'node:assert/strict';
import { test } from 'node:test';
import type { SpaghettiClient, SpaghettiClientResponseMap } from '@vibecook/spaghetti-sdk/client';
import { readCanonicalStats } from '../canonical-queries.js';

test('canonical stats dispatches exactly once through the shared SpaghettiClient', async () => {
  const expected: SpaghettiClientResponseMap['getStats'] = {
    contractVersion: 1,
    atCommitSeq: 17,
    schemaVersion: 4,
    sourceInstances: 1,
    sourceStreams: 2,
    sourceObjects: 3,
    activeSourceObjects: 3,
    sourceRecordErrors: 0,
    ingestCommits: 17,
    factRecords: 40,
    searchableMessages: 8,
    entities: [{ name: 'session', count: 2 }],
    sourceStreamStates: [{ name: 'ready', count: 2 }],
    projectionReadiness: [{ name: 'ready', count: 3 }],
    databasePageCount: 12,
    databasePageSizeBytes: 4096,
    allocatedDatabaseBytes: 49_152,
  };
  let opens = 0;
  let calls = 0;
  const provider = {
    async getObservationClient() {
      opens += 1;
      return {
        async getStats() {
          calls += 1;
          return expected;
        },
      } as SpaghettiClient;
    },
  };

  assert.equal(await readCanonicalStats(provider), expected);
  assert.equal(opens, 1);
  assert.equal(calls, 1);
});

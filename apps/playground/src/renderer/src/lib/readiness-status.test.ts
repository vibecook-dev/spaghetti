import assert from 'node:assert/strict';
import test from 'node:test';

import type { SpaghettiReadiness, SpaghettiReadinessField } from '@vibecook/spaghetti-sdk/observation';
import { readinessIndicator, readinessIsConverging } from './readiness-status.js';

function field(state: SpaghettiReadinessField['state'] = 'ready', detail?: string): SpaghettiReadinessField {
  return { state, committedAtSeq: 9, ...(detail ? { detail } : {}) };
}

function readiness(overrides: Partial<SpaghettiReadiness> = {}): SpaghettiReadiness {
  return {
    catalog: field(),
    history: field(),
    usage: field(),
    capabilities: field(),
    artifacts: field(),
    search: field(),
    atCommitSeq: 9,
    ...overrides,
  };
}

test('the indicator disappears only when all six fields are ready', () => {
  const ready = readiness();
  assert.equal(readinessIsConverging(ready), false);
  assert.deepEqual(readinessIndicator(ready), [null, null]);
});

test('capabilities and artifacts keep readiness polling active', () => {
  const value = readiness({ capabilities: field('pending'), artifacts: field('indexing', '2 of 8') });
  assert.equal(readinessIsConverging(value), true);
  assert.deepEqual(readinessIndicator(value), ['indexing…', 'capabilities: pending\nartifacts: 2 of 8']);
});

test('unavailable fields are reported with any work still converging', () => {
  const value = readiness({ artifacts: field('unavailable', 'source denied access'), search: field('indexing') });
  assert.equal(readinessIsConverging(value), true);
  assert.deepEqual(readinessIndicator(value), ['degraded', 'artifacts: source denied access\nsearch: indexing']);
});

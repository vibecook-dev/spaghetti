import assert from 'node:assert/strict';
import { describe, test } from 'node:test';

import { applyProgressEvent, initialSourceStates } from './source-progress.js';

describe('structured Rust observation progress', () => {
  test('starts the first configured source instead of showing every row as waiting', () => {
    const sources = initialSourceStates();
    assert.equal(sources[0]?.stage, 'active');
    assert.ok(sources.slice(1).every((source) => source.stage === 'pending'));
  });

  test('uses source identity and stage without parsing human-readable text', () => {
    const started = applyProgressEvent(initialSourceStates(), {
      phase: 'parsing',
      message: 'A localized status string with no adapter name',
      sourceId: 'codex',
      sourceStage: 'active',
      sourceIndex: 2,
      sourceCount: 3,
    });
    assert.deepEqual(
      started.map(({ id, stage }) => ({ id, stage })),
      [
        { id: 'claude-code', stage: 'done' },
        { id: 'codex', stage: 'active' },
        { id: 'grok', stage: 'pending' },
      ],
    );

    const completed = applyProgressEvent(started, {
      phase: 'indexing',
      message: 'Still intentionally unattributed',
      sourceId: 'codex',
      sourceStage: 'done',
      sourceIndex: 2,
      sourceCount: 3,
    });
    assert.equal(completed[1]?.stage, 'done');
    assert.equal(completed[1]?.fraction, 1);
  });
});

import assert from 'node:assert/strict';
import { afterEach, describe, test } from 'node:test';

import { resolveActiveEngine } from '../native.js';
import { resolveEngine } from '../settings.js';

describe('RFC 011 production engine resolution', () => {
  const savedEngine = process.env.SPAG_ENGINE;
  const savedLegacy = process.env.SPAG_NATIVE_INGEST;

  afterEach(() => {
    if (savedEngine === undefined) delete process.env.SPAG_ENGINE;
    else process.env.SPAG_ENGINE = savedEngine;
    if (savedLegacy === undefined) delete process.env.SPAG_NATIVE_INGEST;
    else process.env.SPAG_NATIVE_INGEST = savedLegacy;
  });

  test('legacy environment switches cannot revive the TypeScript owner', () => {
    process.env.SPAG_ENGINE = 'ts';
    process.env.SPAG_NATIVE_INGEST = '0';
    assert.equal(resolveEngine(), 'rs');
    assert.equal(resolveActiveEngine().engine, 'rs');
    assert.equal(resolveActiveEngine().preference, 'rs');
  });

  test('addon availability is diagnostic and never changes engine identity', () => {
    const info = resolveActiveEngine();
    assert.equal(info.engine, 'rs');
    assert.equal(typeof info.nativeAvailable, 'boolean');
    if (info.nativeAvailable) assert.equal(typeof info.nativeVersion, 'string');
    else assert.equal(info.nativeVersion, null);
  });
});

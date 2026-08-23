/**
 * VibeField Phase A surface (RFC 012 landing plan §3.2).
 *
 * These are parity tests in the sense the lane brief keeps: they run real Rust
 * and check that the *generated* TypeScript types describe what it actually
 * emits. There is no hand-written parser under test, so the assertions are
 * about native output, not about a second implementation of it.
 */

import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import { readFileSync, mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { after, describe, test } from 'node:test';

import {
  isSameEntity,
  isSameNativeIdentity,
  isSameRevision,
  isSameSnapshot,
  queryWatermark,
  type ExternalEntityRef,
  type NativeIdentity,
  type ProjectRef,
  type SemanticRevisionRef,
  type SessionRef,
} from '../vibefield.js';
import { loadNativeAddon, openSpaghettiEngine, type SpaghettiEngine } from '../index.js';

interface NativeContractAddon {
  parseRfc012aV1Json: (json: string) => string;
}

const native = loadNativeAddon();
const contracts = createRequire(import.meta.url)('@vibecook/spaghetti-sdk-native') as NativeContractAddon;

/** Rust owns this fixture; it is the same one the native helper round-trips. */
const fixtureJson = readFileSync(
  new URL('../../../../crates/spaghetti-napi/fixtures/contracts/rfc012a-v1.json', import.meta.url),
  'utf8',
);

/**
 * Rust's own serialization of the fixture. Assigning it to the generated types
 * is the parity check: if `ExternalEntityRef` in Rust grew or renamed a field
 * without regenerating, this file stops compiling.
 */
function nativeFixture(): {
  external_entity_ref: ExternalEntityRef;
  semantic_revision_ref: SemanticRevisionRef;
  native_identity_claim: { entity_ref: ExternalEntityRef };
} {
  return JSON.parse(contracts.parseRfc012aV1Json(fixtureJson));
}

describe('VibeField Phase A identity surface', { skip: !native }, () => {
  test('generated types describe what Rust actually serializes', () => {
    const parsed = nativeFixture();

    const session: SessionRef = parsed.external_entity_ref;
    const project: ProjectRef = parsed.external_entity_ref;
    const revision: SemanticRevisionRef = parsed.semantic_revision_ref;

    // Field names are the JSON wire names, because ts-rs reads the serde
    // attributes rather than guessing a casing convention.
    assert.equal(typeof session.external_entity_reference_version, 'number');
    assert.equal(typeof session.entity_key, 'string');
    assert.equal(typeof revision.semantic_reference_contract_version, 'number');
    assert.equal(typeof revision.fact_revision_id, 'string');

    // Opaque references carry the encoding version Rust stamps on them and
    // never leak a local path or database name.
    assert.match(session.entity_key, /^v1:[A-Za-z0-9_-]+$/);
    assert.match(revision.fact_revision_id, /^v1:[A-Za-z0-9_-]+$/);

    assert.ok(isSameEntity(session, project));
  });

  test('reference comparison is by value and keeps version mismatches distinct', () => {
    const parsed = nativeFixture();
    const ref = parsed.external_entity_ref;
    const decodedAgain: ExternalEntityRef = nativeFixture().external_entity_ref;

    // Two independent decodings are never `===`, which is the whole reason
    // these helpers exist.
    assert.notEqual(ref, decodedAgain);
    assert.ok(isSameEntity(ref, decodedAgain));

    const otherEntity: ExternalEntityRef = { ...ref, entity_key: `${ref.entity_key}x` };
    assert.equal(isSameEntity(ref, otherEntity), false);

    const futureVersion: ExternalEntityRef = {
      ...ref,
      external_entity_reference_version: ref.external_entity_reference_version + 1,
    };
    assert.equal(
      isSameEntity(ref, futureVersion),
      false,
      'references minted under different contract versions are not the same reference',
    );

    const revision = parsed.semantic_revision_ref;
    assert.ok(isSameRevision(revision, { ...revision }));
    assert.equal(isSameRevision(revision, { ...revision, fact_revision_id: 'v1:other' }), false);
  });

  test('native identity is namespaced', () => {
    const left: NativeIdentity = { native_namespace: 'claude-code', native_id: 'session-1' };
    const right: NativeIdentity = { native_namespace: 'codex', native_id: 'session-1' };
    assert.ok(isSameNativeIdentity(left, { ...left }));
    assert.equal(isSameNativeIdentity(left, right), false, 'the same id from two products is two identities');
  });
});

describe('VibeField Phase A durable watermark', { skip: !native }, () => {
  const engines: SpaghettiEngine[] = [];
  const tempDirs: string[] = [];

  after(async () => {
    for (const engine of engines.splice(0)) await engine.dispose();
    for (const dir of tempDirs.splice(0)) rmSync(dir, { recursive: true, force: true, maxRetries: 10, retryDelay: 50 });
  });

  test('every durable query result reports the snapshot it was computed at', async () => {
    const dir = mkdtempSync(path.join(tmpdir(), 'spaghetti-vibefield-'));
    tempDirs.push(dir);
    const engine = await openSpaghettiEngine({
      dbPath: path.join(dir, 'spaghetti.db'),
      ownerLabel: 'vibefield-watermark-test',
      queryWorkers: 1,
    });
    engines.push(engine);

    const projects = await engine.listHistoryProjects();
    const sources = await engine.listSources();
    const overview = await engine.overview();

    assert.equal(typeof queryWatermark(projects), 'number');
    assert.equal(
      queryWatermark(projects),
      overview.commitSeq,
      'the watermark is the durable commit sequence, not a page-local counter',
    );

    // Two reads with no write between them are the same snapshot, so their
    // pages may be joined without re-reading either.
    assert.ok(isSameSnapshot(projects, sources));

    // The watermark type is structural, so any durable result satisfies it.
    assert.equal(queryWatermark(sources), queryWatermark(projects));
  });
});

import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';

import { parseScopedObservationKnownEnvelope } from '../rfc012d-known-envelope.js';

type JsonObject = Record<string, any>;

function fixture(name: string): JsonObject {
  return JSON.parse(
    readFileSync(new URL(`../../../../../crates/spaghetti-napi/fixtures/contracts/${name}`, import.meta.url), 'utf8'),
  ) as JsonObject;
}

function clone<T>(value: T): T {
  return JSON.parse(JSON.stringify(value)) as T;
}

const usage = fixture('rfc012d-scoped-usage-envelope-v1.json');
const actor = fixture('rfc012d-scoped-actor-envelope-v1.json');
const source = fixture('rfc012d-scoped-source-envelope-v1.json');
const availability = fixture('rfc012d-scoped-artifact-availability-envelope-v1.json');
const completion = fixture('rfc012d-scoped-completion-envelope-v1.json');
const continuity = fixture('rfc012d-scoped-continuity-envelope-v1.json');
const outer = fixture('rfc012d-scoped-known-envelope-v1.json');

function wrapped(family: string, context: unknown, event: unknown): JsonObject {
  return {
    scoped_known_envelope_contract_version: 1,
    family,
    context,
    event,
  };
}

test('known RFC 012D families dispatch through their strict contextual parsers', () => {
  const cases = [
    wrapped('usage', usage.context, usage.upsert),
    wrapped('actor', actor.context, actor.actor_upsert),
    wrapped('source', source.context, source.created),
    wrapped('artifact_availability', availability.context, availability.event),
    wrapped('completion', completion.bootstrap.context, completion.bootstrap.event),
    wrapped('continuity', continuity.contexts.required, continuity.resync_required.watcher_overflow),
  ];
  assert.deepEqual(
    cases.map((value) => parseScopedObservationKnownEnvelope(value).family),
    ['usage', 'actor', 'source', 'artifact_availability', 'completion', 'continuity'],
  );
  assert.deepEqual(parseScopedObservationKnownEnvelope(outer.source_created), outer.source_created);
});

test('outer discriminator, context, and specialist event cannot drift', () => {
  const base = wrapped('source', source.context, source.created);
  for (const mutate of [
    (value: JsonObject) => {
      value.scoped_known_envelope_contract_version = 2;
    },
    (value: JsonObject) => {
      value.future = true;
    },
    (value: JsonObject) => {
      delete value.context;
    },
    (value: JsonObject) => {
      value.family = 'usage';
    },
    (value: JsonObject) => {
      value.context.authorized_sources[0].object_key = value.context.root.session_key;
    },
    (value: JsonObject) => {
      value.event.source.object_key = value.event.root.session_key;
    },
  ]) {
    const changed = clone(base);
    mutate(changed);
    assert.throws(() => parseScopedObservationKnownEnvelope(changed));
  }
});

test('unknown families remain rejected until bounded preservation is negotiated', () => {
  const unknown = wrapped('future_family', source.context, {
    kind: 'future_event',
    payload: { opaque: true },
  });
  assert.throws(
    () => parseScopedObservationKnownEnvelope(unknown),
    /bounded unknown-wire preservation is not negotiated/,
  );
});

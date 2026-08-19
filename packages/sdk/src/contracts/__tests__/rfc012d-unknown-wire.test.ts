import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';

import { parseScopedObservationEventEnvelope } from '../rfc012d-event-envelope.js';
import {
  IncompatibleObservationUnknownWireContractError,
  negotiateObservationUnknownWire,
  parseObservationUnknownWireContractOffer,
  parseObservationUnknownWireContractRequest,
  parseObservationUnknownWireContractSelection,
  parseObservationUnknownWireEvent,
  serializeObservationUnknownWireEvent,
  type ObservationUnknownWireContractSelection,
} from '../rfc012d-unknown-wire.js';

type JsonObject = Record<string, any>;

function fixture(name: string): JsonObject {
  return JSON.parse(
    readFileSync(new URL(`../../../../../crates/spaghetti-napi/fixtures/contracts/${name}`, import.meta.url), 'utf8'),
  ) as JsonObject;
}

function clone<T>(value: T): T {
  return JSON.parse(JSON.stringify(value)) as T;
}

const unknown = fixture('rfc012d-observation-unknown-wire-v1.json');
const source = fixture('rfc012d-scoped-source-envelope-v1.json');

function selected(): ObservationUnknownWireContractSelection {
  return parseObservationUnknownWireContractSelection(
    unknown.contract_selection,
    unknown.contract_request,
    unknown.contract_offer,
    unknown.contract_selection.observation_selection,
  );
}

test('Rust sidecar negotiation and bounded unknown carrier parse identically', () => {
  assert.deepEqual(parseObservationUnknownWireContractRequest(unknown.contract_request), unknown.contract_request);
  assert.deepEqual(parseObservationUnknownWireContractOffer(unknown.contract_offer), unknown.contract_offer);
  const selection = selected();
  assert.deepEqual(
    negotiateObservationUnknownWire(
      unknown.contract_request,
      unknown.contract_offer,
      unknown.contract_selection.observation_selection,
    ),
    selection,
  );
  assert.equal(selection.capability.max_preserved_bytes, unknown.expected.selected_max_preserved_bytes);
  const event = parseObservationUnknownWireEvent(unknown.unknown_wire_event, selection);
  assert.equal(event.type_tag, 'runtime.message_delta_v2');
  assert.deepEqual(serializeObservationUnknownWireEvent(event, selection), unknown.unknown_wire_event);
});

test('complete outer union routes known families and requires negotiated unknown preservation', () => {
  const known = {
    scoped_observation_event_union_contract_version: 1,
    family: 'source',
    context: source.context,
    event: source.created,
  };
  assert.equal(parseScopedObservationEventEnvelope(known).family, 'source');

  const unknownEnvelope = {
    scoped_observation_event_union_contract_version: 1,
    family: 'unknown_wire_event',
    context: source.context,
    event: unknown.unknown_wire_event,
  };
  assert.throws(
    () => parseScopedObservationEventEnvelope(unknownEnvelope),
    /unknown-wire event preservation was not negotiated/,
  );
  const parsed = parseScopedObservationEventEnvelope(unknownEnvelope, selected());
  assert.equal(parsed.family, 'unknown_wire_event');
  if (parsed.family !== 'unknown_wire_event') throw new Error('expected typed unknown event');
  assert.equal(parsed.event.type_tag, 'runtime.message_delta_v2');

  const sourceDrift = clone(unknownEnvelope);
  sourceDrift.event.envelope_provenance.source.object_key = sourceDrift.context.root.session_key;
  assert.throws(() => parseScopedObservationEventEnvelope(sourceDrift, selected()));

  const selectionDrift = clone(unknownEnvelope);
  selectionDrift.context.contract_selection.event_contract_version = 2;
  assert.throws(() => parseScopedObservationEventEnvelope(selectionDrift, selected()));

  const outerDrift = clone(unknownEnvelope);
  (outerDrift as JsonObject).future = true;
  assert.throws(() => parseScopedObservationEventEnvelope(outerDrift, selected()));
});

test('every preservation axis and exact selection fail closed', () => {
  for (const [field, axis] of [
    ['preserves_type_tag', 'type_tag_preservation'],
    ['preserves_encoded_value', 'encoded_value_preservation'],
    ['preserves_envelope_provenance', 'envelope_provenance_preservation'],
  ] as const) {
    const request = clone(unknown.contract_request);
    request.capability[field] = false;
    assert.throws(
      () =>
        negotiateObservationUnknownWire(
          request,
          unknown.contract_offer,
          unknown.contract_selection.observation_selection,
        ),
      (error) => error instanceof IncompatibleObservationUnknownWireContractError && error.axis === axis,
    );
  }

  const futureVersion = clone(unknown.contract_offer);
  futureVersion.capability.unknown_wire_event_contract_version = 2;
  assert.throws(
    () =>
      negotiateObservationUnknownWire(
        unknown.contract_request,
        futureVersion,
        unknown.contract_selection.observation_selection,
      ),
    (error) =>
      error instanceof IncompatibleObservationUnknownWireContractError && error.axis === 'event_contract_version',
  );

  const drifted = clone(unknown.contract_selection);
  drifted.capability.max_preserved_bytes = 8_192;
  assert.throws(() =>
    parseObservationUnknownWireContractSelection(
      drifted,
      unknown.contract_request,
      unknown.contract_offer,
      unknown.contract_selection.observation_selection,
    ),
  );
});

test('typed unknown values are bounded portable and cannot shadow known variants', () => {
  const selection = selected();
  for (const knownTypeTag of unknown.expected.known_event_type_tags as string[]) {
    const changed = clone(unknown.unknown_wire_event);
    changed.type_tag = knownTypeTag;
    assert.throws(() => parseObservationUnknownWireEvent(changed, selection));
  }
  for (const mutate of [
    (value: JsonObject) => {
      value.future = true;
    },
    (value: JsonObject) => {
      value.family = 'source';
    },
    (value: JsonObject) => {
      value.type_tag = 'source_created';
    },
    (value: JsonObject) => {
      value.type_tag = 'Future Event';
    },
    (value: JsonObject) => {
      value.envelope_provenance.observer_sequence = 0;
    },
    (value: JsonObject) => {
      value.envelope_provenance.scope_epoch = 0;
    },
    (value: JsonObject) => {
      value.envelope_provenance.event_id = 'v1:AA';
    },
    (value: JsonObject) => {
      value.envelope_provenance.source.generation = 0;
    },
    (value: JsonObject) => {
      value.envelope_provenance.phase = 'future';
    },
    (value: JsonObject) => {
      value.encoded_value = 1.5;
    },
    (value: JsonObject) => {
      value.encoded_value = JSON.parse('{"__proto__":{"polluted":true}}');
    },
  ]) {
    const changed = clone(unknown.unknown_wire_event);
    mutate(changed);
    assert.throws(() => parseObservationUnknownWireEvent(changed, selection));
  }

  const tinyRequest = clone(unknown.contract_request);
  const tinyOffer = clone(unknown.contract_offer);
  tinyRequest.capability.max_preserved_bytes = 8;
  tinyOffer.capability.max_preserved_bytes = 8;
  const tiny = negotiateObservationUnknownWire(
    tinyRequest,
    tinyOffer,
    unknown.contract_selection.observation_selection,
  );
  assert.throws(() => parseObservationUnknownWireEvent(unknown.unknown_wire_event, tiny));

  const exactRequest = clone(unknown.contract_request);
  const exactOffer = clone(unknown.contract_offer);
  exactRequest.capability.max_preserved_bytes = 16;
  exactOffer.capability.max_preserved_bytes = 16;
  const exactEncoded = negotiateObservationUnknownWire(
    exactRequest,
    exactOffer,
    unknown.contract_selection.observation_selection,
  );
  const exact = clone(unknown.unknown_wire_event);
  exact.encoded_value = '\\'.repeat(6);
  exact.envelope_provenance.additional_envelope_provenance = {};
  assert.doesNotThrow(() => parseObservationUnknownWireEvent(exact, exactEncoded));
  const escapedOversize = clone(exact);
  escapedOversize.encoded_value = '\\'.repeat(7);
  assert.throws(() => parseObservationUnknownWireEvent(escapedOversize, exactEncoded));

  const malformedUnicode = clone(unknown.unknown_wire_event);
  malformedUnicode.encoded_value = '\ud800';
  assert.throws(() => parseObservationUnknownWireEvent(malformedUnicode, selection));

  let deep: unknown = null;
  for (let index = 0; index <= unknown.expected.payload_depth_limit; index += 1) deep = [deep];
  const depthDrift = clone(unknown.unknown_wire_event);
  depthDrift.encoded_value = deep;
  assert.throws(() => parseObservationUnknownWireEvent(depthDrift, selection));

  const nodeDrift = clone(unknown.unknown_wire_event);
  nodeDrift.encoded_value = Array.from({ length: unknown.expected.payload_node_limit }, () => null);
  assert.throws(() => parseObservationUnknownWireEvent(nodeDrift, selection));
});

test('attacker-sized capability and unknown shapes reject before becoming authority', () => {
  for (const bound of [0, 65_537]) {
    const request = clone(unknown.contract_request);
    request.capability.max_preserved_bytes = bound;
    assert.throws(() => parseObservationUnknownWireContractRequest(request));
  }
  const additive = clone(unknown.contract_offer);
  additive.future = true;
  assert.throws(() => parseObservationUnknownWireContractOffer(additive));
});

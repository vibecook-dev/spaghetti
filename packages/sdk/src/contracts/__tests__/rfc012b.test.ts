import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';

import { ContractValidationError } from '../rfc012a.js';
import {
  createCatalogQueryContractRequest,
  IncompatibleCatalogContractError,
  negotiateCatalogQueryContract,
  parseCatalogContinuationRequest,
  parseCatalogQueryContractOffer,
  parseCatalogQueryContractRequest,
  parseCatalogQueryContractResponse,
  parseCatalogQueryContractSelection,
  parseCatalogQueryContractSelectionForRequest,
  serializeCatalogQueryContractResponse,
} from '../rfc012b.js';

interface CatalogQueryFixture {
  fixture_contract_version: number;
  contract_request: unknown;
  contract_offer: unknown;
  selected_response: { contract_selection: unknown };
  selected_response_with_additive_field: unknown;
  unknown_response_variant: unknown;
  continuation_request: unknown;
  expected: {
    selected_query_pack_version: number;
    selected_unknown_max_payload_bytes: number;
    additive_field: string;
    unknown_variant: string;
    cursor_binding_valid: boolean;
    incompatible_error: string;
  };
}

const rawFixture = JSON.parse(
  readFileSync(
    new URL('../../../../../crates/spaghetti-napi/fixtures/contracts/rfc012b-catalog-query-v1.json', import.meta.url),
    'utf8',
  ),
) as CatalogQueryFixture;
const expectedSelection = rawFixture.selected_response.contract_selection;

function clone<T>(value: T): T {
  return structuredClone(value);
}

function reject(value: unknown, parse: (input: unknown) => unknown): void {
  assert.throws(() => parse(value), ContractValidationError);
}

function parseFixtureResponse(value: unknown): ReturnType<typeof parseCatalogQueryContractResponse> {
  return parseCatalogQueryContractResponse(value, expectedSelection);
}

function parseFixtureContinuation(value: unknown): ReturnType<typeof parseCatalogContinuationRequest> {
  return parseCatalogContinuationRequest(value, expectedSelection);
}

test('Rust RFC 012B query fixture negotiates identically in the portable SDK', () => {
  assert.equal(rawFixture.fixture_contract_version, 1);
  const request = parseCatalogQueryContractRequest(rawFixture.contract_request);
  const offer = parseCatalogQueryContractOffer(rawFixture.contract_offer);
  const selection = negotiateCatalogQueryContract(request, offer);
  assert.deepEqual(selection, rawFixture.selected_response.contract_selection);
  assert.equal(selection.contract_versions.query_pack_version, rawFixture.expected.selected_query_pack_version);
  assert.equal(selection.typed_unknown.max_payload_bytes, rawFixture.expected.selected_unknown_max_payload_bytes);

  const continuation = parseFixtureContinuation(rawFixture.continuation_request);
  assert.equal(continuation.cursor.snapshot_id.complete_commit, continuation.snapshot_id.complete_commit);
  assert.equal(continuation.cursor.query_fingerprint, continuation.query_fingerprint);
  assert.equal(rawFixture.expected.cursor_binding_valid, true);
});

test('default catalog request and selected response retain the caller offer', () => {
  const request = createCatalogQueryContractRequest();
  assert.deepEqual(request.contract_versions.fact_family_versions, {
    'catalog.project': [1],
    'catalog.session': [1],
  });

  const selection = clone(rawFixture.selected_response.contract_selection) as Record<string, unknown>;
  assert.deepEqual(parseCatalogQueryContractSelectionForRequest(selection, rawFixture.contract_request), selection);

  const contracts = selection.contract_versions as Record<string, unknown>;
  contracts.fact_family_versions = { 'catalog.project': 1 };
  assert.throws(
    () => parseCatalogQueryContractSelectionForRequest(selection, rawFixture.contract_request),
    /does not satisfy the caller-held request/,
  );
});

test('additive fields and future response variants survive typed-unknown round trips', () => {
  const selected = parseFixtureResponse(rawFixture.selected_response_with_additive_field);
  assert.equal(selected.kind, 'selected');
  if (selected.kind !== 'selected') throw new Error('expected selected response');
  assert.deepEqual(selected.additive_fields[rawFixture.expected.additive_field], {
    mode: 'bounded',
    retry_after: 2,
  });
  assert.deepEqual(serializeCatalogQueryContractResponse(selected), rawFixture.selected_response_with_additive_field);

  const unknown = parseFixtureResponse(rawFixture.unknown_response_variant);
  assert.equal(unknown.kind, 'typed_unknown');
  if (unknown.kind !== 'typed_unknown') throw new Error('expected typed unknown response');
  assert.equal(unknown.variant, rawFixture.expected.unknown_variant);
  assert.deepEqual(unknown.payload.capability, { enabled: true, name: 'server_rank_hint' });
  assert.deepEqual(serializeCatalogQueryContractResponse(unknown), rawFixture.unknown_response_variant);
});

test('incompatible model and catalog-pack versions reject with the typed catalog error', () => {
  const request = clone(rawFixture.contract_request) as {
    contract_versions: { model_major: number; query_pack_versions: number[] };
  };
  const offer = clone(rawFixture.contract_offer) as {
    contract_versions: { model_major: number; query_pack_versions: number[] };
  };
  request.contract_versions.model_major = 2;
  offer.contract_versions.model_major = 2;
  assert.throws(
    () => negotiateCatalogQueryContract(request, offer),
    (error: unknown) =>
      error instanceof IncompatibleCatalogContractError &&
      error.name === rawFixture.expected.incompatible_error &&
      error.axis === 'base_model_major',
  );

  request.contract_versions.model_major = 1;
  offer.contract_versions.model_major = 1;
  request.contract_versions.query_pack_versions = [2];
  offer.contract_versions.query_pack_versions = [2];
  assert.throws(
    () => negotiateCatalogQueryContract(request, offer),
    (error: unknown) => error instanceof IncompatibleCatalogContractError && error.axis === 'query_pack_version',
  );

  const factRequest = clone(rawFixture.contract_request) as {
    contract_versions: { fact_family_versions: Record<string, number[]> };
  };
  const factOffer = clone(rawFixture.contract_offer) as {
    contract_versions: { fact_family_versions: Record<string, number[]> };
  };
  factRequest.contract_versions.fact_family_versions['catalog.session'] = [1];
  factOffer.contract_versions.fact_family_versions['catalog.session'] = [2];
  assert.throws(
    () => negotiateCatalogQueryContract(factRequest, factOffer),
    (error: unknown) => error instanceof IncompatibleCatalogContractError && error.axis === 'fact_family_version',
  );

  const observationRequest = clone(rawFixture.contract_request) as {
    contract_versions: { observation_contract_versions: number[] | null };
  };
  const observationOffer = clone(rawFixture.contract_offer) as {
    contract_versions: { observation_contract_versions: number[] };
  };
  observationRequest.contract_versions.observation_contract_versions = [1];
  observationOffer.contract_versions.observation_contract_versions = [2];
  assert.throws(
    () => negotiateCatalogQueryContract(observationRequest, observationOffer),
    (error: unknown) =>
      error instanceof IncompatibleCatalogContractError && error.axis === 'observation_contract_version',
  );
});

test('wire majors, malformed preferences, and forged selections are rejected', () => {
  const request = clone(rawFixture.contract_request) as {
    catalog_query_contract_version: number;
    contract_versions: { query_pack_versions: number[] | null };
  };
  request.catalog_query_contract_version = 2;
  reject(request, parseCatalogQueryContractRequest);

  request.catalog_query_contract_version = 1;
  request.contract_versions.query_pack_versions = [1, 1];
  reject(request, parseCatalogQueryContractRequest);
  request.contract_versions.query_pack_versions = null;
  reject(request, parseCatalogQueryContractRequest);

  const response = clone(rawFixture.selected_response) as {
    catalog_query_response_contract_version?: number;
    contract_selection: { contract_versions: { query_pack_version: number } };
  };
  response.catalog_query_response_contract_version = 2;
  reject(response, parseFixtureResponse);
  response.catalog_query_response_contract_version = 1;
  response.contract_selection.contract_versions.query_pack_version = 2;
  reject(response, parseFixtureResponse);

  const driftedSelection = clone(rawFixture.selected_response) as {
    contract_selection: { typed_unknown: { max_payload_bytes: number } };
  };
  driftedSelection.contract_selection.typed_unknown.max_payload_bytes = 8_192;
  reject(driftedSelection, parseFixtureResponse);

  const requestUnknown = clone(rawFixture.contract_request) as Record<string, unknown>;
  requestUnknown.future_request_meaning = true;
  reject(requestUnknown, parseCatalogQueryContractRequest);

  const contractUnknown = clone(rawFixture.contract_request) as {
    contract_versions: Record<string, unknown>;
  };
  contractUnknown.contract_versions.future_contract_axis = 1;
  reject(contractUnknown, parseCatalogQueryContractRequest);

  const offerUnknown = clone(rawFixture.contract_offer) as Record<string, unknown>;
  offerUnknown.future_offer_meaning = true;
  reject(offerUnknown, parseCatalogQueryContractOffer);

  const selectedUnknown = clone(expectedSelection) as Record<string, unknown>;
  selectedUnknown.future_selection_meaning = true;
  reject(selectedUnknown, parseCatalogQueryContractSelection);

  const typedUnknown = clone(rawFixture.contract_request) as {
    typed_unknown: Record<string, unknown>;
  };
  typedUnknown.typed_unknown.future_preservation_semantics = true;
  reject(typedUnknown, parseCatalogQueryContractRequest);
});

test('typed unknown preservation is bounded and rejects nonportable payloads', () => {
  const request = clone(rawFixture.contract_request) as {
    typed_unknown: { preserves_unknown_fields: boolean };
  };
  request.typed_unknown.preserves_unknown_fields = false;
  assert.throws(
    () => negotiateCatalogQueryContract(request, rawFixture.contract_offer),
    (error: unknown) =>
      error instanceof IncompatibleCatalogContractError && error.axis === 'typed_unknown_preservation',
  );

  const oversized = clone(rawFixture.selected_response) as Record<string, unknown>;
  oversized.future_payload = 'x'.repeat(4_096);
  reject(oversized, parseFixtureResponse);

  const floating = clone(rawFixture.selected_response) as Record<string, unknown>;
  floating.future_number = 1.5;
  reject(floating, parseFixtureResponse);

  const nonJson = clone(rawFixture.selected_response) as Record<string, unknown>;
  nonJson.future_value = new Date();
  reject(nonJson, parseFixtureResponse);

  let nested: unknown = null;
  for (let index = 0; index <= 16; index += 1) nested = [nested];
  const tooDeep = clone(rawFixture.selected_response) as Record<string, unknown>;
  tooDeep.future_nested = nested;
  reject(tooDeep, parseFixtureResponse);

  const selected = parseFixtureResponse(rawFixture.selected_response);
  if (selected.kind !== 'selected') throw new Error('expected selected response');
  assert.throws(
    () =>
      serializeCatalogQueryContractResponse({
        ...selected,
        additive_fields: { kind: 'cannot_replace_discriminant' },
      }),
    ContractValidationError,
  );
});

test('continuation parsing rejects snapshot, query, sort, pack, and page-size drift', () => {
  const wrongSnapshot = clone(rawFixture.continuation_request) as {
    cursor: { snapshot_id: { complete_commit: number } };
  };
  wrongSnapshot.cursor.snapshot_id.complete_commit += 1;
  reject(wrongSnapshot, parseFixtureContinuation);

  const wrongQuery = clone(rawFixture.continuation_request) as {
    cursor: { query_fingerprint: string; last_entity_key: string };
  };
  wrongQuery.cursor.query_fingerprint = wrongQuery.cursor.last_entity_key;
  reject(wrongQuery, parseFixtureContinuation);

  const wrongSort = clone(rawFixture.continuation_request) as {
    cursor: { sort_spec_version: number };
  };
  wrongSort.cursor.sort_spec_version += 1;
  reject(wrongSort, parseFixtureContinuation);

  const overflowingSort = clone(rawFixture.continuation_request) as {
    sort_spec_version: number;
    cursor: { sort_spec_version: number };
  };
  overflowingSort.sort_spec_version = 0x1_0000_0000;
  overflowingSort.cursor.sort_spec_version = 0x1_0000_0000;
  reject(overflowingSort, parseFixtureContinuation);

  const wrongPack = clone(rawFixture.continuation_request) as {
    snapshot_id: { pack_contract_version: number };
    cursor: { snapshot_id: { pack_contract_version: number } };
  };
  wrongPack.snapshot_id.pack_contract_version = 2;
  wrongPack.cursor.snapshot_id.pack_contract_version = 2;
  reject(wrongPack, parseFixtureContinuation);

  const unsafeCommit = clone(rawFixture.continuation_request) as {
    snapshot_id: { complete_commit: number };
    cursor: { snapshot_id: { complete_commit: number } };
  };
  unsafeCommit.snapshot_id.complete_commit = Number.MAX_SAFE_INTEGER + 1;
  unsafeCommit.cursor.snapshot_id.complete_commit = Number.MAX_SAFE_INTEGER + 1;
  reject(unsafeCommit, parseFixtureContinuation);

  const unsafeEpoch = clone(rawFixture.continuation_request) as {
    snapshot_id: { readiness_epoch: number };
    cursor: { snapshot_id: { readiness_epoch: number } };
  };
  unsafeEpoch.snapshot_id.readiness_epoch = Number.MAX_SAFE_INTEGER + 1;
  unsafeEpoch.cursor.snapshot_id.readiness_epoch = Number.MAX_SAFE_INTEGER + 1;
  reject(unsafeEpoch, parseFixtureContinuation);

  const driftedSelection = clone(rawFixture.continuation_request) as {
    contract_selection: { typed_unknown: { max_payload_bytes: number } };
  };
  driftedSelection.contract_selection.typed_unknown.max_payload_bytes = 8_192;
  reject(driftedSelection, parseFixtureContinuation);

  const zeroPage = clone(rawFixture.continuation_request) as { page_size: number };
  zeroPage.page_size = 0;
  reject(zeroPage, parseFixtureContinuation);

  const unknownContinuation = clone(rawFixture.continuation_request) as Record<string, unknown>;
  unknownContinuation.future_continuation_meaning = true;
  reject(unknownContinuation, parseFixtureContinuation);

  const unknownCursor = clone(rawFixture.continuation_request) as {
    cursor: Record<string, unknown>;
  };
  unknownCursor.cursor.future_cursor_meaning = true;
  reject(unknownCursor, parseFixtureContinuation);

  const unknownSnapshot = clone(rawFixture.continuation_request) as {
    cursor: { snapshot_id: Record<string, unknown> };
  };
  unknownSnapshot.cursor.snapshot_id.future_snapshot_meaning = true;
  reject(unknownSnapshot, parseFixtureContinuation);
});

test('portable query parsing does not depend on the Node Buffer global', () => {
  const globals = globalThis as unknown as { Buffer?: unknown };
  const originalBuffer = globals.Buffer;
  globals.Buffer = undefined;
  try {
    assert.equal(parseFixtureContinuation(rawFixture.continuation_request).page_size, 50);
  } finally {
    globals.Buffer = originalBuffer;
  }
});

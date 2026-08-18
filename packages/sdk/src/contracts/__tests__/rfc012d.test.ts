import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';

import { ContractValidationError } from '../rfc012a.js';
import {
  IncompatibleObservationContractError,
  negotiateObservationContract,
  parseObservationContractOffer,
  parseObservationContractRequest,
  parseObservationContractSelection,
} from '../rfc012d.js';

interface ObservationNegotiationFixture {
  fixture_contract_version: number;
  contract_request: unknown;
  contract_offer: unknown;
  contract_selection: unknown;
  expected: {
    incompatible_error: string;
    selected_fact_family: string;
    selected_fact_family_version: number;
    query_pack_selected: boolean;
    typed_unknown_event_preservation: string;
  };
}

const fixture = JSON.parse(
  readFileSync(
    new URL(
      '../../../../../crates/spaghetti-napi/fixtures/contracts/rfc012d-observation-negotiation-v1.json',
      import.meta.url,
    ),
    'utf8',
  ),
) as ObservationNegotiationFixture;

function clone<T>(value: T): T {
  return structuredClone(value);
}

function reject(value: unknown, parser: (input: unknown) => unknown): void {
  assert.throws(() => parser(value), ContractValidationError);
}

test('Rust RFC 012D negotiation fixture selects identically in portable TypeScript', () => {
  assert.equal(fixture.fixture_contract_version, 1);
  const request = parseObservationContractRequest(fixture.contract_request);
  const offer = parseObservationContractOffer(fixture.contract_offer);
  const selection = negotiateObservationContract(request, offer);
  assert.deepEqual(selection, fixture.contract_selection);
  assert.equal(
    selection.contract_versions.fact_family_versions[fixture.expected.selected_fact_family],
    fixture.expected.selected_fact_family_version,
  );
  assert.equal(selection.contract_versions.query_pack_version !== null, fixture.expected.query_pack_selected);
  assert.equal(fixture.expected.typed_unknown_event_preservation, 'not_yet_negotiated');
});

test('every incompatible observation axis returns the typed pre-access error', () => {
  const cases: Array<{
    axis: ConstructorParameters<typeof IncompatibleObservationContractError>[0];
    mutate: (request: any, offer: any) => void;
  }> = [
    {
      axis: 'base_model_major',
      mutate: (request, offer) => {
        request.contract_versions.model_major = 2;
        offer.contract_versions.model_major = 2;
      },
    },
    {
      axis: 'external_entity_reference_version',
      mutate: (request, offer) => {
        request.contract_versions.external_entity_reference_version = 2;
        offer.contract_versions.external_entity_reference_versions = [2];
      },
    },
    {
      axis: 'semantic_revision_reference_version',
      mutate: (request, offer) => {
        request.contract_versions.semantic_revision_reference_version = 2;
        offer.contract_versions.semantic_revision_reference_versions = [2];
      },
    },
    {
      axis: 'coverage_contract_version',
      mutate: (request, offer) => {
        request.contract_versions.coverage_contract_versions = [2];
        offer.contract_versions.coverage_contract_versions = [2];
      },
    },
    {
      axis: 'fact_family_version',
      mutate: (request) => {
        request.contract_versions.fact_family_versions['runtime.usage-v2'] = [2];
      },
    },
    {
      axis: 'observation_profile_version',
      mutate: (request, offer) => {
        request.contract_versions.observation_contract_versions = [2];
        offer.contract_versions.observation_contract_versions = [2];
      },
    },
    {
      axis: 'envelope_contract_version',
      mutate: (request, offer) => {
        request.envelope_contract_versions = [2];
        offer.envelope_contract_versions = [2];
      },
    },
    {
      axis: 'event_contract_version',
      mutate: (request, offer) => {
        request.event_contract_versions = [2];
        offer.event_contract_versions = [2];
      },
    },
    {
      axis: 'lifecycle_contract_version',
      mutate: (request, offer) => {
        request.lifecycle_contract_versions = [2];
        offer.lifecycle_contract_versions = [2];
      },
    },
  ];

  for (const { axis, mutate } of cases) {
    const request = clone(fixture.contract_request);
    const offer = clone(fixture.contract_offer);
    mutate(request, offer);
    assert.throws(
      () => negotiateObservationContract(request, offer),
      (error: unknown) =>
        error instanceof IncompatibleObservationContractError &&
        error.name === fixture.expected.incompatible_error &&
        error.axis === axis,
    );
  }
});

test('strict bounded requests cannot smuggle query-pack authority', () => {
  const query = clone(fixture.contract_request) as any;
  query.contract_versions.query_pack_versions = [1];
  reject(query, parseObservationContractRequest);

  const queryOffer = clone(fixture.contract_offer) as any;
  queryOffer.contract_versions.query_pack_versions = [1];
  reject(queryOffer, parseObservationContractOffer);

  const duplicate = clone(fixture.contract_request) as any;
  duplicate.event_contract_versions = [1, 1];
  reject(duplicate, parseObservationContractRequest);

  const oversized = clone(fixture.contract_request) as any;
  oversized.lifecycle_contract_versions = Array.from({ length: 17 }, (_, index) => index + 1);
  reject(oversized, parseObservationContractRequest);

  const unknown = clone(fixture.contract_request) as Record<string, unknown>;
  unknown.future_request_meaning = true;
  reject(unknown, parseObservationContractRequest);

  const emptyFamilies = clone(fixture.contract_request) as any;
  emptyFamilies.contract_versions.fact_family_versions = {};
  reject(emptyFamilies, parseObservationContractRequest);

  const invalidFamily = clone(fixture.contract_request) as any;
  invalidFamily.contract_versions.fact_family_versions = { 'Invalid Family': [1] };
  reject(invalidFamily, parseObservationContractRequest);
});

test('selected contract parsing rejects forged or unnegotiated semantics', () => {
  const parseSelection = (value: unknown) =>
    parseObservationContractSelection(value, fixture.contract_request, fixture.contract_offer);
  const selection = parseSelection(fixture.contract_selection);
  assert.deepEqual(selection, fixture.contract_selection);

  const event = clone(fixture.contract_selection) as any;
  event.event_contract_version = 2;
  reject(event, parseSelection);

  const query = clone(fixture.contract_selection) as any;
  query.contract_versions.query_pack_version = 1;
  reject(query, parseSelection);

  const unknownFamily = clone(fixture.contract_selection) as any;
  unknownFamily.contract_versions.fact_family_versions['Invalid Family'] = 1;
  reject(unknownFamily, parseSelection);

  const retargetedFamily = clone(fixture.contract_selection) as any;
  retargetedFamily.contract_versions.fact_family_versions = { 'runtime.other-v1': 1 };
  reject(retargetedFamily, parseSelection);

  const downgradedFamily = clone(fixture.contract_selection) as any;
  downgradedFamily.contract_versions.fact_family_versions['runtime.usage-v2'] = 2;
  reject(downgradedFamily, parseSelection);
});

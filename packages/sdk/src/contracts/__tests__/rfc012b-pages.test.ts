import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';

import { ContractValidationError } from '../rfc012a.js';
import {
  parseCatalogPortableCoveragePlan,
  parseCatalogEntityResolutionResponse,
  parseCatalogProjectPage,
  parseCatalogReadinessResponse,
  parseCatalogSessionPage,
  parseCatalogSnapshotExpired,
} from '../rfc012b-pages.js';

interface PageFixture {
  fixture_contract_version: number;
  contract_selection: unknown;
  published_plan: unknown;
  current_plan: unknown;
  project_page: Record<string, unknown> & { request: unknown };
  session_page: Record<string, unknown> & { request: unknown };
  readiness_response: unknown;
  resolutions: Record<string, Record<string, unknown> & { request: unknown }>;
  continuation_request: unknown;
  snapshot_expired: Record<string, unknown> & { scope: unknown };
  expected: {
    project_has_more: boolean;
    session_total_count_state: string;
    current_readiness_state: string;
    retained_snapshot_is_prior_plan: boolean;
    native_values_withheld: boolean;
  };
}

const fixture = JSON.parse(
  readFileSync(
    new URL('../../../../../crates/spaghetti-napi/fixtures/contracts/rfc012b-catalog-pages-v1.json', import.meta.url),
    'utf8',
  ),
) as PageFixture;

function clone<T>(value: T): T {
  return structuredClone(value);
}

function reject(callback: () => unknown): void {
  assert.throws(callback, ContractValidationError);
}

function parseProject(value: unknown, expectedRequest: unknown = fixture.project_page.request) {
  return parseCatalogProjectPage(value, expectedRequest, fixture.published_plan);
}

function parseSession(value: unknown, expectedRequest: unknown = fixture.session_page.request) {
  return parseCatalogSessionPage(value, expectedRequest, fixture.published_plan);
}

test('Rust RFC 012B project/session, readiness, resolution, and expiration fixtures parse portably', () => {
  assert.equal(fixture.fixture_contract_version, 1);
  const publishedPlan = parseCatalogPortableCoveragePlan(fixture.published_plan);
  const currentPlan = parseCatalogPortableCoveragePlan(fixture.current_plan);
  assert.notEqual(publishedPlan.coverage_plan_id, currentPlan.coverage_plan_id);

  const projects = parseProject(fixture.project_page);
  assert.equal(projects.has_more, fixture.expected.project_has_more);
  assert.equal(
    projects.next_continuation?.cursor.last_entity_key,
    projects.rows.at(-1)?.row.project_ref.external_ref.entity_key,
  );

  const sessions = parseSession(fixture.session_page);
  assert.equal(sessions.total_count.state, fixture.expected.session_total_count_state);

  const readiness = parseCatalogReadinessResponse(
    fixture.readiness_response,
    fixture.contract_selection,
    fixture.current_plan,
  );
  assert.equal(readiness.readiness.state, fixture.expected.current_readiness_state);

  const states = Object.fromEntries(
    Object.entries(fixture.resolutions).map(([name, response]) => [
      name,
      parseCatalogEntityResolutionResponse(response, response.request).resolution.state,
    ]),
  );
  assert.deepEqual(states, {
    live: 'live',
    superseded: 'superseded',
    tombstoned: 'tombstoned',
    unknown: 'unknown',
  });

  const expiration = parseCatalogSnapshotExpired(
    fixture.snapshot_expired,
    fixture.continuation_request,
    fixture.contract_selection,
    fixture.snapshot_expired.scope,
  );
  assert.ok(expiration.latest_snapshot.complete_commit > expiration.request.snapshot_id.complete_commit);
});

test('pages bind exact caller selection, snapshot, query, sort, size, ordering, and final cursor', () => {
  const selectionDrift = clone(fixture.project_page) as {
    request: { contract_selection: { typed_unknown: { max_payload_bytes: number } } };
  };
  selectionDrift.request.contract_selection.typed_unknown.max_payload_bytes = 8_192;
  reject(() => parseProject(selectionDrift));

  const responseSnapshotDrift = clone(fixture.project_page) as {
    request: { snapshot_id: { complete_commit: number } };
  };
  responseSnapshotDrift.request.snapshot_id.complete_commit += 1;
  reject(() => parseProject(responseSnapshotDrift));

  const foreignQuery = clone(fixture.project_page) as {
    request: { query_fingerprint: string };
  };
  foreignQuery.request.query_fingerprint = (
    fixture.resolutions.unknown!.request as { external_ref: { entity_key: string } }
  ).external_ref.entity_key;
  reject(() => parseProject(foreignQuery));

  const foreignSort = clone(fixture.project_page) as { request: { sort_spec_version: number } };
  foreignSort.request.sort_spec_version = 2;
  reject(() => parseProject(foreignSort));

  const foreignSize = clone(fixture.project_page) as { request: { page_size: number } };
  foreignSize.request.page_size = 2;
  reject(() => parseProject(foreignSize));

  const duplicateRows = clone(fixture.project_page) as {
    request: { page_size: number };
    rows: unknown[];
    next_continuation: { page_size: number };
  };
  duplicateRows.request.page_size = 2;
  duplicateRows.next_continuation.page_size = 2;
  duplicateRows.rows.push(clone(duplicateRows.rows[0]));
  reject(() => parseProject(duplicateRows, duplicateRows.request));

  const duplicateEntity = clone(fixture.project_page) as {
    request: { page_size: number };
    total_count: { state: string; value: number };
    rows: Array<{ sort_key: string }>;
    next_continuation: { page_size: number; cursor: { last_sort_key: string } };
  };
  const repeated = clone(duplicateEntity.rows[0]!);
  repeated.sort_key = 'v1:Zml4dHVyZS1wcm9qZWN0LXo';
  duplicateEntity.rows.push(repeated);
  duplicateEntity.request.page_size = 2;
  duplicateEntity.total_count = { state: 'known', value: 3 };
  duplicateEntity.next_continuation.page_size = 2;
  duplicateEntity.next_continuation.cursor.last_sort_key = repeated.sort_key;
  reject(() => parseProject(duplicateEntity, duplicateEntity.request));

  const impossibleTotal = clone(fixture.project_page) as unknown as {
    total_count: { state: string; value: number };
  };
  impossibleTotal.total_count = { state: 'known', value: 1 };
  reject(() => parseProject(impossibleTotal));

  const missingNext = clone(fixture.project_page) as { next_continuation?: unknown };
  delete missingNext.next_continuation;
  reject(() => parseProject(missingNext));

  const emptyWithNext = clone(fixture.project_page) as unknown as { rows: unknown[] };
  emptyWithNext.rows = [];
  reject(() => parseProject(emptyWithNext));

  const foreignFinalEntity = clone(fixture.project_page) as unknown as {
    next_continuation: { cursor: { last_entity_key: string } };
  };
  foreignFinalEntity.next_continuation.cursor.last_entity_key = (
    fixture.resolutions.unknown!.request as { external_ref: { entity_key: string } }
  ).external_ref.entity_key;
  reject(() => parseProject(foreignFinalEntity));
});

test('unknown and unavailable values stay typed and additive fields remain bounded data', () => {
  const projects = parseProject(fixture.project_page);
  const sessions = parseSession(fixture.session_page);
  assert.deepEqual(sessions.total_count, { state: 'unknown', reason: 'not_yet_observed' });
  assert.deepEqual(sessions.rows[0]?.row.native_message_count, {
    state: 'unknown',
    reason: 'not_yet_observed',
  });
  assert.equal(projects.rows[0]?.row.native_identity.state, 'selected');
  if (projects.rows[0]?.row.native_identity.state !== 'selected')
    throw new Error('expected selected identity evidence');
  assert.equal(projects.rows[0].row.native_identity.selection.field.value, null);
  assert.equal(projects.rows[0].row.native_identity.selection.field.unknown_reason, 'withheld');
  assert.deepEqual(projects.additive_fields.future_page_hint, { cache: 'warm' });

  const reservedEnvelopeName = clone(fixture.project_page) as Record<string, unknown>;
  reservedEnvelopeName.kind = 'future_page_metadata';
  assert.equal(parseProject(reservedEnvelopeName).additive_fields.kind, 'future_page_metadata');

  const bareZero = clone(fixture.session_page) as unknown as { total_count: unknown };
  bareZero.total_count = 0;
  reject(() => parseSession(bareZero));

  const unavailable = clone(fixture.session_page) as unknown as {
    rows: Array<{ row: { availability: { field: { value: unknown } } } }>;
  };
  unavailable.rows[0]!.row.availability.field.value = { state: 'unavailable', reason: 'source_unavailable' };
  assert.deepEqual(parseSession(unavailable).rows[0]?.row.availability.field.value, {
    state: 'unavailable',
    reason: 'source_unavailable',
  });

  const rowAdditive = clone(fixture.project_page) as unknown as {
    rows: Array<{ row: Record<string, unknown> }>;
  };
  rowAdditive.rows[0]!.row.future_row_hint = { confidence: 1 };
  assert.deepEqual(parseProject(rowAdditive).rows[0]?.row.additive_fields.future_row_hint, { confidence: 1 });

  const foreignConflict = clone(fixture.project_page) as unknown as {
    rows: Array<{
      row: { display_name: { selection: { conflicting_assertion_keys: string[] } } };
    }>;
  };
  foreignConflict.rows[0]!.row.display_name.selection.conflicting_assertion_keys = [
    (fixture.resolutions.unknown!.request as { external_ref: { entity_key: string } }).external_ref.entity_key,
  ];
  reject(() => parseProject(foreignConflict));

  const oversizedConflicts = clone(fixture.project_page) as unknown as {
    rows: Array<{ row: { display_name: { selection: { conflicting_assertion_keys: unknown[] } } } }>;
  };
  const selected = oversizedConflicts.rows[0]!.row.display_name.selection;
  selected.conflicting_assertion_keys = Array.from({ length: 4_097 }, () =>
    clone((oversizedConflicts.rows[0]!.row as unknown as { assertion_keys: unknown[] }).assertion_keys[0]),
  );
  reject(() => parseProject(oversizedConflicts));
});

test('current readiness may retain a prior-plan snapshot but cannot present it as current coverage', () => {
  const parsed = parseCatalogReadinessResponse(
    fixture.readiness_response,
    fixture.contract_selection,
    fixture.current_plan,
  );
  assert.equal(parsed.readiness.state, 'building');
  assert.equal(parsed.readiness.complete_through_commit, undefined);
  assert.notEqual(parsed.readiness.last_complete_snapshot?.coverage_plan_id, parsed.readiness.coverage_plan_id);

  const falseCurrent = clone(fixture.readiness_response) as {
    readiness: { complete_through_commit?: number; last_complete_snapshot: { complete_commit: number } };
  };
  falseCurrent.readiness.complete_through_commit = falseCurrent.readiness.last_complete_snapshot.complete_commit;
  reject(() => parseCatalogReadinessResponse(falseCurrent, fixture.contract_selection, fixture.current_plan));

  const futureRetained = clone(fixture.readiness_response) as {
    readiness: { epoch: number; last_complete_snapshot: { readiness_epoch: number } };
  };
  futureRetained.readiness.last_complete_snapshot.readiness_epoch = futureRetained.readiness.epoch + 1;
  reject(() => parseCatalogReadinessResponse(futureRetained, fixture.contract_selection, fixture.current_plan));

  const discardedButRetained = clone(fixture.readiness_response) as {
    readiness: { state: string; reason?: unknown };
  };
  discardedButRetained.readiness.state = 'error';
  discardedButRetained.readiness.reason = {
    kind: 'integrity_failure',
    code: 'fixture_integrity_failure',
    snapshot_disposition: 'discarded',
  };
  reject(() => parseCatalogReadinessResponse(discardedButRetained, fixture.contract_selection, fixture.current_plan));

  reject(() =>
    parseCatalogReadinessResponse(fixture.readiness_response, fixture.contract_selection, fixture.published_plan),
  );

  const policyDrift = clone(fixture.project_page) as unknown as {
    published_readiness: {
      source_coverage: Array<{ scope: { source_or_scope_declaration_digest: string } }>;
    };
  };
  policyDrift.published_readiness.source_coverage[0]!.scope.source_or_scope_declaration_digest = (
    fixture.published_plan as {
      required_sources: Array<{ catalog_declaration_digest: string }>;
    }
  ).required_sources[0]!.catalog_declaration_digest;
  reject(() => parseProject(policyDrift));

  const zeroPoint = clone(fixture.project_page) as unknown as {
    published_readiness: { source_coverage: Array<{ points: Array<{ generation: number }> }> };
  };
  zeroPoint.published_readiness.source_coverage[0]!.points[0]!.generation = 0;
  reject(() => parseProject(zeroPoint));

  const zeroAbsence = clone(fixture.project_page) as unknown as {
    published_readiness: {
      source_coverage: Array<{
        points: Array<{ stream_key: string; object_key: string }>;
        explicit_absence_or_deletion: unknown[];
      }>;
    };
  };
  const coverage = zeroAbsence.published_readiness.source_coverage[0]!;
  const point = coverage.points[0]!;
  coverage.points = [];
  coverage.explicit_absence_or_deletion = [
    { stream_key: point.stream_key, object_key: point.object_key, generation: 0, kind: 'absent' },
  ];
  reject(() => parseProject(zeroAbsence));

  const duplicateCoverageError = clone(fixture.project_page) as unknown as {
    published_readiness: {
      source_coverage: Array<{ explicit_errors: unknown[] }>;
    };
  };
  duplicateCoverageError.published_readiness.source_coverage[0]!.explicit_errors = [
    { code: 'fixture_error' },
    { code: 'fixture_error' },
  ];
  reject(() => parseProject(duplicateCoverageError));
});

test('resolution preserves requested identity and canonical bounded provenance for every state', () => {
  const tombstoned = fixture.resolutions.tombstoned!;
  const reversed = clone(tombstoned) as unknown as { resolution: { provenance: unknown[] } };
  reversed.resolution.provenance.reverse();
  reject(() => parseCatalogEntityResolutionResponse(reversed, tombstoned.request));

  const superseded = fixture.resolutions.superseded!;
  const duplicateTarget = clone(superseded) as unknown as { resolution: { target_refs: unknown[] } };
  duplicateTarget.resolution.target_refs.push(clone(duplicateTarget.resolution.target_refs[0]));
  reject(() => parseCatalogEntityResolutionResponse(duplicateTarget, superseded.request));

  const live = fixture.resolutions.live!;
  const driftedIdentity = clone(live) as { request: { external_ref: unknown } };
  driftedIdentity.request.external_ref = clone(
    fixture.resolutions.unknown!.request as { external_ref: unknown },
  ).external_ref;
  reject(() => parseCatalogEntityResolutionResponse(driftedIdentity, live.request));

  const future = clone(fixture.resolutions.unknown!) as {
    resolution: Record<string, unknown>;
    request: unknown;
  };
  future.resolution = {
    state: 'typed_unknown',
    external_ref: clone((future.request as { external_ref: unknown }).external_ref),
    variant: 'future_resolution_state',
    payload: { available_later: true },
  };
  const parsed = parseCatalogEntityResolutionResponse(future, future.request);
  assert.equal(parsed.resolution.state, 'typed_unknown');
  if (parsed.resolution.state !== 'typed_unknown') throw new Error('expected typed-unknown resolution');
  assert.deepEqual(parsed.resolution.payload, { available_later: true });

  const session = clone(fixture.session_page) as unknown as {
    rows: Array<{
      row: {
        project_association: {
          selection: {
            association: Record<string, unknown> & {
              association_key: string;
              owner: { generation: number };
              project_ref: { kind: string; external_ref: unknown };
            };
            competing_associations: unknown[];
            conflicting_association_keys: string[];
          };
        };
      };
    }>;
  };
  const association = session.rows[0]!.row.project_association.selection.association;
  const competitor = clone(association);
  competitor.association_key = (session.rows[0]!.row as unknown as { assertion_keys: string[] }).assertion_keys[0]!;
  competitor.project_ref = {
    kind: 'project',
    external_ref: clone((fixture.resolutions.unknown!.request as { external_ref: unknown }).external_ref),
  };
  session.rows[0]!.row.project_association.selection.competing_associations = [competitor];
  reject(() => parseSession(session));
  session.rows[0]!.row.project_association.selection.conflicting_association_keys = [competitor.association_key];
  parseSession(session);

  association.owner.generation = 0;
  reject(() => parseSession(session));
});

test('attacker-sized arrays and unknown identity-bearing plan fields reject before traversal', () => {
  const oversizedPlan = clone(fixture.published_plan) as {
    required_sources: unknown[];
    optional_sources: unknown[];
  };
  oversizedPlan.required_sources = Array.from({ length: 4_097 }, () => null);
  oversizedPlan.optional_sources = [];
  assert.throws(() => parseCatalogPortableCoveragePlan(oversizedPlan), /exceeds/);

  const oversizedCoverage = clone(fixture.project_page) as unknown as {
    published_readiness: { source_coverage: unknown[] };
  };
  oversizedCoverage.published_readiness.source_coverage = [null, null];
  assert.throws(() => parseProject(oversizedCoverage), /exceeds/);

  const oversizedAssociations = clone(fixture.session_page) as unknown as {
    rows: Array<{
      row: { project_association: { selection: { competing_associations: unknown[] } } };
    }>;
  };
  oversizedAssociations.rows[0]!.row.project_association.selection.competing_associations = Array.from(
    { length: 4_097 },
    () => null,
  );
  assert.throws(() => parseSession(oversizedAssociations), /exceeds/);

  const oversizedTargets = clone(fixture.resolutions.superseded!) as unknown as {
    resolution: { target_refs: unknown[] };
    request: unknown;
  };
  oversizedTargets.resolution.target_refs = Array.from({ length: 4_097 }, () => null);
  assert.throws(() => parseCatalogEntityResolutionResponse(oversizedTargets, oversizedTargets.request), /exceeds/);

  const unknownSourceField = clone(fixture.published_plan) as {
    required_sources: Array<Record<string, unknown>>;
  };
  unknownSourceField.required_sources[0]!.future_source_identity = true;
  reject(() => parseCatalogPortableCoveragePlan(unknownSourceField));

  const unknownScopeField = clone(fixture.published_plan) as { scope: Record<string, unknown> };
  unknownScopeField.scope.future_scope_meaning = true;
  reject(() => parseCatalogPortableCoveragePlan(unknownScopeField));

  const unknownEntityReference = clone(fixture.project_page) as unknown as {
    rows: Array<{ row: { project_ref: { external_ref: Record<string, unknown> } } }>;
  };
  unknownEntityReference.rows[0]!.row.project_ref.external_ref.future_identity_meaning = true;
  reject(() => parseProject(unknownEntityReference));

  const unknownSemanticReference = clone(fixture.project_page) as unknown as {
    rows: Array<{
      row: {
        display_name: {
          selection: { field: { provenance: Array<Record<string, unknown>> } };
        };
      };
    }>;
  };
  unknownSemanticReference.rows[0]!.row.display_name.selection.field.provenance[0]!.future_revision_meaning = true;
  reject(() => parseProject(unknownSemanticReference));

  const unknownCoverageSet = clone(fixture.project_page) as unknown as {
    published_readiness: { source_coverage: Array<Record<string, unknown>> };
  };
  unknownCoverageSet.published_readiness.source_coverage[0]!.future_set_meaning = true;
  reject(() => parseProject(unknownCoverageSet));

  const unknownCoveragePoint = clone(fixture.project_page) as unknown as {
    published_readiness: {
      source_coverage: Array<{ points: Array<Record<string, unknown>> }>;
    };
  };
  unknownCoveragePoint.published_readiness.source_coverage[0]!.points[0]!.future_point_meaning = true;
  reject(() => parseProject(unknownCoveragePoint));

  const unknownCoverageScope = clone(fixture.project_page) as unknown as {
    published_readiness: {
      source_coverage: Array<{ scope: Record<string, unknown> }>;
    };
  };
  unknownCoverageScope.published_readiness.source_coverage[0]!.scope.future_scope_meaning = true;
  reject(() => parseProject(unknownCoverageScope));

  const oversizedUnavailableReason = clone(fixture.project_page) as unknown as {
    published_readiness: {
      source_coverage: Array<{
        completeness: string;
        points: Array<{ status: unknown }>;
      }>;
    };
  };
  oversizedUnavailableReason.published_readiness.source_coverage[0]!.completeness = 'partial';
  oversizedUnavailableReason.published_readiness.source_coverage[0]!.points[0]!.status = {
    kind: 'unavailable',
    reason: 'x'.repeat(1_025),
  };
  assert.throws(() => parseProject(oversizedUnavailableReason), /bounded/);

  const oversizedCoverageError = clone(fixture.project_page) as unknown as {
    published_readiness: {
      source_coverage: Array<{
        completeness: string;
        explicit_errors: unknown[];
      }>;
    };
  };
  oversizedCoverageError.published_readiness.source_coverage[0]!.completeness = 'partial';
  oversizedCoverageError.published_readiness.source_coverage[0]!.explicit_errors.push({
    code: 'x'.repeat(257),
  });
  assert.throws(() => parseProject(oversizedCoverageError), /bounded/);
});

test('SnapshotExpired never launders malformed/foreign cursors and requires a strictly newer same-lineage snapshot', () => {
  const parse = (value: unknown, expectedContinuation: unknown = fixture.continuation_request) =>
    parseCatalogSnapshotExpired(
      value,
      expectedContinuation,
      fixture.contract_selection,
      fixture.snapshot_expired.scope,
    );

  const malformedCursor = clone(fixture.snapshot_expired) as unknown as {
    request: { cursor: { query_fingerprint: string } };
  };
  malformedCursor.request.cursor.query_fingerprint = (
    fixture.resolutions.unknown!.request as { external_ref: { entity_key: string } }
  ).external_ref.entity_key;
  reject(() => parse(malformedCursor));

  const unknownCursorField = clone(fixture.snapshot_expired) as unknown as {
    request: { cursor: Record<string, unknown> };
  };
  unknownCursorField.request.cursor.future_cursor_meaning = true;
  reject(() => parse(unknownCursorField));

  const unknownSnapshotField = clone(fixture.snapshot_expired) as unknown as {
    request: { cursor: { snapshot_id: Record<string, unknown> } };
  };
  unknownSnapshotField.request.cursor.snapshot_id.future_snapshot_meaning = true;
  reject(() => parse(unknownSnapshotField));

  const foreignButValid = clone(fixture.snapshot_expired) as unknown as { request: { page_size: number } };
  foreignButValid.request.page_size = 2;
  reject(() => parse(foreignButValid));

  const equalLatest = clone(fixture.snapshot_expired) as unknown as {
    latest_snapshot: unknown;
    request: { snapshot_id: unknown };
  };
  equalLatest.latest_snapshot = clone(equalLatest.request.snapshot_id);
  reject(() => parse(equalLatest));

  const olderLatest = clone(fixture.snapshot_expired) as unknown as {
    latest_snapshot: { complete_commit: number };
    request: { snapshot_id: { complete_commit: number } };
  };
  olderLatest.latest_snapshot.complete_commit = olderLatest.request.snapshot_id.complete_commit - 1;
  reject(() => parse(olderLatest));

  const differentPack = clone(fixture.snapshot_expired) as unknown as {
    latest_snapshot: { pack_contract_version: number };
  };
  differentPack.latest_snapshot.pack_contract_version = 2;
  reject(() => parse(differentPack));

  const sameEpochDifferentPlan = clone(fixture.snapshot_expired) as unknown as {
    latest_snapshot: { readiness_epoch: number; coverage_plan_id: string };
    request: { snapshot_id: { readiness_epoch: number; coverage_plan_id: string } };
  };
  sameEpochDifferentPlan.latest_snapshot.readiness_epoch = sameEpochDifferentPlan.request.snapshot_id.readiness_epoch;
  assert.notEqual(
    sameEpochDifferentPlan.latest_snapshot.coverage_plan_id,
    sameEpochDifferentPlan.request.snapshot_id.coverage_plan_id,
  );
  reject(() => parse(sameEpochDifferentPlan));

  const driftedSelection = clone(fixture.snapshot_expired) as unknown as {
    contract_selection: { typed_unknown: { max_payload_bytes: number } };
  };
  driftedSelection.contract_selection.typed_unknown.max_payload_bytes = 8_192;
  reject(() => parse(driftedSelection));
});

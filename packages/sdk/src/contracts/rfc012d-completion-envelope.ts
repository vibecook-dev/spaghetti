/** Strict contextual RFC 012D bootstrap/resync completion envelopes.
 *
 * Rust alone mints the consumer context and derives event/barrier digests. The
 * portable parser checks those opaque values against that caller-held context
 * and composes the existing strict capability, replacement, scope, artifact,
 * source-coverage, and ordered-envelope contracts. This does not authorize
 * source access or expose a native observer transport.
 */

import {
  ContractValidationError,
  parseOpaqueContractReference,
  parseSourceCoverageSet,
  type CoverageError,
  type OpaqueContractReference,
  type SourceCoverageSet,
} from './rfc012a.js';
import {
  parseScopedArtifactAvailabilityContext,
  parseScopedArtifactAvailabilitySnapshot,
  type ScopedArtifactAvailabilityContext,
  type ScopedArtifactAvailabilitySnapshot,
} from './rfc012d-artifact-availability.js';
import {
  parseScopedCapabilitySnapshot,
  parseScopedCapabilitySnapshotContext,
  type ScopedCapabilitySnapshot,
  type ScopedCapabilitySnapshotContext,
} from './rfc012d-capability-snapshot.js';
import {
  parseScopedReplacementManifest,
  parseScopedReplacementManifestContext,
  type ScopedReplacementManifest,
  type ScopedReplacementManifestContext,
} from './rfc012d-replacement-manifest.js';
import {
  parseScopedScopeCoverage,
  parseScopedScopeCoverageContext,
  type ScopedScopeCoverage,
  type ScopedScopeCoverageContext,
} from './rfc012d-scope-coverage.js';
import {
  parseScopedSourceEnvelope,
  type ScopedSourceActor,
  type ScopedSourceAffiliations,
  type ScopedSourceCoordinate,
} from './rfc012d-source-envelope.js';
import { parseScopedUsageRoot, type ScopedUsageRoot, type ScopedUsageSourceBinding } from './rfc012d-usage-envelope.js';
import { parseObservationContractSelectionForExpected, type ObservationContractSelection } from './rfc012d.js';

export const SCOPED_COMPLETION_ENVELOPE_CONTRACT_VERSION = 1 as const;
export const SCOPED_COMPLETION_BARRIER_CONTRACT_VERSION = 3 as const;

const MAX_SOURCE_COVERAGE_SETS = 64;
const MAX_COVERAGE_ERRORS_PER_SET = 4_096;
const MAX_EXPLICIT_OBJECT_ERRORS = MAX_SOURCE_COVERAGE_SETS * MAX_COVERAGE_ERRORS_PER_SET;
type UnknownRecord = Record<string, unknown>;

export interface ScopedCompletionQueueState {
  scope_epoch: number;
  offered_through_sequence: number;
  delivered_through_sequence: number;
  continuity: 'valid';
  queued_semantic_events: number;
  queued_retained_native_bytes: number;
  queued_source_control_items: number;
}

export type ScopedExpectedCompletionBarrier =
  | {
      kind: 'bootstrap';
      snapshot_digest: OpaqueContractReference;
      replacement_snapshot_digest: OpaqueContractReference;
    }
  | {
      kind: 'resync';
      started_control_sequence: number;
      coverage_snapshot_digest: OpaqueContractReference;
      replacement_snapshot_digest: OpaqueContractReference;
    };

export interface ScopedCompletionEnvelopeContext {
  contract_selection: ObservationContractSelection;
  adapter_id: string;
  root: ScopedUsageRoot;
  expected_source: ScopedUsageSourceBinding;
  expected_observer_sequence: number;
  expected_scope_epoch: number;
  expected_event_id: OpaqueContractReference;
  expected_observed_at: number;
  expected_phase: 'bootstrap' | 'correction';
  expected_barrier: ScopedExpectedCompletionBarrier;
  expected_queue_state: ScopedCompletionQueueState;
  expected_root_present: boolean;
  capability_context: ScopedCapabilitySnapshotContext;
  replacement_manifest_context: ScopedReplacementManifestContext;
  scope_coverage_context: ScopedScopeCoverageContext;
  artifact_availability_context: ScopedArtifactAvailabilityContext;
}

export interface ScopedBootstrapCompletionBarrier {
  barrier_contract_version: typeof SCOPED_COMPLETION_BARRIER_CONTRACT_VERSION;
  scope_epoch: number;
  barrier_sequence: number;
  snapshot_digest: OpaqueContractReference;
  replacement_snapshot_digest: OpaqueContractReference;
  replacement_manifest: ScopedReplacementManifest;
  capability_snapshot: ScopedCapabilitySnapshot;
  source_coverage: SourceCoverageSet[];
  scope_coverage: ScopedScopeCoverage;
  explicit_object_errors: CoverageError[];
  artifact_availability: ScopedArtifactAvailabilitySnapshot;
  queue_state: ScopedCompletionQueueState;
  root_present: boolean;
}

export interface ScopedResyncCompletionBarrier {
  barrier_contract_version: typeof SCOPED_COMPLETION_BARRIER_CONTRACT_VERSION;
  scope_epoch: number;
  replacement: 'full_snapshot';
  started_control_sequence: number;
  barrier_sequence: number;
  replacement_snapshot_digest: OpaqueContractReference;
  coverage_snapshot_digest: OpaqueContractReference;
  replacement_manifest: ScopedReplacementManifest;
  capability_snapshot: ScopedCapabilitySnapshot;
  source_coverage: SourceCoverageSet[];
  scope_coverage: ScopedScopeCoverage;
  explicit_object_errors: CoverageError[];
  artifact_availability: ScopedArtifactAvailabilitySnapshot;
  queue_state: ScopedCompletionQueueState;
  root_present: boolean;
}

export type ScopedCompletionEvent =
  | { kind: 'observer_bootstrap_complete'; barrier: ScopedBootstrapCompletionBarrier }
  | { kind: 'observer_resync_complete'; barrier: ScopedResyncCompletionBarrier };

export interface ScopedCompletionEnvelope {
  scoped_completion_envelope_contract_version: typeof SCOPED_COMPLETION_ENVELOPE_CONTRACT_VERSION;
  contract_version: number;
  contract_selection: ObservationContractSelection;
  observer_sequence: number;
  scope_epoch: number;
  event_id: OpaqueContractReference;
  semantic_revision_ref: null;
  root: ScopedUsageRoot;
  actor: ScopedSourceActor;
  actor_attribution: { kind: 'scope_fallback'; reason: 'observer_lifecycle_control' };
  affiliations: ScopedSourceAffiliations;
  source: ScopedSourceCoordinate;
  native_time: null;
  observed_at: number;
  phase: 'bootstrap' | 'correction';
  evidence: {
    authority: 'engine_control';
    quality: 'derived';
    effective_at: null;
    completeness: 'complete';
  };
  event: ScopedCompletionEvent;
  native_evidence: { kind: 'engine_control' };
}

function record(value: unknown, label: string): UnknownRecord {
  if (value === null || typeof value !== 'object' || Array.isArray(value)) {
    throw new ContractValidationError(`${label} must be an object`);
  }
  const prototype = Object.getPrototypeOf(value);
  if (prototype !== Object.prototype && prototype !== null) {
    throw new ContractValidationError(`${label} must be a plain JSON object`);
  }
  return value as UnknownRecord;
}

function exactRecord(value: unknown, fields: readonly string[], label: string): UnknownRecord {
  const input = record(value, label);
  const known = new Set(fields);
  for (const key of Object.keys(input)) {
    if (!known.has(key)) throw new ContractValidationError(`${label} contains unknown field ${key}`);
  }
  for (const field of fields) {
    if (!Object.hasOwn(input, field)) throw new ContractValidationError(`${label} is missing field ${field}`);
  }
  return input;
}

function safeInteger(value: unknown, label: string): number {
  if (typeof value !== 'number' || !Number.isSafeInteger(value)) {
    throw new ContractValidationError(`${label} must be a portable integer`);
  }
  return value;
}

function positiveInteger(value: unknown, label: string): number {
  const parsed = safeInteger(value, label);
  if (parsed <= 0) throw new ContractValidationError(`${label} must be positive`);
  return parsed;
}

function nonnegativeInteger(value: unknown, label: string): number {
  const parsed = safeInteger(value, label);
  if (parsed < 0) throw new ContractValidationError(`${label} must be non-negative`);
  return parsed;
}

function fixedNonzeroOpaque(value: unknown, label: string): OpaqueContractReference {
  const parsed = parseOpaqueContractReference(value, label);
  const encoded = parsed.slice(3);
  if (encoded.length !== 43 || encoded.includes('=') || !/^[A-Za-z0-9_-]+$/.test(encoded)) {
    throw new ContractValidationError(`${label} must contain 32 canonical nonzero bytes`);
  }
  const standard = encoded.replace(/-/g, '+').replace(/_/g, '/');
  const padded = standard.padEnd(Math.ceil(standard.length / 4) * 4, '=');
  let binary: string;
  try {
    binary = atob(padded);
  } catch {
    throw new ContractValidationError(`${label} is not canonical base64url`);
  }
  let roundTrip = '';
  let nonzero = false;
  for (let index = 0; index < binary.length; index += 1) {
    const byte = binary.charCodeAt(index);
    roundTrip += String.fromCharCode(byte);
    nonzero ||= byte !== 0;
  }
  const canonical = btoa(roundTrip).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/g, '');
  if (binary.length !== 32 || canonical !== encoded || !nonzero) {
    throw new ContractValidationError(`${label} must contain 32 canonical nonzero bytes`);
  }
  return parsed;
}

function canonicalJson(value: unknown): string {
  if (Array.isArray(value)) return `[${value.map(canonicalJson).join(',')}]`;
  if (value !== null && typeof value === 'object') {
    const input = value as UnknownRecord;
    return `{${Object.keys(input)
      .sort()
      .map((key) => `${JSON.stringify(key)}:${canonicalJson(input[key])}`)
      .join(',')}}`;
  }
  return JSON.stringify(value);
}

function sameValue(left: unknown, right: unknown): boolean {
  return canonicalJson(left) === canonicalJson(right);
}

function parseSourceBinding(value: unknown): ScopedUsageSourceBinding {
  const input = exactRecord(value, ['instance_key', 'stream_key', 'object_key'], 'completion source binding');
  return {
    instance_key: fixedNonzeroOpaque(input.instance_key, 'completion source instance key'),
    stream_key: fixedNonzeroOpaque(input.stream_key, 'completion source stream key'),
    object_key: fixedNonzeroOpaque(input.object_key, 'completion source object key'),
  };
}

function parseQueueState(value: unknown): ScopedCompletionQueueState {
  const input = exactRecord(
    value,
    [
      'scope_epoch',
      'offered_through_sequence',
      'delivered_through_sequence',
      'continuity',
      'queued_semantic_events',
      'queued_retained_native_bytes',
      'queued_source_control_items',
    ],
    'completion queue state',
  );
  const result: ScopedCompletionQueueState = {
    scope_epoch: positiveInteger(input.scope_epoch, 'queue scope_epoch'),
    offered_through_sequence: positiveInteger(input.offered_through_sequence, 'queue offered sequence'),
    delivered_through_sequence: nonnegativeInteger(input.delivered_through_sequence, 'queue delivered sequence'),
    continuity:
      input.continuity === 'valid'
        ? 'valid'
        : (() => {
            throw new ContractValidationError('completion queue continuity must be valid');
          })(),
    queued_semantic_events: nonnegativeInteger(input.queued_semantic_events, 'queued semantic events'),
    queued_retained_native_bytes: nonnegativeInteger(
      input.queued_retained_native_bytes,
      'queued retained-native bytes',
    ),
    queued_source_control_items: nonnegativeInteger(input.queued_source_control_items, 'queued source-control items'),
  };
  if (
    result.delivered_through_sequence >= result.offered_through_sequence ||
    result.queued_source_control_items === 0
  ) {
    throw new ContractValidationError('completion queue does not retain the offered barrier');
  }
  return result;
}

function parseExpectedBarrier(value: unknown): ScopedExpectedCompletionBarrier {
  const input = record(value, 'expected completion barrier');
  if (input.kind === 'bootstrap') {
    const exact = exactRecord(
      input,
      ['kind', 'snapshot_digest', 'replacement_snapshot_digest'],
      'expected bootstrap barrier',
    );
    return {
      kind: 'bootstrap',
      snapshot_digest: fixedNonzeroOpaque(exact.snapshot_digest, 'expected bootstrap digest'),
      replacement_snapshot_digest: fixedNonzeroOpaque(
        exact.replacement_snapshot_digest,
        'expected bootstrap replacement digest',
      ),
    };
  }
  if (input.kind === 'resync') {
    const exact = exactRecord(
      input,
      ['kind', 'started_control_sequence', 'coverage_snapshot_digest', 'replacement_snapshot_digest'],
      'expected resync barrier',
    );
    return {
      kind: 'resync',
      started_control_sequence: positiveInteger(exact.started_control_sequence, 'expected resync start sequence'),
      coverage_snapshot_digest: fixedNonzeroOpaque(exact.coverage_snapshot_digest, 'expected resync coverage digest'),
      replacement_snapshot_digest: fixedNonzeroOpaque(
        exact.replacement_snapshot_digest,
        'expected resync replacement digest',
      ),
    };
  }
  throw new ContractValidationError('unsupported expected completion barrier');
}

function assertSelectionEqual(
  value: ObservationContractSelection,
  expected: ObservationContractSelection,
  label: string,
): void {
  if (!sameValue(value, expected)) throw new ContractValidationError(`${label} selection drifted`);
}

export function parseScopedCompletionEnvelopeContext(value: unknown): ScopedCompletionEnvelopeContext {
  const input = exactRecord(
    value,
    [
      'contract_selection',
      'adapter_id',
      'root',
      'expected_source',
      'expected_observer_sequence',
      'expected_scope_epoch',
      'expected_event_id',
      'expected_observed_at',
      'expected_phase',
      'expected_barrier',
      'expected_queue_state',
      'expected_root_present',
      'capability_context',
      'replacement_manifest_context',
      'scope_coverage_context',
      'artifact_availability_context',
    ],
    'scoped completion envelope context',
  );
  const contractSelection = parseObservationContractSelectionForExpected(
    input.contract_selection,
    input.contract_selection,
  );
  const root = parseScopedUsageRoot(input.root);
  const expectedSource = parseSourceBinding(input.expected_source);
  const observerSequence = positiveInteger(input.expected_observer_sequence, 'expected observer sequence');
  const scopeEpoch = positiveInteger(input.expected_scope_epoch, 'expected scope epoch');
  const queueState = parseQueueState(input.expected_queue_state);
  const expectedBarrier = parseExpectedBarrier(input.expected_barrier);
  const expectedPhase = expectedBarrier.kind === 'bootstrap' ? 'bootstrap' : 'correction';
  const capabilityContext = parseScopedCapabilitySnapshotContext(input.capability_context);
  const replacementContext = parseScopedReplacementManifestContext(input.replacement_manifest_context);
  const scopeContext = parseScopedScopeCoverageContext(input.scope_coverage_context);
  const artifactContext = parseScopedArtifactAvailabilityContext(input.artifact_availability_context);
  if (typeof input.adapter_id !== 'string' || input.adapter_id !== scopeContext.root.adapter_id) {
    throw new ContractValidationError('completion context adapter does not match the scoped authority');
  }
  if (
    input.expected_phase !== expectedPhase ||
    queueState.scope_epoch !== scopeEpoch ||
    queueState.offered_through_sequence !== observerSequence ||
    (expectedBarrier.kind === 'resync' && expectedBarrier.started_control_sequence >= observerSequence) ||
    expectedSource.instance_key !== scopeContext.root.source_instance_key ||
    scopeContext.root.session_key !== root.session_key ||
    artifactContext.root_session_key !== root.session_key ||
    typeof input.expected_root_present !== 'boolean'
  ) {
    throw new ContractValidationError('completion context does not describe one exact barrier authority');
  }
  assertSelectionEqual(capabilityContext.contract_selection, contractSelection, 'capability context');
  assertSelectionEqual(replacementContext.contract_selection, contractSelection, 'replacement context');
  assertSelectionEqual(artifactContext.contract_selection, contractSelection, 'artifact context');
  if (
    replacementContext.source_coverage.length === 0 ||
    replacementContext.source_coverage.length > MAX_SOURCE_COVERAGE_SETS ||
    replacementContext.source_coverage.some(
      (set) =>
        set.scope.adapter_id !== input.adapter_id ||
        set.scope.source_instance_key !== expectedSource.instance_key ||
        set.scope.root_entity_key !== root.session_key,
    )
  ) {
    throw new ContractValidationError('completion context source coverage is not scoped to the authorized root');
  }
  const decodeSets = replacementContext.source_coverage.filter((set) => set.coverage_domain.kind === 'decode');
  if (
    decodeSets.length !== 1 ||
    !sameValue(decodeSets[0], scopeContext.decode_coverage) ||
    replacementContext.source_coverage.some(
      (set) => set.scope.support_release_id !== capabilityContext.support_release_id,
    )
  ) {
    throw new ContractValidationError('completion context coverage is not bound to its scope and support authority');
  }
  return {
    contract_selection: contractSelection,
    adapter_id: input.adapter_id,
    root,
    expected_source: expectedSource,
    expected_observer_sequence: observerSequence,
    expected_scope_epoch: scopeEpoch,
    expected_event_id: fixedNonzeroOpaque(input.expected_event_id, 'expected completion event id'),
    expected_observed_at: safeInteger(input.expected_observed_at, 'expected completion observed_at'),
    expected_phase: expectedPhase,
    expected_barrier: expectedBarrier,
    expected_queue_state: queueState,
    expected_root_present: input.expected_root_present,
    capability_context: capabilityContext,
    replacement_manifest_context: replacementContext,
    scope_coverage_context: scopeContext,
    artifact_availability_context: artifactContext,
  };
}

function parseSourceCoverage(value: unknown, context: ScopedCompletionEnvelopeContext): SourceCoverageSet[] {
  if (
    !Array.isArray(value) ||
    value.length === 0 ||
    value.length > MAX_SOURCE_COVERAGE_SETS ||
    value.length !== context.replacement_manifest_context.source_coverage.length
  ) {
    throw new ContractValidationError('completion source coverage does not have the exact bounded set count');
  }
  const parsed = value.map(parseSourceCoverageSet);
  if (!sameValue(parsed, context.replacement_manifest_context.source_coverage)) {
    throw new ContractValidationError('completion source coverage does not match caller-held coverage');
  }
  return parsed;
}

function canonicalExplicitErrors(sourceCoverage: SourceCoverageSet[]): CoverageError[] {
  const count = sourceCoverage.reduce((total, set) => total + set.explicit_errors.length, 0);
  if (!Number.isSafeInteger(count) || count > MAX_EXPLICIT_OBJECT_ERRORS) {
    throw new ContractValidationError('completion explicit errors exceed the portable bound');
  }
  const keyed = new Map<string, CoverageError>();
  for (const set of sourceCoverage) {
    for (const error of set.explicit_errors) {
      const key = JSON.stringify([error.stream_key ?? null, error.object_key ?? null, error.code]);
      keyed.set(key, error);
    }
  }
  return [...keyed.values()].sort((left, right) => {
    const leftKey = [left.stream_key ?? '', left.object_key ?? '', left.code];
    const rightKey = [right.stream_key ?? '', right.object_key ?? '', right.code];
    for (let index = 0; index < leftKey.length; index += 1) {
      if (leftKey[index]! < rightKey[index]!) return -1;
      if (leftKey[index]! > rightKey[index]!) return 1;
    }
    return 0;
  });
}

function parseExplicitError(value: unknown): CoverageError {
  const input = record(value, 'completion explicit object error');
  for (const key of Object.keys(input)) {
    if (key !== 'stream_key' && key !== 'object_key' && key !== 'code') {
      throw new ContractValidationError(`completion explicit object error contains unknown field ${key}`);
    }
  }
  if (!Object.hasOwn(input, 'code')) {
    throw new ContractValidationError('completion explicit object error is missing its code');
  }
  if (
    typeof input.code !== 'string' ||
    input.code.length === 0 ||
    input.code.length > 64 ||
    !/^[a-z][a-z0-9_]*$/.test(input.code)
  ) {
    throw new ContractValidationError('completion explicit object error code is not a bounded machine code');
  }
  const result: CoverageError = { code: input.code };
  if (Object.hasOwn(input, 'stream_key')) {
    result.stream_key = fixedNonzeroOpaque(input.stream_key, 'completion error stream key');
  }
  if (Object.hasOwn(input, 'object_key')) {
    if (result.stream_key === undefined) {
      throw new ContractValidationError('completion error object key requires a stream key');
    }
    result.object_key = fixedNonzeroOpaque(input.object_key, 'completion error object key');
  }
  return result;
}

function parseExplicitErrors(value: unknown, sourceCoverage: SourceCoverageSet[]): CoverageError[] {
  const expected = canonicalExplicitErrors(sourceCoverage);
  if (!Array.isArray(value) || value.length !== expected.length || value.length > MAX_EXPLICIT_OBJECT_ERRORS) {
    throw new ContractValidationError('completion explicit errors are not the canonical coverage error set');
  }
  const parsed = value.map(parseExplicitError);
  if (!sameValue(parsed, expected)) {
    throw new ContractValidationError('completion explicit errors are not the canonical coverage error set');
  }
  return parsed;
}

function parseBarrierComponents(value: UnknownRecord, context: ScopedCompletionEnvelopeContext) {
  const sourceCoverage = parseSourceCoverage(value.source_coverage, context);
  return {
    replacement_manifest: parseScopedReplacementManifest(
      value.replacement_manifest,
      context.replacement_manifest_context,
    ),
    capability_snapshot: parseScopedCapabilitySnapshot(value.capability_snapshot, context.capability_context),
    source_coverage: sourceCoverage,
    scope_coverage: parseScopedScopeCoverage(value.scope_coverage, context.scope_coverage_context),
    explicit_object_errors: parseExplicitErrors(value.explicit_object_errors, sourceCoverage),
    artifact_availability: parseScopedArtifactAvailabilitySnapshot(
      value.artifact_availability,
      context.artifact_availability_context,
    ),
    queue_state: parseQueueState(value.queue_state),
  };
}

function parseCompletionEvent(value: unknown, context: ScopedCompletionEnvelopeContext): ScopedCompletionEvent {
  const input = exactRecord(value, ['kind', 'barrier'], 'completion event');
  if (input.kind === 'observer_bootstrap_complete' && context.expected_barrier.kind === 'bootstrap') {
    const barrier = exactRecord(
      input.barrier,
      [
        'barrier_contract_version',
        'scope_epoch',
        'barrier_sequence',
        'snapshot_digest',
        'replacement_snapshot_digest',
        'replacement_manifest',
        'capability_snapshot',
        'source_coverage',
        'scope_coverage',
        'explicit_object_errors',
        'artifact_availability',
        'queue_state',
        'root_present',
      ],
      'bootstrap completion barrier',
    );
    const scopeEpoch = positiveInteger(barrier.scope_epoch, 'bootstrap scope epoch');
    const sequence = positiveInteger(barrier.barrier_sequence, 'bootstrap barrier sequence');
    const snapshotDigest = fixedNonzeroOpaque(barrier.snapshot_digest, 'bootstrap snapshot digest');
    const replacementDigest = fixedNonzeroOpaque(barrier.replacement_snapshot_digest, 'bootstrap replacement digest');
    const components = parseBarrierComponents(barrier, context);
    if (
      barrier.barrier_contract_version !== SCOPED_COMPLETION_BARRIER_CONTRACT_VERSION ||
      scopeEpoch !== context.expected_scope_epoch ||
      sequence !== context.expected_observer_sequence ||
      snapshotDigest !== context.expected_barrier.snapshot_digest ||
      replacementDigest !== context.expected_barrier.replacement_snapshot_digest ||
      !sameValue(components.queue_state, context.expected_queue_state) ||
      barrier.root_present !== context.expected_root_present
    ) {
      throw new ContractValidationError('bootstrap completion barrier does not match caller-held context');
    }
    return {
      kind: 'observer_bootstrap_complete',
      barrier: {
        barrier_contract_version: SCOPED_COMPLETION_BARRIER_CONTRACT_VERSION,
        scope_epoch: scopeEpoch,
        barrier_sequence: sequence,
        snapshot_digest: snapshotDigest,
        replacement_snapshot_digest: replacementDigest,
        ...components,
        root_present: context.expected_root_present,
      },
    };
  }
  if (input.kind === 'observer_resync_complete' && context.expected_barrier.kind === 'resync') {
    const barrier = exactRecord(
      input.barrier,
      [
        'barrier_contract_version',
        'scope_epoch',
        'replacement',
        'started_control_sequence',
        'barrier_sequence',
        'replacement_snapshot_digest',
        'coverage_snapshot_digest',
        'replacement_manifest',
        'capability_snapshot',
        'source_coverage',
        'scope_coverage',
        'explicit_object_errors',
        'artifact_availability',
        'queue_state',
        'root_present',
      ],
      'resync completion barrier',
    );
    const scopeEpoch = positiveInteger(barrier.scope_epoch, 'resync scope epoch');
    const startedSequence = positiveInteger(barrier.started_control_sequence, 'resync started sequence');
    const sequence = positiveInteger(barrier.barrier_sequence, 'resync barrier sequence');
    const replacementDigest = fixedNonzeroOpaque(barrier.replacement_snapshot_digest, 'resync replacement digest');
    const coverageDigest = fixedNonzeroOpaque(barrier.coverage_snapshot_digest, 'resync coverage digest');
    const components = parseBarrierComponents(barrier, context);
    if (
      barrier.barrier_contract_version !== SCOPED_COMPLETION_BARRIER_CONTRACT_VERSION ||
      barrier.replacement !== 'full_snapshot' ||
      scopeEpoch !== context.expected_scope_epoch ||
      startedSequence !== context.expected_barrier.started_control_sequence ||
      startedSequence >= sequence ||
      sequence !== context.expected_observer_sequence ||
      replacementDigest !== context.expected_barrier.replacement_snapshot_digest ||
      coverageDigest !== context.expected_barrier.coverage_snapshot_digest ||
      !sameValue(components.queue_state, context.expected_queue_state) ||
      barrier.root_present !== context.expected_root_present
    ) {
      throw new ContractValidationError('resync completion barrier does not match caller-held context');
    }
    return {
      kind: 'observer_resync_complete',
      barrier: {
        barrier_contract_version: SCOPED_COMPLETION_BARRIER_CONTRACT_VERSION,
        scope_epoch: scopeEpoch,
        replacement: 'full_snapshot',
        started_control_sequence: startedSequence,
        barrier_sequence: sequence,
        replacement_snapshot_digest: replacementDigest,
        coverage_snapshot_digest: coverageDigest,
        ...components,
        root_present: context.expected_root_present,
      },
    };
  }
  throw new ContractValidationError('completion event kind does not match caller-held barrier context');
}

function parseCommonEnvelope(
  input: UnknownRecord,
  context: ScopedCompletionEnvelopeContext,
): ReturnType<typeof parseScopedSourceEnvelope> {
  const proxy: UnknownRecord = { ...input };
  delete proxy.scoped_completion_envelope_contract_version;
  proxy.scoped_source_envelope_contract_version = 1;
  proxy.actor_attribution = { kind: 'scope_fallback', reason: 'source_lifecycle_control' };
  proxy.event = { kind: 'source_created', generation: context.expected_scope_epoch };
  return parseScopedSourceEnvelope(proxy, {
    contract_selection: context.contract_selection,
    root: context.root,
    authorized_sources: [context.expected_source],
  });
}

export function parseScopedCompletionEnvelope(value: unknown, expectedContextInput: unknown): ScopedCompletionEnvelope {
  const context = parseScopedCompletionEnvelopeContext(expectedContextInput);
  const fields = [
    'scoped_completion_envelope_contract_version',
    'contract_version',
    'contract_selection',
    'observer_sequence',
    'scope_epoch',
    'event_id',
    'semantic_revision_ref',
    'root',
    'actor',
    'actor_attribution',
    'affiliations',
    'source',
    'native_time',
    'observed_at',
    'phase',
    'evidence',
    'event',
    'native_evidence',
  ];
  const input = exactRecord(value, fields, 'scoped completion envelope');
  const common = parseCommonEnvelope(input, context);
  const attribution = exactRecord(input.actor_attribution, ['kind', 'reason'], 'completion actor attribution');
  const evidence = exactRecord(
    input.evidence,
    ['authority', 'quality', 'effective_at', 'completeness'],
    'completion evidence',
  );
  const nativeEvidence = exactRecord(input.native_evidence, ['kind'], 'completion native evidence');
  const eventId = fixedNonzeroOpaque(input.event_id, 'completion event id');
  if (
    input.scoped_completion_envelope_contract_version !== SCOPED_COMPLETION_ENVELOPE_CONTRACT_VERSION ||
    common.contract_version !== context.contract_selection.envelope_contract_version ||
    common.observer_sequence !== context.expected_observer_sequence ||
    common.scope_epoch !== context.expected_scope_epoch ||
    eventId !== context.expected_event_id ||
    common.observed_at !== context.expected_observed_at ||
    common.phase !== context.expected_phase ||
    attribution.kind !== 'scope_fallback' ||
    attribution.reason !== 'observer_lifecycle_control' ||
    evidence.authority !== 'engine_control' ||
    evidence.quality !== 'derived' ||
    evidence.effective_at !== null ||
    evidence.completeness !== 'complete' ||
    nativeEvidence.kind !== 'engine_control'
  ) {
    throw new ContractValidationError('completion envelope does not match caller-held ordered context');
  }
  const event = parseCompletionEvent(input.event, context);
  return {
    scoped_completion_envelope_contract_version: SCOPED_COMPLETION_ENVELOPE_CONTRACT_VERSION,
    contract_version: common.contract_version,
    contract_selection: common.contract_selection,
    observer_sequence: common.observer_sequence,
    scope_epoch: common.scope_epoch,
    event_id: eventId,
    semantic_revision_ref: null,
    root: common.root,
    actor: common.actor,
    actor_attribution: { kind: 'scope_fallback', reason: 'observer_lifecycle_control' },
    affiliations: common.affiliations,
    source: common.source,
    native_time: null,
    observed_at: common.observed_at,
    phase: context.expected_phase,
    evidence: {
      authority: 'engine_control',
      quality: 'derived',
      effective_at: null,
      completeness: 'complete',
    },
    event,
    native_evidence: { kind: 'engine_control' },
  };
}

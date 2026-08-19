/** Strict RFC 012D wire projection for observer continuity controls.
 *
 * This deliberately incomplete contract covers only resync-required,
 * resync-started, and terminal observer-failed controls. Consumption requires
 * caller-held selection, root, derived control source, epoch/watermark,
 * baseline snapshot, and (for replacement start) the delivered invalidation.
 * Rust remains authoritative for deterministic event-ID recomputation.
 */

import { ContractValidationError, parseOpaqueContractReference } from './rfc012a.js';
import { parseObservationContractSelectionForExpected, type ObservationContractSelection } from './rfc012d.js';
import {
  parseScopedUsageEnvelopeContext,
  type ScopedUsageActor,
  type ScopedUsageAffiliations,
  type ScopedUsageRoot,
  type ScopedUsageSourceBinding,
} from './rfc012d-usage-envelope.js';

export const SCOPED_CONTINUITY_ENVELOPE_CONTRACT_VERSION = 1 as const;

type UnknownRecord = Record<string, unknown>;
export type ScopedContinuityPhase = 'bootstrap' | 'live' | 'correction';
export type ScopedResyncReason = 'watcher_overflow' | 'transport_continuity_loss' | 'explicit_consumer_request';
export type ScopedObserverFailureReason =
  | 'native_watcher_recovery_exhausted'
  | 'native_watcher_routing_failed'
  | 'internal_control_failure';

export type ScopedContinuityRoot = ScopedUsageRoot;
export type ScopedContinuityActor = ScopedUsageActor;
export type ScopedContinuityAffiliations = ScopedUsageAffiliations;
export type ScopedContinuitySourceBinding = ScopedUsageSourceBinding;

export interface ScopedContinuitySource extends ScopedContinuitySourceBinding {
  locator_id: null;
  generation: number;
  source_record_id: null;
  record_index: null;
  cursor_start: null;
  cursor_end: null;
  byte_range: null;
}

export interface ScopedResyncRequiredControl {
  kind: 'observer_resync_required';
  invalid_scope_epoch: number;
  control_sequence: number;
  last_contiguous_sequence: number;
  baseline_snapshot_digest: string;
  reason: ScopedResyncReason;
  discarded_semantic_events: number;
  discarded_source_controls: number;
  discarded_retained_native_bytes: number;
}

export interface ScopedResyncStartedControl {
  kind: 'observer_resync_started';
  old_scope_epoch: number;
  new_scope_epoch: number;
  control_sequence: number;
  required_control_sequence: number;
  baseline_snapshot_digest: string;
  reason: ScopedResyncReason;
  replacement: 'full_snapshot';
}

export interface ScopedObserverFailedControl {
  kind: 'observer_failed';
  failed_scope_epoch: number;
  control_sequence: number;
  last_contiguous_sequence: number;
  phase: ScopedContinuityPhase;
  reason: ScopedObserverFailureReason;
  discarded_semantic_events: number;
  discarded_source_controls: number;
  discarded_retained_native_bytes: number;
}

export type ScopedContinuityControl =
  | ScopedResyncRequiredControl
  | ScopedResyncStartedControl
  | ScopedObserverFailedControl;

export interface ScopedContinuityConsumerState {
  current_scope_epoch: number;
  last_contiguous_sequence: number;
  baseline_snapshot_digest: string | null;
  phase: ScopedContinuityPhase;
  prior_resync_required: ScopedResyncRequiredControl | null;
}

export interface ScopedContinuityEnvelopeContext {
  contract_selection: ObservationContractSelection;
  root: ScopedContinuityRoot;
  control_source: ScopedContinuitySourceBinding;
  state: ScopedContinuityConsumerState;
}

export interface ScopedContinuityEnvelope {
  scoped_continuity_envelope_contract_version: typeof SCOPED_CONTINUITY_ENVELOPE_CONTRACT_VERSION;
  contract_version: number;
  contract_selection: ObservationContractSelection;
  observer_sequence: number;
  scope_epoch: number;
  event_id: string;
  semantic_revision_ref: null;
  root: ScopedContinuityRoot;
  actor: ScopedContinuityActor;
  actor_attribution: { kind: 'scope_fallback'; reason: 'observer_lifecycle_control' };
  affiliations: ScopedContinuityAffiliations;
  source: ScopedContinuitySource;
  native_time: null;
  observed_at: number;
  phase: ScopedContinuityPhase;
  evidence: {
    authority: 'engine_control';
    quality: 'derived';
    effective_at: null;
    completeness: 'complete';
  };
  event: ScopedContinuityControl;
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

function nonnegativeSafeInteger(value: unknown, label: string): number {
  const parsed = safeInteger(value, label);
  if (parsed < 0) throw new ContractValidationError(`${label} must be nonnegative`);
  return parsed;
}

function positiveSafeInteger(value: unknown, label: string): number {
  const parsed = safeInteger(value, label);
  if (parsed <= 0) throw new ContractValidationError(`${label} must be positive`);
  return parsed;
}

function decodeFixedOpaque(value: unknown, label: string): string {
  if (typeof value !== 'string' || !value.startsWith('v1:')) {
    throw new ContractValidationError(`${label} is not a v1 opaque reference`);
  }
  const encoded = value.slice(3);
  if (encoded.length === 0 || encoded.includes('=') || !/^[A-Za-z0-9_-]+$/.test(encoded)) {
    throw new ContractValidationError(`${label} is not canonical base64url`);
  }
  const standard = encoded.replace(/-/g, '+').replace(/_/g, '/');
  const padded = standard.padEnd(Math.ceil(standard.length / 4) * 4, '=');
  let binary: string;
  try {
    binary = atob(padded);
  } catch {
    throw new ContractValidationError(`${label} is not canonical base64url`);
  }
  const bytes = Uint8Array.from(binary, (character) => character.charCodeAt(0));
  let roundTrip = '';
  for (const byte of bytes) roundTrip += String.fromCharCode(byte);
  const canonical = btoa(roundTrip).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/g, '');
  if (bytes.byteLength !== 32 || canonical !== encoded) {
    throw new ContractValidationError(`${label} must contain exactly 32 canonical bytes`);
  }
  return value;
}

function phase(value: unknown, label: string): ScopedContinuityPhase {
  if (value !== 'bootstrap' && value !== 'live' && value !== 'correction') {
    throw new ContractValidationError(`${label} is unsupported`);
  }
  return value;
}

function resyncReason(value: unknown): ScopedResyncReason {
  if (value !== 'watcher_overflow' && value !== 'transport_continuity_loss' && value !== 'explicit_consumer_request') {
    throw new ContractValidationError('resync reason is unsupported');
  }
  return value;
}

function failureReason(value: unknown): ScopedObserverFailureReason {
  if (
    value !== 'native_watcher_recovery_exhausted' &&
    value !== 'native_watcher_routing_failed' &&
    value !== 'internal_control_failure'
  ) {
    throw new ContractValidationError('observer failure reason is unsupported');
  }
  return value;
}

function sourceKey(source: ScopedContinuitySourceBinding): string {
  return `${source.instance_key}\0${source.stream_key}\0${source.object_key}`;
}

function canonicalEqual(left: unknown, right: unknown): boolean {
  return JSON.stringify(left) === JSON.stringify(right);
}

function parseRequiredShape(value: unknown): ScopedResyncRequiredControl {
  const fields = [
    'kind',
    'invalid_scope_epoch',
    'control_sequence',
    'last_contiguous_sequence',
    'baseline_snapshot_digest',
    'reason',
    'discarded_semantic_events',
    'discarded_source_controls',
    'discarded_retained_native_bytes',
  ];
  const input = exactRecord(value, fields, 'resync-required control');
  if (input.kind !== 'observer_resync_required') {
    throw new ContractValidationError('resync-required control has the wrong kind');
  }
  const control: ScopedResyncRequiredControl = {
    kind: 'observer_resync_required',
    invalid_scope_epoch: positiveSafeInteger(input.invalid_scope_epoch, 'invalid scope epoch'),
    control_sequence: positiveSafeInteger(input.control_sequence, 'resync-required control sequence'),
    last_contiguous_sequence: nonnegativeSafeInteger(
      input.last_contiguous_sequence,
      'resync-required last contiguous sequence',
    ),
    baseline_snapshot_digest: decodeFixedOpaque(input.baseline_snapshot_digest, 'baseline snapshot digest'),
    reason: resyncReason(input.reason),
    discarded_semantic_events: nonnegativeSafeInteger(input.discarded_semantic_events, 'discarded semantic events'),
    discarded_source_controls: nonnegativeSafeInteger(input.discarded_source_controls, 'discarded source controls'),
    discarded_retained_native_bytes: nonnegativeSafeInteger(
      input.discarded_retained_native_bytes,
      'discarded retained native bytes',
    ),
  };
  if (control.control_sequence <= control.last_contiguous_sequence) {
    throw new ContractValidationError('resync-required control does not advance its watermark');
  }
  return control;
}

export function parseScopedContinuityEnvelopeContext(value: unknown): ScopedContinuityEnvelopeContext {
  const input = exactRecord(value, ['contract_selection', 'root', 'control_source', 'state'], 'continuity context');
  const common = parseScopedUsageEnvelopeContext({
    contract_selection: input.contract_selection,
    root: input.root,
    authorized_sources: [input.control_source],
  });
  const controlSource = common.authorized_sources[0]!;
  const stateInput = exactRecord(
    input.state,
    ['current_scope_epoch', 'last_contiguous_sequence', 'baseline_snapshot_digest', 'phase', 'prior_resync_required'],
    'continuity consumer state',
  );
  const currentScopeEpoch = positiveSafeInteger(stateInput.current_scope_epoch, 'current scope epoch');
  const lastContiguousSequence = nonnegativeSafeInteger(
    stateInput.last_contiguous_sequence,
    'caller last contiguous sequence',
  );
  const baselineSnapshotDigest =
    stateInput.baseline_snapshot_digest === null
      ? null
      : decodeFixedOpaque(stateInput.baseline_snapshot_digest, 'caller baseline digest');
  const prior = stateInput.prior_resync_required === null ? null : parseRequiredShape(stateInput.prior_resync_required);
  const statePhase = phase(stateInput.phase, 'caller continuity phase');
  if ((statePhase === 'bootstrap') !== (baselineSnapshotDigest === null)) {
    throw new ContractValidationError('caller baseline presence does not match its delivery phase');
  }
  if (
    prior !== null &&
    (statePhase !== 'live' ||
      baselineSnapshotDigest === null ||
      prior.invalid_scope_epoch !== currentScopeEpoch ||
      prior.baseline_snapshot_digest !== baselineSnapshotDigest ||
      prior.control_sequence !== lastContiguousSequence)
  ) {
    throw new ContractValidationError('caller-held invalidation is not the delivered current continuity state');
  }
  return {
    contract_selection: common.contract_selection,
    root: common.root,
    control_source: controlSource,
    state: {
      current_scope_epoch: currentScopeEpoch,
      last_contiguous_sequence: lastContiguousSequence,
      baseline_snapshot_digest: baselineSnapshotDigest,
      phase: statePhase,
      prior_resync_required: prior,
    },
  };
}

function parseRoot(value: unknown, context: ScopedContinuityEnvelopeContext): ScopedContinuityRoot {
  const parsed = parseScopedUsageEnvelopeContext({
    contract_selection: context.contract_selection,
    root: value,
    authorized_sources: [context.control_source],
  }).root;
  if (!canonicalEqual(parsed, context.root)) {
    throw new ContractValidationError('continuity envelope does not match the caller-held root');
  }
  return parsed;
}

function parseActor(value: unknown, root: ScopedContinuityRoot): ScopedContinuityActor {
  const fields = [
    'root_session_key',
    'run_key',
    'role',
    'parent_run_key',
    'native_session_id',
    'native_actor_id',
    'native_actor_type',
  ];
  const input = exactRecord(value, fields, 'continuity actor');
  const nativeSession = root.native_session_claim?.identity.value?.native_id ?? null;
  if (
    input.root_session_key !== root.session_key ||
    input.run_key !== root.root_actor_run_key ||
    input.role !== 'root' ||
    input.parent_run_key !== null ||
    input.native_session_id !== nativeSession ||
    input.native_actor_id !== null ||
    input.native_actor_type !== null
  ) {
    throw new ContractValidationError('continuity control actor is not the exact root actor');
  }
  return {
    root_session_key: parseOpaqueContractReference(input.root_session_key, 'actor root session key'),
    run_key: parseOpaqueContractReference(input.run_key, 'actor run key'),
    role: 'root',
    parent_run_key: null,
    native_session_id: nativeSession,
    native_actor_id: null,
    native_actor_type: null,
  };
}

function parseAffiliations(value: unknown, actor: ScopedContinuityActor): ScopedContinuityAffiliations {
  const fields = [
    'actor_run_key',
    'team_key',
    'native_team_id',
    'team_name',
    'member_key',
    'workflow_key',
    'native_workflow_id',
    'completeness',
    'derived_from_revision_refs',
  ];
  const input = exactRecord(value, fields, 'continuity affiliations');
  if (
    input.actor_run_key !== actor.run_key ||
    input.team_key !== null ||
    input.native_team_id !== null ||
    input.team_name !== null ||
    input.member_key !== null ||
    input.workflow_key !== null ||
    input.native_workflow_id !== null ||
    input.completeness !== 'unknown' ||
    !Array.isArray(input.derived_from_revision_refs) ||
    input.derived_from_revision_refs.length !== 0
  ) {
    throw new ContractValidationError('continuity affiliations must remain explicitly unknown');
  }
  return {
    actor_run_key: parseOpaqueContractReference(input.actor_run_key, 'affiliation actor run key'),
    team_key: null,
    native_team_id: null,
    team_name: null,
    member_key: null,
    workflow_key: null,
    native_workflow_id: null,
    completeness: 'unknown',
    derived_from_revision_refs: [],
  };
}

function parseSource(
  value: unknown,
  context: ScopedContinuityEnvelopeContext,
  scopeEpoch: number,
): ScopedContinuitySource {
  const fields = [
    'instance_key',
    'stream_key',
    'object_key',
    'locator_id',
    'generation',
    'source_record_id',
    'record_index',
    'cursor_start',
    'cursor_end',
    'byte_range',
  ];
  const input = exactRecord(value, fields, 'continuity source');
  const binding: ScopedContinuitySourceBinding = {
    instance_key: parseOpaqueContractReference(input.instance_key, 'control source instance key'),
    stream_key: parseOpaqueContractReference(input.stream_key, 'control source stream key'),
    object_key: parseOpaqueContractReference(input.object_key, 'control source object key'),
  };
  if (sourceKey(binding) !== sourceKey(context.control_source)) {
    throw new ContractValidationError('continuity envelope does not use the caller-held observer control source');
  }
  for (const field of ['locator_id', 'source_record_id', 'record_index', 'cursor_start', 'cursor_end', 'byte_range']) {
    if (input[field] !== null) {
      throw new ContractValidationError('continuity control cannot disclose source occurrence data');
    }
  }
  const generation = positiveSafeInteger(input.generation, 'continuity source generation');
  if (generation !== scopeEpoch) {
    throw new ContractValidationError('continuity control source generation does not match its scope epoch');
  }
  return {
    ...binding,
    locator_id: null,
    generation,
    source_record_id: null,
    record_index: null,
    cursor_start: null,
    cursor_end: null,
    byte_range: null,
  };
}

function parseStarted(
  value: unknown,
  context: ScopedContinuityEnvelopeContext,
  observerSequence: number,
  scopeEpoch: number,
  envelopePhase: ScopedContinuityPhase,
): ScopedResyncStartedControl {
  const fields = [
    'kind',
    'old_scope_epoch',
    'new_scope_epoch',
    'control_sequence',
    'required_control_sequence',
    'baseline_snapshot_digest',
    'reason',
    'replacement',
  ];
  const input = exactRecord(value, fields, 'resync-started control');
  if (input.kind !== 'observer_resync_started') {
    throw new ContractValidationError('resync-started control has the wrong kind');
  }
  const prior = context.state.prior_resync_required;
  if (prior === null) {
    throw new ContractValidationError('resync-started control requires the caller-held delivered invalidation');
  }
  const oldScopeEpoch = positiveSafeInteger(input.old_scope_epoch, 'old scope epoch');
  const newScopeEpoch = positiveSafeInteger(input.new_scope_epoch, 'new scope epoch');
  const controlSequence = positiveSafeInteger(input.control_sequence, 'resync-started control sequence');
  const requiredControlSequence = positiveSafeInteger(input.required_control_sequence, 'required control sequence');
  const baseline = decodeFixedOpaque(input.baseline_snapshot_digest, 'resync-started baseline digest');
  const reason = resyncReason(input.reason);
  if (
    oldScopeEpoch + 1 !== newScopeEpoch ||
    oldScopeEpoch !== context.state.current_scope_epoch ||
    newScopeEpoch !== scopeEpoch ||
    controlSequence !== observerSequence ||
    controlSequence <= requiredControlSequence ||
    requiredControlSequence !== prior.control_sequence ||
    baseline !== context.state.baseline_snapshot_digest ||
    baseline !== prior.baseline_snapshot_digest ||
    reason !== prior.reason ||
    input.replacement !== 'full_snapshot' ||
    envelopePhase !== 'correction'
  ) {
    throw new ContractValidationError('resync-started control does not continue the caller-held invalidation');
  }
  return {
    kind: 'observer_resync_started',
    old_scope_epoch: oldScopeEpoch,
    new_scope_epoch: newScopeEpoch,
    control_sequence: controlSequence,
    required_control_sequence: requiredControlSequence,
    baseline_snapshot_digest: baseline,
    reason,
    replacement: 'full_snapshot',
  };
}

function parseFailure(
  value: unknown,
  context: ScopedContinuityEnvelopeContext,
  observerSequence: number,
  scopeEpoch: number,
  envelopePhase: ScopedContinuityPhase,
): ScopedObserverFailedControl {
  const fields = [
    'kind',
    'failed_scope_epoch',
    'control_sequence',
    'last_contiguous_sequence',
    'phase',
    'reason',
    'discarded_semantic_events',
    'discarded_source_controls',
    'discarded_retained_native_bytes',
  ];
  const input = exactRecord(value, fields, 'observer-failed control');
  if (input.kind !== 'observer_failed') {
    throw new ContractValidationError('observer-failed control has the wrong kind');
  }
  const failedScopeEpoch = positiveSafeInteger(input.failed_scope_epoch, 'failed scope epoch');
  const controlSequence = positiveSafeInteger(input.control_sequence, 'observer-failed control sequence');
  const lastContiguousSequence = nonnegativeSafeInteger(
    input.last_contiguous_sequence,
    'observer-failed last contiguous sequence',
  );
  const failurePhase = phase(input.phase, 'observer-failed phase');
  if (
    failedScopeEpoch !== context.state.current_scope_epoch ||
    failedScopeEpoch !== scopeEpoch ||
    controlSequence !== observerSequence ||
    controlSequence <= lastContiguousSequence ||
    lastContiguousSequence !== context.state.last_contiguous_sequence ||
    failurePhase !== context.state.phase ||
    failurePhase !== envelopePhase
  ) {
    throw new ContractValidationError('observer-failed control does not match caller-held continuity state');
  }
  return {
    kind: 'observer_failed',
    failed_scope_epoch: failedScopeEpoch,
    control_sequence: controlSequence,
    last_contiguous_sequence: lastContiguousSequence,
    phase: failurePhase,
    reason: failureReason(input.reason),
    discarded_semantic_events: nonnegativeSafeInteger(input.discarded_semantic_events, 'discarded semantic events'),
    discarded_source_controls: nonnegativeSafeInteger(input.discarded_source_controls, 'discarded source controls'),
    discarded_retained_native_bytes: nonnegativeSafeInteger(
      input.discarded_retained_native_bytes,
      'discarded retained native bytes',
    ),
  };
}

function parseEvent(
  value: unknown,
  context: ScopedContinuityEnvelopeContext,
  observerSequence: number,
  scopeEpoch: number,
  envelopePhase: ScopedContinuityPhase,
): ScopedContinuityControl {
  const input = record(value, 'continuity control');
  if (input.kind === 'observer_resync_required') {
    const control = parseRequiredShape(input);
    if (
      control.invalid_scope_epoch !== context.state.current_scope_epoch ||
      control.invalid_scope_epoch !== scopeEpoch ||
      control.control_sequence !== observerSequence ||
      control.last_contiguous_sequence !== context.state.last_contiguous_sequence ||
      control.baseline_snapshot_digest !== context.state.baseline_snapshot_digest ||
      context.state.prior_resync_required !== null ||
      context.state.phase !== 'live' ||
      envelopePhase !== 'live'
    ) {
      throw new ContractValidationError('resync-required control does not match caller-held continuity state');
    }
    return control;
  }
  if (input.kind === 'observer_resync_started') {
    return parseStarted(input, context, observerSequence, scopeEpoch, envelopePhase);
  }
  if (input.kind === 'observer_failed') {
    return parseFailure(input, context, observerSequence, scopeEpoch, envelopePhase);
  }
  throw new ContractValidationError('unsupported observer continuity control kind');
}

export function parseScopedContinuityEnvelope(value: unknown, expectedContextInput: unknown): ScopedContinuityEnvelope {
  const context = parseScopedContinuityEnvelopeContext(expectedContextInput);
  const fields = [
    'scoped_continuity_envelope_contract_version',
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
  const input = exactRecord(value, fields, 'scoped continuity envelope');
  if (input.scoped_continuity_envelope_contract_version !== SCOPED_CONTINUITY_ENVELOPE_CONTRACT_VERSION) {
    throw new ContractValidationError('unsupported scoped continuity envelope contract version');
  }
  const selection = parseObservationContractSelectionForExpected(input.contract_selection, context.contract_selection);
  if (input.contract_version !== selection.envelope_contract_version) {
    throw new ContractValidationError('continuity envelope does not match the selected envelope contract');
  }
  const observerSequence = positiveSafeInteger(input.observer_sequence, 'observer sequence');
  const scopeEpoch = positiveSafeInteger(input.scope_epoch, 'scope epoch');
  const envelopePhase = phase(input.phase, 'continuity delivery phase');
  if (input.semantic_revision_ref !== null || input.native_time !== null) {
    throw new ContractValidationError('observer continuity controls cannot carry semantic or native time evidence');
  }
  const root = parseRoot(input.root, context);
  const actor = parseActor(input.actor, root);
  const actorAttribution = exactRecord(input.actor_attribution, ['kind', 'reason'], 'continuity actor attribution');
  if (actorAttribution.kind !== 'scope_fallback' || actorAttribution.reason !== 'observer_lifecycle_control') {
    throw new ContractValidationError('continuity control has invalid actor attribution');
  }
  const evidence = exactRecord(
    input.evidence,
    ['authority', 'quality', 'effective_at', 'completeness'],
    'continuity evidence',
  );
  if (
    evidence.authority !== 'engine_control' ||
    evidence.quality !== 'derived' ||
    evidence.effective_at !== null ||
    evidence.completeness !== 'complete'
  ) {
    throw new ContractValidationError('continuity control has invalid engine evidence');
  }
  const nativeEvidence = exactRecord(input.native_evidence, ['kind'], 'continuity native evidence');
  if (nativeEvidence.kind !== 'engine_control') {
    throw new ContractValidationError('continuity control cannot carry native record evidence');
  }
  const event = parseEvent(input.event, context, observerSequence, scopeEpoch, envelopePhase);
  return {
    scoped_continuity_envelope_contract_version: SCOPED_CONTINUITY_ENVELOPE_CONTRACT_VERSION,
    contract_version: selection.envelope_contract_version,
    contract_selection: selection,
    observer_sequence: observerSequence,
    scope_epoch: scopeEpoch,
    event_id: decodeFixedOpaque(input.event_id, 'continuity event_id'),
    semantic_revision_ref: null,
    root,
    actor,
    actor_attribution: { kind: 'scope_fallback', reason: 'observer_lifecycle_control' },
    affiliations: parseAffiliations(input.affiliations, actor),
    source: parseSource(input.source, context, scopeEpoch),
    native_time: null,
    observed_at: safeInteger(input.observed_at, 'continuity observed_at'),
    phase: envelopePhase,
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

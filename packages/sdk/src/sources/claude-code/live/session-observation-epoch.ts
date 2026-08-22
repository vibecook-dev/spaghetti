/**
 * Consumer-owned RFC 012D epoch reducer for the Claude shadow migration.
 *
 * The reducer consumes only already-validated typed observer envelopes. It
 * never parses native payloads. Bootstrap and correction epochs are staged in
 * isolation and become visible only at their completion barrier.
 */

import type { SourceCoverageSet } from '../../../contracts/rfc012a.js';
import type {
  ScopedBootstrapCompletionBarrier,
  ScopedResyncCompletionBarrier,
} from '../../../contracts/rfc012d-completion-envelope.js';
import type { ScopedObservationEventEnvelope } from '../../../contracts/rfc012d-event-envelope.js';
import type { ScopedReplacementManifest } from '../../../contracts/rfc012d-replacement-manifest.js';

const MAX_SHADOW_ENTITIES = 16_384;

export type SessionObservationShadowPhase =
  | 'opening'
  | 'bootstrap'
  | 'live'
  | 'invalidated'
  | 'resync'
  | 'failed'
  | 'closed';

export type SessionObservationShadowEntityEnvelope = Extract<
  ScopedObservationEventEnvelope,
  { family: 'usage' | 'actor' | 'artifact_availability' | 'unknown_wire_event' }
>;

export interface SessionObservationShadowSnapshot {
  readonly phase: SessionObservationShadowPhase;
  readonly scopeEpoch: number | null;
  readonly records: readonly SessionObservationShadowEntityEnvelope[];
  readonly sourceCoverage: readonly SourceCoverageSet[];
  readonly replacementManifest: ScopedReplacementManifest | null;
  readonly failure: 'observer_failed' | 'transport_failed' | null;
}

export type SessionObservationEpochAction =
  | { kind: 'none' }
  | { kind: 'bootstrap_complete'; barrier: ScopedBootstrapCompletionBarrier }
  | { kind: 'resync_required' }
  | { kind: 'resync_complete'; barrier: ScopedResyncCompletionBarrier }
  | { kind: 'observer_failed' };

interface EpochStage {
  readonly scopeEpoch: number;
  readonly records: Map<string, SessionObservationShadowEntityEnvelope>;
}

export class SessionObservationEpochReducer {
  #phase: SessionObservationShadowPhase = 'opening';
  #scopeEpoch: number | null = null;
  #lastObserverSequence = 0;
  #active = new Map<string, SessionObservationShadowEntityEnvelope>();
  #stage: EpochStage | undefined;
  #sourceCoverage: readonly SourceCoverageSet[] = Object.freeze([]);
  #replacementManifest: ScopedReplacementManifest | null = null;
  #failure: 'observer_failed' | 'transport_failed' | null = null;

  beginBootstrap(): void {
    if (this.#phase !== 'opening') throw new Error('scoped observation shadow bootstrap already began');
    this.#phase = 'bootstrap';
  }

  apply(envelope: ScopedObservationEventEnvelope): SessionObservationEpochAction {
    if (this.#phase === 'failed' || this.#phase === 'closed') {
      throw new Error('scoped observation shadow is terminal');
    }
    const sequence = observerSequence(envelope);
    if (sequence <= this.#lastObserverSequence) {
      throw new Error('scoped observation shadow sequence did not advance');
    }
    if (sequence !== this.#lastObserverSequence + 1 && !permitsSequenceGap(envelope)) {
      throw new Error('scoped observation shadow sequence has an unexplained gap');
    }
    this.#lastObserverSequence = sequence;

    switch (envelope.family) {
      case 'usage':
      case 'actor':
      case 'artifact_availability':
      case 'unknown_wire_event':
        this.#applyEntity(envelope);
        return { kind: 'none' };
      case 'source':
        this.#requireEventEpoch(envelope.event.scope_epoch, envelope.event.phase);
        return { kind: 'none' };
      case 'completion':
        return this.#applyCompletion(envelope.event);
      case 'continuity':
        return this.#applyContinuity(envelope.event);
    }
  }

  close(): void {
    if (this.#phase === 'closed') return;
    this.#phase = 'closed';
    this.#stage = undefined;
  }

  fail(): void {
    if (this.#phase === 'closed' || this.#failure === 'observer_failed') return;
    this.#phase = 'failed';
    this.#failure = 'transport_failed';
    this.#stage = undefined;
  }

  snapshot(): SessionObservationShadowSnapshot {
    const records = [...this.#active.entries()]
      .sort(([left], [right]) => left.localeCompare(right))
      .map(([, envelope]) => envelope);
    return Object.freeze({
      phase: this.#phase,
      scopeEpoch: this.#scopeEpoch,
      records: Object.freeze(records),
      sourceCoverage: this.#sourceCoverage,
      replacementManifest: this.#replacementManifest,
      failure: this.#failure,
    });
  }

  #applyEntity(envelope: SessionObservationShadowEntityEnvelope): void {
    const { scopeEpoch, phase } = eventCoordinate(envelope);
    const target = this.#targetFor(scopeEpoch, phase);
    const key = entityKey(envelope);
    if (entityOperation(envelope) === 'retract') {
      target.delete(key);
      return;
    }
    if (!target.has(key) && target.size >= MAX_SHADOW_ENTITIES) {
      throw new Error('scoped observation shadow entity capacity exceeded');
    }
    target.set(key, envelope);
  }

  #targetFor(
    scopeEpoch: number,
    phase: 'bootstrap' | 'live' | 'correction',
  ): Map<string, SessionObservationShadowEntityEnvelope> {
    if (phase === 'bootstrap') {
      if (this.#phase !== 'bootstrap' || this.#scopeEpoch !== null) {
        throw new Error('scoped observation shadow received bootstrap data out of phase');
      }
      this.#stage ??= { scopeEpoch, records: new Map() };
      if (this.#stage.scopeEpoch !== scopeEpoch) {
        throw new Error('scoped observation shadow bootstrap epoch changed');
      }
      return this.#stage.records;
    }
    if (phase === 'correction') {
      if (this.#phase !== 'resync' || this.#stage?.scopeEpoch !== scopeEpoch) {
        throw new Error('scoped observation shadow received correction data out of phase');
      }
      return this.#stage.records;
    }
    if (this.#phase !== 'live' || this.#scopeEpoch !== scopeEpoch) {
      throw new Error('scoped observation shadow received live data outside the active epoch');
    }
    return this.#active;
  }

  #requireEventEpoch(scopeEpoch: number, phase: 'bootstrap' | 'live' | 'correction'): void {
    void this.#targetFor(scopeEpoch, phase);
  }

  #applyCompletion(
    envelope: Extract<ScopedObservationEventEnvelope, { family: 'completion' }>['event'],
  ): SessionObservationEpochAction {
    const completion = envelope.event;
    if (completion.kind === 'observer_bootstrap_complete') {
      if (this.#phase !== 'bootstrap' || this.#scopeEpoch !== null) {
        throw new Error('scoped observation shadow bootstrap completion is out of phase');
      }
      const stage = this.#stage ?? { scopeEpoch: envelope.scope_epoch, records: new Map() };
      if (stage.scopeEpoch !== envelope.scope_epoch || completion.barrier.scope_epoch !== envelope.scope_epoch) {
        throw new Error('scoped observation shadow bootstrap completion changed epoch');
      }
      this.#activate(stage, completion.barrier.source_coverage, completion.barrier.replacement_manifest);
      return { kind: 'bootstrap_complete', barrier: completion.barrier };
    }

    if (
      this.#phase !== 'resync' ||
      this.#stage === undefined ||
      this.#stage.scopeEpoch !== envelope.scope_epoch ||
      completion.barrier.scope_epoch !== envelope.scope_epoch
    ) {
      throw new Error('scoped observation shadow resync completion is out of phase');
    }
    this.#activate(this.#stage, completion.barrier.source_coverage, completion.barrier.replacement_manifest);
    return { kind: 'resync_complete', barrier: completion.barrier };
  }

  #applyContinuity(
    envelope: Extract<ScopedObservationEventEnvelope, { family: 'continuity' }>['event'],
  ): SessionObservationEpochAction {
    const control = envelope.event;
    switch (control.kind) {
      case 'observer_resync_required':
        if (this.#phase !== 'live' || this.#scopeEpoch !== control.invalid_scope_epoch) {
          throw new Error('scoped observation shadow invalidation is out of phase');
        }
        this.#phase = 'invalidated';
        return { kind: 'resync_required' };
      case 'observer_resync_started':
        if (
          this.#phase !== 'invalidated' ||
          this.#scopeEpoch !== control.old_scope_epoch ||
          envelope.scope_epoch !== control.new_scope_epoch
        ) {
          throw new Error('scoped observation shadow resync start is out of phase');
        }
        this.#phase = 'resync';
        this.#stage = { scopeEpoch: control.new_scope_epoch, records: new Map() };
        return { kind: 'none' };
      case 'observer_failed':
        if (this.#scopeEpoch !== null && control.failed_scope_epoch !== this.#scopeEpoch) {
          throw new Error('scoped observation shadow failure changed epoch');
        }
        this.#phase = 'failed';
        this.#failure = 'observer_failed';
        this.#stage = undefined;
        return { kind: 'observer_failed' };
    }
  }

  #activate(
    stage: EpochStage,
    sourceCoverage: readonly SourceCoverageSet[],
    replacementManifest: ScopedReplacementManifest,
  ): void {
    this.#active = stage.records;
    this.#scopeEpoch = stage.scopeEpoch;
    this.#stage = undefined;
    this.#sourceCoverage = Object.freeze([...sourceCoverage]);
    this.#replacementManifest = replacementManifest;
    this.#phase = 'live';
  }
}

function observerSequence(envelope: ScopedObservationEventEnvelope): number {
  return envelope.family === 'unknown_wire_event'
    ? envelope.event.envelope_provenance.observer_sequence
    : envelope.event.observer_sequence;
}

function permitsSequenceGap(envelope: ScopedObservationEventEnvelope): boolean {
  return (
    envelope.family === 'continuity' &&
    (envelope.event.event.kind === 'observer_resync_required' || envelope.event.event.kind === 'observer_failed')
  );
}

function eventCoordinate(envelope: SessionObservationShadowEntityEnvelope): {
  scopeEpoch: number;
  phase: 'bootstrap' | 'live' | 'correction';
} {
  return envelope.family === 'unknown_wire_event'
    ? {
        scopeEpoch: envelope.event.envelope_provenance.scope_epoch,
        phase: envelope.event.envelope_provenance.phase,
      }
    : { scopeEpoch: envelope.event.scope_epoch, phase: envelope.event.phase };
}

function entityKey(envelope: SessionObservationShadowEntityEnvelope): string {
  switch (envelope.family) {
    case 'usage':
    case 'actor':
      return `${envelope.event.event.fact_family}\0${envelope.event.event.fact_id}`;
    case 'artifact_availability': {
      const entry = envelope.event.event.entry;
      return `artifact_availability\0${entry.artifact_kind}\0${entry.artifact_key}`;
    }
    case 'unknown_wire_event': {
      const event = envelope.event;
      const revision =
        event.envelope_provenance.semantic_revision_ref?.fact_revision_id ?? event.envelope_provenance.event_id;
      return `unknown_wire_event\0${event.type_tag}\0${revision}`;
    }
  }
}

function entityOperation(envelope: SessionObservationShadowEntityEnvelope): 'upsert' | 'retract' {
  if (envelope.family === 'usage' || envelope.family === 'actor') {
    return envelope.event.event.operation;
  }
  return 'upsert';
}

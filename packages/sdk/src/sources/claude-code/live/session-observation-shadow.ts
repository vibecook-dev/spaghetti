/**
 * Feature-flagged RFC 012D shadow beside watchSessionTranscript.
 *
 * Default (`rfc012dObserver: false`) delegates to the legacy transcript tail
 * (rollback). When enabled, the legacy tail still runs for comparison and a
 * live typed observer source supplies SemanticRevisionRef/event_id records
 * bound to a scope epoch. Overflow/resync swaps the epoch atomically and
 * drops in-flight old-epoch records. This path does not parse native JSON.
 */

import type { SemanticRevisionRef } from '../../../contracts/rfc012a.js';
import {
  watchSessionTranscript,
  type SessionTranscriptTail,
  type WatchSessionTranscriptOptions,
} from './session-tail.js';

export interface ScopedUsageShadowRecord {
  eventId: string;
  semanticRevisionRef: SemanticRevisionRef;
  factId: string;
  operation: 'upsert' | 'retract';
  scopeEpoch: number;
}

export interface ObserverRecordSource {
  scopeEpoch(): number;
  poll(): readonly ScopedUsageShadowRecord[];
}

export interface WatchSessionObservationShadowOptions extends WatchSessionTranscriptOptions {
  /** Default false. When false, behavior equals watchSessionTranscript. */
  rfc012dObserver?: boolean;
  /** Live typed observer records. Required when the flag is on. */
  observerSource?: ObserverRecordSource;
}

export interface SessionObservationShadow {
  records(): readonly ScopedUsageShadowRecord[];
  scopeEpoch(): number;
  /** Atomically replace the consumer epoch and drop old-epoch records. */
  swapEpoch(nextEpoch: number): void;
}

export interface SessionObservationShadowTail extends SessionTranscriptTail {
  readonly rfc012dObserver: true;
  readonly shadow: SessionObservationShadow;
}

export function isSessionObservationShadowTail(tail: SessionTranscriptTail): tail is SessionObservationShadowTail {
  return Object.hasOwn(tail, 'rfc012dObserver') && (tail as SessionObservationShadowTail).rfc012dObserver === true;
}

class EpochBoundShadow implements SessionObservationShadow {
  private epoch: number;
  private applied: ScopedUsageShadowRecord[] = [];

  constructor(
    private readonly source: ObserverRecordSource,
    initialEpoch: number,
  ) {
    if (!Number.isInteger(initialEpoch) || initialEpoch < 1) {
      throw new RangeError('scope epoch must be a positive integer');
    }
    this.epoch = initialEpoch;
  }

  scopeEpoch(): number {
    return this.epoch;
  }

  records(): readonly ScopedUsageShadowRecord[] {
    const next: ScopedUsageShadowRecord[] = [];
    for (const record of this.source.poll()) {
      if (record.scopeEpoch !== this.epoch) {
        continue;
      }
      next.push(record);
    }
    this.applied = next;
    return this.applied;
  }

  swapEpoch(nextEpoch: number): void {
    if (!Number.isInteger(nextEpoch) || nextEpoch <= this.epoch) {
      throw new RangeError('scope epoch replacement must be a strictly greater positive integer');
    }
    this.epoch = nextEpoch;
    this.applied = [];
  }
}

export function watchSessionObservationShadow(
  transcriptPath: string,
  options: WatchSessionObservationShadowOptions = {},
): SessionTranscriptTail {
  const { rfc012dObserver = false, observerSource, ...tailOptions } = options;
  const tail = watchSessionTranscript(transcriptPath, tailOptions);
  if (!rfc012dObserver) {
    return tail;
  }
  const source = observerSource ?? {
    scopeEpoch: () => 1,
    poll: () => [],
  };
  const shadow = new EpochBoundShadow(source, source.scopeEpoch());
  return Object.assign(tail, {
    rfc012dObserver: true as const,
    shadow,
  });
}

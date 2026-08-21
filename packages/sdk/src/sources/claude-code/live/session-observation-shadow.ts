/**
 * Feature-flagged RFC 012D shadow beside watchSessionTranscript.
 *
 * Default (`rfc012dObserver: false`) delegates to the legacy transcript tail.
 * When enabled, the legacy tail still runs and a crate-private helper records
 * SemanticRevisionRef/event_id from the Rust-produced usage envelope fixture.
 * This path does not call the N-API observer.
 */

import { rfc012dUsageEnvelopeShadowRecords, type ScopedUsageShadowRecord } from './rfc012d-usage-envelope-shadow.js';
import {
  watchSessionTranscript,
  type SessionTranscriptTail,
  type WatchSessionTranscriptOptions,
} from './session-tail.js';

export type { ScopedUsageShadowRecord };

export interface WatchSessionObservationShadowOptions extends WatchSessionTranscriptOptions {
  /** Default false. When false, behavior equals watchSessionTranscript. */
  rfc012dObserver?: boolean;
  shadowHelper?: () => readonly ScopedUsageShadowRecord[];
}

export interface SessionObservationShadow {
  records(): readonly ScopedUsageShadowRecord[];
}

export interface SessionObservationShadowTail extends SessionTranscriptTail {
  readonly rfc012dObserver: true;
  readonly shadow: SessionObservationShadow;
}

export function isSessionObservationShadowTail(tail: SessionTranscriptTail): tail is SessionObservationShadowTail {
  return Object.hasOwn(tail, 'rfc012dObserver') && (tail as SessionObservationShadowTail).rfc012dObserver === true;
}

export function watchSessionObservationShadow(
  transcriptPath: string,
  options: WatchSessionObservationShadowOptions = {},
): SessionTranscriptTail {
  const { rfc012dObserver = false, shadowHelper, ...tailOptions } = options;
  const tail = watchSessionTranscript(transcriptPath, tailOptions);
  if (!rfc012dObserver) {
    return tail;
  }
  const records = (shadowHelper ?? rfc012dUsageEnvelopeShadowRecords)();
  const shadow: SessionObservationShadow = {
    records: () => records,
  };
  return Object.assign(tail, {
    rfc012dObserver: true as const,
    shadow,
  });
}

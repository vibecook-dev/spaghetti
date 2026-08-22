/**
 * Feature-flagged RFC 012D observer shadow beside watchSessionTranscript.
 *
 * The compatibility tail remains authoritative and operational. When the flag
 * is enabled, this module opens and owns the typed store-free observer, drains
 * it continuously, and maintains an independent epoch-replacement state for
 * comparison. Observer failure is reported without stopping the legacy tail.
 */

import type { ScopedResyncCompletionBarrier } from '../../../contracts/rfc012d-completion-envelope.js';
import type { ScopedObservationWatermark } from '../../../contracts/rfc012d-watermark.js';
import { observeSession, type SessionObservationRequest, type SessionObserver } from '../../../scoped-observation.js';
import { SessionObservationEpochReducer, type SessionObservationShadowSnapshot } from './session-observation-epoch.js';
import {
  watchSessionTranscript,
  type SessionTranscriptEvent,
  type SessionTranscriptTail,
  type WatchSessionTranscriptOptions,
} from './session-tail.js';

type Dispose = () => void;
type SessionObserverFactory = (request: SessionObservationRequest) => Promise<SessionObserver>;

export interface WatchSessionObservationShadowOptions extends WatchSessionTranscriptOptions {
  /** Default false. False is an exact rollback to watchSessionTranscript. */
  rfc012dObserver?: boolean;
  /** Required when the feature flag is on. Native access remains Rust-owned. */
  observerRequest?: SessionObservationRequest;
}

export interface SessionObservationShadow {
  snapshot(): SessionObservationShadowSnapshot;
  /** Resolves only after bootstrap completion was applied and acknowledged. */
  ready(): Promise<SessionObservationShadowSnapshot>;
  /** Hook-friendly native poll hint. Returns null after independent failure. */
  poll(): Promise<ScopedObservationWatermark | null>;
  /** Explicit full-snapshot replacement, swapped only at resync completion. */
  resync(): Promise<SessionObservationShadowSnapshot>;
  close(): Promise<void>;
}

export interface SessionObservationShadowTail extends SessionTranscriptTail {
  readonly rfc012dObserver: true;
  readonly shadow: SessionObservationShadow;
}

export function isSessionObservationShadowTail(tail: SessionTranscriptTail): tail is SessionObservationShadowTail {
  return Object.hasOwn(tail, 'rfc012dObserver') && (tail as SessionObservationShadowTail).rfc012dObserver === true;
}

interface Deferred<T> {
  readonly promise: Promise<T>;
  resolve(value: T): void;
  reject(error: unknown): void;
}

function deferred<T>(): Deferred<T> {
  let resolvePromise!: (value: T) => void;
  let rejectPromise!: (error: unknown) => void;
  let settled = false;
  const promise = new Promise<T>((resolve, reject) => {
    resolvePromise = resolve;
    rejectPromise = reject;
  });
  void promise.catch(() => undefined);
  return {
    promise,
    resolve(value) {
      if (settled) return;
      settled = true;
      resolvePromise(value);
    },
    reject(error) {
      if (settled) return;
      settled = true;
      rejectPromise(error);
    },
  };
}

class OwnedSessionObservationShadow implements SessionObservationShadow {
  readonly #request: SessionObservationRequest;
  readonly #factory: SessionObserverFactory;
  readonly #reportFailure: (error: Error) => void;
  readonly #reducer = new SessionObservationEpochReducer();
  readonly #opened = deferred<SessionObserver>();
  readonly #ready = deferred<SessionObservationShadowSnapshot>();
  readonly #closeRequested = deferred<void>();
  #observer: SessionObserver | undefined;
  #runPromise: Promise<void>;
  #closePromise: Promise<void> | undefined;
  #resyncPromise: Promise<SessionObservationShadowSnapshot> | undefined;
  #closing = false;
  #failed = false;

  constructor(
    request: SessionObservationRequest,
    factory: SessionObserverFactory,
    reportFailure: (error: Error) => void,
  ) {
    this.#request = request;
    this.#factory = factory;
    this.#reportFailure = reportFailure;
    this.#runPromise = this.#run();
    void this.#runPromise.catch(() => undefined);
  }

  snapshot(): SessionObservationShadowSnapshot {
    return this.#reducer.snapshot();
  }

  ready(): Promise<SessionObservationShadowSnapshot> {
    return this.#ready.promise;
  }

  async poll(): Promise<ScopedObservationWatermark | null> {
    if (this.#failed || this.#closing) return null;
    try {
      const observer = await this.#opened.promise;
      return await observer.poll();
    } catch (error) {
      await this.#fail(error);
      return null;
    }
  }

  resync(): Promise<SessionObservationShadowSnapshot> {
    if (this.#resyncPromise !== undefined) return this.#resyncPromise;
    if (this.#failed || this.#closing) return Promise.reject(shadowFailure());
    this.#resyncPromise = this.#requestResync().finally(() => {
      this.#resyncPromise = undefined;
    });
    return this.#resyncPromise;
  }

  close(): Promise<void> {
    if (this.#closePromise !== undefined) return this.#closePromise;
    this.#closing = true;
    this.#closeRequested.resolve(undefined);
    this.#reducer.close();
    const error = shadowClosed();
    this.#opened.reject(error);
    this.#ready.reject(error);
    this.#closePromise = (async () => {
      const observer = this.#observer;
      if (observer !== undefined) await observer.close().catch(() => undefined);
      await this.#runPromise.catch(() => undefined);
    })();
    return this.#closePromise;
  }

  async #run(): Promise<void> {
    try {
      const opening = Promise.resolve().then(() => this.#factory(this.#request));
      const outcome = await Promise.race([
        opening.then((observer) => ({ kind: 'opened' as const, observer })),
        this.#closeRequested.promise.then(() => ({ kind: 'closed' as const })),
      ]);
      if (outcome.kind === 'closed') {
        // Native open currently has no cancellation parameter. Do not make
        // close wait for it, but close any owner that eventually arrives.
        void opening.then(
          (observer) => observer.close().catch(() => undefined),
          () => undefined,
        );
        return;
      }
      const observer = outcome.observer;
      if (this.#closing) {
        await observer.close().catch(() => undefined);
        return;
      }
      this.#observer = observer;
      this.#opened.resolve(observer);
      this.#reducer.beginBootstrap();

      const appliedReady = observer.ready().then(() => {
        const snapshot = this.#reducer.snapshot();
        if (snapshot.phase !== 'live' || snapshot.scopeEpoch === null) throw shadowFailure();
        this.#ready.resolve(snapshot);
      });
      void appliedReady.catch((error: unknown) => this.#fail(error));

      await observer.consume((envelope) => {
        const action = this.#reducer.apply(envelope);
        if (action.kind === 'resync_required') {
          void this.resync().catch(() => undefined);
        } else if (action.kind === 'observer_failed') {
          void this.#fail(shadowFailure());
        }
      });
      if (!this.#closing) throw shadowFailure();
    } catch (error) {
      if (!this.#closing) await this.#fail(error);
    }
  }

  async #requestResync(): Promise<SessionObservationShadowSnapshot> {
    try {
      const observer = await this.#opened.promise;
      const barrier: ScopedResyncCompletionBarrier = await observer.resync();
      const snapshot = this.#reducer.snapshot();
      if (snapshot.phase !== 'live' || snapshot.scopeEpoch !== barrier.scope_epoch) throw shadowFailure();
      return snapshot;
    } catch (error) {
      await this.#fail(error);
      throw shadowFailure();
    }
  }

  async #fail(_error: unknown): Promise<void> {
    if (this.#failed || this.#closing) return;
    this.#failed = true;
    this.#reducer.fail();
    const error = shadowFailure();
    this.#opened.reject(error);
    this.#ready.reject(error);
    this.#reportFailure(error);
    const observer = this.#observer;
    if (observer !== undefined) await observer.close().catch(() => undefined);
  }
}

/**
 * Keep the legacy tail live while shadowing the owned RFC 012D observer. The
 * return is synchronous so existing hook wiring retains its lifecycle;
 * `shadow.ready()` exposes the asynchronous observer boundary.
 */
export function watchSessionObservationShadow(
  transcriptPath: string,
  options: WatchSessionObservationShadowOptions = {},
): SessionTranscriptTail {
  return watchSessionObservationShadowWithFactory(transcriptPath, options, observeSession);
}

/** @internal Test seam; not re-exported by the Claude live barrel. */
export function watchSessionObservationShadowWithFactory(
  transcriptPath: string,
  options: WatchSessionObservationShadowOptions,
  observerFactory: SessionObserverFactory,
): SessionTranscriptTail {
  const { rfc012dObserver = false, observerRequest, ...tailOptions } = options;
  const tail = watchSessionTranscript(transcriptPath, tailOptions);
  if (!rfc012dObserver) return tail;
  if (observerRequest === undefined) {
    tail.stop();
    throw new TypeError('rfc012dObserver requires a typed observerRequest');
  }

  const shadowErrorListeners = new Set<(error: Error) => void>();
  const shadow = new OwnedSessionObservationShadow(observerRequest, observerFactory, (error) => {
    for (const listener of shadowErrorListeners) {
      try {
        listener(error);
      } catch {
        // A diagnostic listener cannot affect either observation path.
      }
    }
  });
  let stopped = false;

  const result: SessionObservationShadowTail = {
    rfc012dObserver: true,
    shadow,
    onMessage(listener: (event: SessionTranscriptEvent) => void): Dispose {
      return tail.onMessage(listener);
    },
    onError(listener: (error: Error) => void): Dispose {
      const disposeTail = tail.onError(listener);
      shadowErrorListeners.add(listener);
      return () => {
        disposeTail();
        shadowErrorListeners.delete(listener);
      };
    },
    async poll(): Promise<void> {
      await tail.poll();
      // Hook delivery and the compatibility tail must never wait behind an
      // observer attach/read. The shadow owns and reports that work
      // independently.
      void shadow.poll().catch(() => null);
    },
    stop(): void {
      if (stopped) return;
      stopped = true;
      tail.stop();
      shadowErrorListeners.clear();
      void shadow.close();
    },
  };
  return result;
}

function shadowFailure(): Error {
  return new Error('RFC 012D observation shadow failed');
}

function shadowClosed(): Error {
  return new Error('RFC 012D observation shadow is closed');
}

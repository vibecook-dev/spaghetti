/**
 * `observeSession` — the store-free session observer, as an async iterator.
 *
 * This is a transport wrapper and nothing else. The native observer owns
 * decoding, ordering, epochs, and queue bounds (RFC 012D §10–§13); every event
 * shape here is generated from Rust by `pnpm generate:types`. The only work
 * this file does is turn "call `waitForEvents`, parse the batch, repeat" into
 * `for await`, and guarantee the attachment is released when iteration ends.
 *
 * ```ts
 * const observer = observeSession({
 *   adapter_id: 'claude-code',
 *   agent_root: `${homedir()}/.claude`,
 *   transcript_path: transcript,
 * });
 * try {
 *   for await (const event of observer) {
 *     if (event.type === 'bootstrap_complete') swapStagedEpoch(event.scope_epoch);
 *     else if (isSemanticEvent(event)) apply(event);
 *   }
 * } finally {
 *   await observer.close();
 * }
 * ```
 */

import type { ObserverStatus } from '@vibecook/spaghetti-sdk-native';

import type { ObserveSessionRequest, ObserverEvent, ObserverFamily } from './generated/index.js';
import { openNativeSessionObserver, type NativeSessionObserver } from './native.js';

export type { ObserveSessionRequest, ObserverEvent };

/**
 * Epoch, queue depth, and whether continuity still holds.
 *
 * This is the napi-generated status object, not a copy of it: renaming a field
 * in Rust breaks this file rather than silently diverging from it.
 */
export type SessionObserverStatus = ObserverStatus;

/**
 * The eleven semantic families, as they appear on the stream.
 *
 * Derived from {@link ObserverEvent} rather than listed, so a family added in
 * Rust joins this type by regenerating and nothing else.
 */
export type SemanticObserverEvent = Extract<ObserverEvent, { family: ObserverFamily }>;

/**
 * Whether an event carries a typed RFC 012C revision.
 *
 * The eleven semantic variants are exactly the ones with a `family`; control
 * events do not have one, and `unknown_evidence` carries `family_hint` instead
 * precisely because its family was never established.
 */
export function isSemanticEvent(event: ObserverEvent): event is SemanticObserverEvent {
  return 'family' in event;
}

export interface ObserveSessionOptions {
  /**
   * Events taken from the native queue per call. Default 256; the native layer
   * clamps to 4096. This is also the SDK's entire buffer — one batch is held
   * at a time, so a slow consumer applies backpressure to the native queue
   * rather than growing an unbounded array in JavaScript.
   */
  batchSize?: number;
  /**
   * How long each native wait blocks before the loop re-checks for close.
   * Default 250 ms. It bounds shutdown latency, not event latency: an event
   * that arrives during the wait resolves it immediately.
   */
  waitTimeoutMs?: number;
  /**
   * Ends the attachment when aborted.
   *
   * A live session never ends on its own, so a parked `for await` has nothing
   * to return until something closes the observer. This is that something,
   * for a consumer that already has a signal — a session switch, a shutdown
   * handler — and does not want to thread `close()` through it.
   *
   * Aborting is a clean stop, not an error: whatever is already queued is
   * still delivered, the final `closed` event still arrives, and the loop
   * returns instead of throwing. That is deliberate — a consumer applying
   * events cannot lose the ones it already has to a rejection it did not ask
   * for. When the abort *should* propagate, ask for it after the loop:
   *
   * ```ts
   * for await (const event of observer) apply(event);
   * signal.throwIfAborted();
   * ```
   *
   * An already-aborted signal is honoured immediately. The request is still
   * validated first, so a bad locator still throws from `observeSession`
   * rather than being masked by the abort.
   */
  signal?: AbortSignal;
}

/**
 * One live attachment to a session tree.
 *
 * Iteration is single-consumer: `[Symbol.asyncIterator]()` returns the same
 * iterator every time, because a second one would take events from the first
 * rather than seeing its own copy of the stream. Leaving the loop — `break`,
 * `return`, or a thrown error — closes the attachment.
 */
export interface SessionObserver extends AsyncIterable<ObserverEvent> {
  status(): SessionObserverStatus;
  /** Idempotent. Resolves once every owned watch, read, and decode has stopped. */
  close(): Promise<void>;
}

const DEFAULT_BATCH_SIZE = 256;
const DEFAULT_WAIT_TIMEOUT_MS = 250;

/**
 * Attach the observer to one native session tree.
 *
 * Validation is synchronous: an unusable agent root, a locator outside the
 * adapter's declared source roots, or a session id that disagrees with the
 * locator throws here rather than arriving later as an error event. Failures
 * of an *attached* observer are events on the stream (`source_error` for one
 * object, `error` when the observer itself gives up), so a consumer sees them
 * in order with the data they affect instead of as an exception that
 * interrupts a `for await`.
 */
export function observeSession(request: ObserveSessionRequest, options: ObserveSessionOptions = {}): SessionObserver {
  const batchSize = options.batchSize ?? DEFAULT_BATCH_SIZE;
  const waitTimeoutMs = options.waitTimeoutMs ?? DEFAULT_WAIT_TIMEOUT_MS;
  const native: NativeSessionObserver = openNativeSessionObserver(request);

  let buffer: ObserverEvent[] = [];
  let cursor = 0;
  let closing = false;
  let finished = false;
  let closed: Promise<void> | undefined;
  let detachAbort: (() => void) | undefined;

  function close(): Promise<void> {
    closing = true;
    detachAbort?.();
    detachAbort = undefined;
    closed ??= native.close();
    return closed;
  }

  if (options.signal) {
    const signal = options.signal;
    // Closing from the handler also wakes a `waitForEvents` that is parked
    // right now, so an abort is noticed immediately rather than after
    // `waitTimeoutMs`. The `catch` only marks the promise handled — a
    // consumer that later awaits `close()` still sees the failure.
    if (signal.aborted) {
      void close().catch(() => {});
    } else {
      const onAbort = () => void close().catch(() => {});
      signal.addEventListener('abort', onAbort, { once: true });
      detachAbort = () => signal.removeEventListener('abort', onAbort);
    }
  }

  async function fill(): Promise<void> {
    let json: string;
    if (closing) {
      // Wait for the native shutdown to finish before draining. `close()` is
      // what enqueues the final `closed` event, so polling ahead of it reads
      // an empty queue and ends the iteration one event short — which is
      // exactly what an abort mid-loop used to do.
      await close();
      json = native.poll(batchSize);
    } else {
      json = await native.waitForEvents(waitTimeoutMs, batchSize);
    }
    buffer = JSON.parse(json) as ObserverEvent[];
    cursor = 0;
  }

  const iterator: AsyncIterator<ObserverEvent> = {
    async next(): Promise<IteratorResult<ObserverEvent>> {
      while (!finished) {
        if (cursor < buffer.length) {
          const event = buffer[cursor];
          cursor += 1;
          // `closed` is emitted once and is always last. Yield it — it reports
          // how many events were discarded — then end the iteration.
          if (event.type === 'closed') finished = true;
          return { value: event, done: false };
        }
        await fill();
        // A closing observer with nothing left to hand over is done. An open
        // one just timed out; loop and wait again.
        if (closing && buffer.length === 0) finished = true;
      }
      await close();
      return { value: undefined, done: true };
    },

    async return(): Promise<IteratorResult<ObserverEvent>> {
      finished = true;
      await close();
      return { value: undefined, done: true };
    },

    async throw(error: unknown): Promise<IteratorResult<ObserverEvent>> {
      finished = true;
      await close();
      throw error;
    },
  };

  return {
    [Symbol.asyncIterator]: () => iterator,
    status: () => native.status(),
    close,
  };
}

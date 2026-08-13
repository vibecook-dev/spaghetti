/**
 * spaghetti-live.ts — Public `api.live` surface (RFC 005 C3.4).
 *
 * Final Phase 3 component. Binds `AgentDataStore.subscribe` (fan-out)
 * and `LiveUpdates.prewarm` (ref-counted watcher attach) into one
 * handle consumers interact with through `createSpaghettiService({
 * live: true })`. See `docs/LIVE-UPDATES-DESIGN.md` §8 for the final
 * public shape this module implements.
 *
 * Key responsibilities:
 *
 *  - `onChange(listener)` / `onChange(topic, listener, options)` —
 *    composed of `prewarm(topic) + store.subscribe(topic, ...)`. The
 *    returned `Dispose` tears down the subscription and drops the
 *    scope ref, so a short-lived consumer that subscribes, receives a
 *    few events, and disposes leaves the watcher detached.
 *
 *  - `events(opts?)` — sugar over `onChange` returning an
 *    `AsyncIterable<Change>` backed by a bounded ring buffer. Drops
 *    oldest on overflow (matches the RFC 005 §Public API note).
 *
 *  - `prewarm` + `isSaturated` — pass-throughs to the live-updates
 *    orchestrator.
 *
 * Notes on the firehose path:
 *
 *  - `onChange(listener)` subscribes to the firehose but does NOT
 *    prewarm every known scope; doing so would force a filesystem
 *    watcher on every `~/.claude/` subtree for any caller that just
 *    wanted "all events". Instead, firehose delivery piggybacks on
 *    whatever scopes other subscribers (or explicit prewarms) have
 *    attached. Callers that want the firehose to receive events
 *    without also attaching scoped subscriptions must `prewarm(topic)`
 *    explicitly. See RFC 005 §Resolved during design (lazy attachment)
 *    for the rationale.
 */

import type {
  Change,
  ChangeTopic,
  Dispose,
  SubscribeOptions,
  SubscribeOptionsCoalesced,
  SubscribeOptionsLatest,
} from './change-events.js';
import type { AgentDataStore } from '../data/agent-data-store.js';
import type { ErrorSink } from '../io/error-sink.js';
import type { LiveWatch } from './live-watch.js';

// ═══════════════════════════════════════════════════════════════════════════
// PUBLIC TYPES
// ═══════════════════════════════════════════════════════════════════════════

/**
 * The `api.live` handle exposed on the repository-only legacy API when the service was
 * constructed with `{ live: true }`. Matches `docs/LIVE-UPDATES-DESIGN.md`
 * §8.
 */
export interface SpaghettiLive {
  /** Firehose subscribe (single-change delivery). */
  onChange(listener: (e: Change) => void, options?: SubscribeOptionsLatest): Dispose;
  /** Firehose subscribe (coalesced batch delivery when `{ latest: false }` is set). */
  onChange(listener: (e: Change[]) => void, options: SubscribeOptionsCoalesced): Dispose;
  /** Scoped subscribe (single-change delivery). */
  onChange(topic: ChangeTopic, listener: (e: Change) => void, options?: SubscribeOptionsLatest): Dispose;
  /** Scoped subscribe (coalesced batch delivery when `{ latest: false }` is set). */
  onChange(topic: ChangeTopic, listener: (e: Change[]) => void, options: SubscribeOptionsCoalesced): Dispose;

  /**
   * Async iterable form. Use with `for await (const e of
   * api.live.events()) { ... }`. Bounded ring buffer with drop-oldest
   * semantics on overflow — set `bufferSize` to raise or lower the
   * budget; pass `onDrop` to observe drops (e.g. for "live lag"
   * banners).
   */
  events(options?: { bufferSize?: number; onDrop?: (dropped: Change) => void }): AsyncIterable<Change>;

  /**
   * Explicit watcher attachment. Returns a `Dispose` that drops the
   * ref; the underlying `~/.claude/` subtree detaches once the last
   * ref is released. Stacks with `onChange`-driven prewarm refs.
   */
  prewarm(topic: ChangeTopic): Dispose;

  /**
   * True when the live pipeline's coalescing queue has been behind
   * its saturation threshold (default 5s) for at least one observed
   * moment. Useful for UI banners; recovery is automatic.
   */
  isSaturated(): boolean;
}

// ═══════════════════════════════════════════════════════════════════════════
// IMPLEMENTATION
// ═══════════════════════════════════════════════════════════════════════════

class SpaghettiLiveImpl implements SpaghettiLive {
  constructor(
    private readonly store: AgentDataStore,
    private readonly liveWatch: LiveWatch,
    private readonly errorSink: ErrorSink | undefined,
  ) {}

  onChange(listener: (e: Change) => void, options?: SubscribeOptionsLatest): Dispose;
  onChange(listener: (e: Change[]) => void, options: SubscribeOptionsCoalesced): Dispose;
  onChange(topic: ChangeTopic, listener: (e: Change) => void, options?: SubscribeOptionsLatest): Dispose;
  onChange(topic: ChangeTopic, listener: (e: Change[]) => void, options: SubscribeOptionsCoalesced): Dispose;
  onChange(
    topicOrListener: ChangeTopic | ((e: Change) => void) | ((e: Change[]) => void),
    listenerOrOptions?: ((e: Change) => void) | ((e: Change[]) => void) | SubscribeOptions,
    maybeOptions?: SubscribeOptions,
  ): Dispose {
    // Overload resolution: when the first arg is a function, this is
    // the firehose form.
    const isFirehose = typeof topicOrListener === 'function';
    const topic: ChangeTopic | undefined = isFirehose ? undefined : topicOrListener;
    const listener = (
      isFirehose ? topicOrListener : (listenerOrOptions as ((e: Change) => void) | ((e: Change[]) => void))
    )!;
    const options = (isFirehose ? (listenerOrOptions as SubscribeOptions | undefined) : maybeOptions) ?? undefined;

    // Firehose: don't force every scope online. See the module
    // doc-comment for the rationale.
    const prewarmDispose: Dispose | undefined = topic !== undefined ? this.liveWatch.prewarm?.(topic) : undefined;
    // Forward to the store's overloaded subscribe through a widened
    // signature — the public overloads above keep the caller's
    // listener/options shapes aligned.
    const subscribeDispose = (
      this.store.subscribe as (
        t: ChangeTopic | undefined,
        l: ((e: Change) => void) | ((e: Change[]) => void),
        o?: SubscribeOptions,
      ) => Dispose
    )(topic, listener, options);

    let disposed = false;
    return () => {
      if (disposed) return;
      disposed = true;
      subscribeDispose();
      if (prewarmDispose) prewarmDispose();
    };
  }

  events(options?: { bufferSize?: number; onDrop?: (dropped: Change) => void }): AsyncIterable<Change> {
    const bufferSize = options?.bufferSize ?? 1000;
    const onDrop = options?.onDrop;
    // Using the firehose so the async iterator sees every change.
    // Matches the spec in RFC 005 §Public API.
    return {
      [Symbol.asyncIterator]: () => this.makeIterator(bufferSize, onDrop),
    };
  }

  prewarm(topic: ChangeTopic): Dispose {
    // Optional on the general LiveWatch — a source that watches its whole tree
    // (Codex) has no scopes to ref-count, so this is a no-op Dispose.
    return this.liveWatch.prewarm?.(topic) ?? (() => {});
  }

  isSaturated(): boolean {
    return this.liveWatch.isSaturated?.() ?? false;
  }

  // ── private ──────────────────────────────────────────────────────────

  /**
   * Bounded-ring async iterator over the firehose. Pending `next()`
   * promises are resolved directly when a change lands; otherwise
   * the change buffers into the ring with drop-oldest semantics.
   * `return()` disposes the firehose subscription so callers can
   * `break` out of the `for-await` loop without leaking.
   */
  private makeIterator(bufferSize: number, onDrop?: (dropped: Change) => void): AsyncIterator<Change> {
    // Circular buffer stored as plain array with head/tail pointers.
    const buffer: (Change | undefined)[] = new Array(bufferSize);
    let head = 0; // oldest
    let tail = 0; // next write
    let size = 0;

    // Pending next-promise resolver (there's at most one in flight
    // because the standard async-iterator protocol serialises
    // next() calls). Storing just the resolver is sufficient.
    let pendingResolve: ((r: IteratorResult<Change>) => void) | null = null;

    let ended = false;
    let dispose: Dispose | null = null;

    const drain = (c: Change): void => {
      if (pendingResolve !== null) {
        const resolve = pendingResolve;
        pendingResolve = null;
        resolve({ value: c, done: false });
        return;
      }
      if (size >= bufferSize) {
        // Overflow → drop-oldest.
        const dropped = buffer[head]!;
        head = (head + 1) % bufferSize;
        size -= 1;
        if (onDrop) {
          try {
            onDrop(dropped);
          } catch (err) {
            // Forward observer errors to the unified ErrorSink so a
            // misbehaving onDrop can't go unnoticed (RFC 005).
            // Falls through silently if no sink is wired.
            this.errorSink?.error(err instanceof Error ? err : new Error(String(err)), {
              component: 'SpaghettiLive.events.onDrop',
            });
          }
        }
      }
      buffer[tail] = c;
      tail = (tail + 1) % bufferSize;
      size += 1;
    };

    dispose = this.onChange(drain);

    return {
      next: (): Promise<IteratorResult<Change>> => {
        if (ended) return Promise.resolve({ value: undefined, done: true });
        if (size > 0) {
          const c = buffer[head]!;
          buffer[head] = undefined;
          head = (head + 1) % bufferSize;
          size -= 1;
          return Promise.resolve({ value: c, done: false });
        }
        return new Promise<IteratorResult<Change>>((resolve) => {
          pendingResolve = resolve;
        });
      },
      return: (): Promise<IteratorResult<Change>> => {
        if (!ended) {
          ended = true;
          if (dispose) {
            dispose();
            dispose = null;
          }
          if (pendingResolve !== null) {
            const resolve = pendingResolve;
            pendingResolve = null;
            resolve({ value: undefined, done: true });
          }
        }
        return Promise.resolve({ value: undefined, done: true });
      },
    };
  }
}

// ═══════════════════════════════════════════════════════════════════════════
// FACTORY
// ═══════════════════════════════════════════════════════════════════════════

export function createSpaghettiLive(store: AgentDataStore, liveWatch: LiveWatch, errorSink?: ErrorSink): SpaghettiLive {
  return new SpaghettiLiveImpl(store, liveWatch, errorSink);
}

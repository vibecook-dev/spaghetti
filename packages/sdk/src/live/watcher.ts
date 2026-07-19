/**
 * watcher.ts — filesystem watcher abstraction for RFC 005 live updates.
 *
 * This file introduces the cross-platform `Watcher` interface that
 * `LiveUpdates` (landing in C2.7) will consume, plus two concrete
 * implementations:
 *
 *  - `createParcelWatcher()` wraps `@parcel/watcher`. It's the default:
 *    single-epoll on Linux (no inotify quota explosion), correct Windows
 *    buffer sizing, macOS FSEvents, and — critically for the "what
 *    changed while we were down" recovery path in RFC 005 §Filesystem
 *    Watcher — first-class `writeSnapshot`/`getEventsSince` APIs.
 *
 *  - `createChokidarWatcher()` is a pure-JS fallback for environments
 *    where the parcel native binary fails to load (odd platforms, some
 *    CI sandboxes). Snapshot support degrades here — callers that rely
 *    on snapshots must upgrade to the parcel backend. See the per-method
 *    notes below.
 *
 * Nothing from this module is exported from the package entry yet;
 * `packages/sdk/src/live/index.ts` and the public-API wiring are
 * deliberately deferred until C2.7.
 */

import parcelWatcher from '@parcel/watcher';
import chokidar from 'chokidar';
import { readdir, stat } from 'node:fs/promises';
import * as path from 'node:path';

// ═══════════════════════════════════════════════════════════════════════════
// PUBLIC TYPES
// ═══════════════════════════════════════════════════════════════════════════

/**
 * A single filesystem event, normalised across backends.
 *
 * Matches parcel's shape one-for-one so the parcel impl can pass events
 * straight through. The chokidar impl maps its richer event vocabulary
 * onto these three cases (see `createChokidarWatcher` below).
 */
export type WatchEvent = {
  type: 'create' | 'update' | 'delete';
  path: string;
};

/**
 * "Stop watching" handle returned by `subscribe`. May be async (parcel's
 * `AsyncSubscription.unsubscribe` returns a promise); the sync-void case
 * is kept so simpler backends don't have to fabricate a promise.
 */
export type Unsubscribe = () => Promise<void> | void;

/**
 * The narrow surface `LiveUpdates` depends on. Deliberately tiny:
 *
 *  - `subscribe` is the hot path — it fires on every fs event the
 *    backend coalesces for `rootPath`. `options.ignore` is an array of
 *    glob patterns the backend skips at the source (parcel) or we
 *    filter in JS (chokidar). `options.recursive` toggles whether
 *    subdirectories are watched — parcel is always recursive, so we
 *    accept `recursive: false` but ignore it (documented in the impl);
 *    chokidar honours it via `depth: 0`.
 *
 *  - `writeSnapshot` / `getEventsSince` are the "crash recovery"
 *    escape hatch. After `LiveUpdates.stop()` we write a snapshot;
 *    on next start we ask the backend for everything that changed
 *    between the snapshot and now. Parcel does this natively;
 *    chokidar can't, and throws.
 */
export interface Watcher {
  subscribe(
    rootPath: string,
    onEvents: (events: WatchEvent[]) => void,
    options: { ignore: string[]; recursive: boolean },
  ): Promise<Unsubscribe>;

  writeSnapshot(rootPath: string, snapshotFile: string): Promise<void>;

  getEventsSince(rootPath: string, snapshotFile: string): Promise<WatchEvent[]>;
}

export interface PollingWatcherOptions {
  /** Fast cadence for files that changed recently. Default 500 ms. */
  pollIntervalMs?: number;
  /** Full discovery cadence for new or resumed files. Default 5 seconds. */
  discoveryIntervalMs?: number;
  /** Optional shallow scan depth for cheap, fast new-session discovery. */
  quickDiscoveryDepth?: number;
  /** Cadence for the optional shallow scan. Defaults to pollIntervalMs. */
  quickDiscoveryIntervalMs?: number;
  /** Keep a recently-changing file on the fast lane. Default 5 minutes. */
  activeWindowMs?: number;
  /** Reconcile recent writes that raced watcher startup. Default 2 minutes. */
  initialLookbackMs?: number;
  /** Source-specific filter applied before stat calls. */
  shouldInclude?: (filePath: string) => boolean;
  onError?: (error: Error) => void;
}

interface FileStamp {
  mtimeMs: number;
  size: number;
}

function sameStamp(a: FileStamp | undefined, b: FileStamp): boolean {
  return !!a && a.mtimeMs === b.mtimeMs && a.size === b.size;
}

function globRegex(glob: string): RegExp {
  const directoryMarker = '\u0000';
  const globstarMarker = '\u0001';
  const escaped = glob
    .replaceAll('**/', directoryMarker)
    .replaceAll('**', globstarMarker)
    .replace(/[.+^${}()|[\]\\]/g, '\\$&')
    .replaceAll('*', '[^/]*')
    .replaceAll(directoryMarker, '(?:.*/)?')
    .replaceAll(globstarMarker, '.*');
  return new RegExp(`^(?:${escaped})$`);
}

/**
 * Descriptor-free reconciliation watcher. It polls only recently-active files
 * each second and performs a bounded discovery scan less frequently, so it
 * remains reliable when FSEvents/inotify handles are exhausted.
 */
export function createPollingWatcher(options: PollingWatcherOptions = {}): Watcher {
  const pollIntervalMs = options.pollIntervalMs ?? 500;
  const discoveryIntervalMs = Math.max(pollIntervalMs, options.discoveryIntervalMs ?? 5_000);
  const activeWindowMs = options.activeWindowMs ?? 5 * 60_000;
  const initialLookbackMs = options.initialLookbackMs ?? 2 * 60_000;
  const report = options.onError ?? (() => {});

  return {
    async subscribe(rootPath, onEvents, watchOptions) {
      const ignored = watchOptions.ignore.map(globRegex);
      const known = new Map<string, FileStamp>();
      const activeUntil = new Map<string, number>();
      let stopped = false;
      let running: Promise<void> | null = null;
      let lastDiscoveryAt = 0;
      let lastQuickDiscoveryAt = 0;

      const include = (filePath: string): boolean => {
        const relative = path.relative(rootPath, filePath).split(path.sep).join('/');
        if (ignored.some((pattern) => pattern.test(relative) || pattern.test(`/${relative}`))) return false;
        return options.shouldInclude?.(filePath) ?? true;
      };

      const readStamp = async (filePath: string): Promise<FileStamp | null> => {
        try {
          const info = await stat(filePath);
          return info.isFile() ? { mtimeMs: info.mtimeMs, size: info.size } : null;
        } catch (error) {
          const code = (error as NodeJS.ErrnoException).code;
          if (code !== 'ENOENT') report(error instanceof Error ? error : new Error(String(error)));
          return null;
        }
      };

      const scanDirectory = async (
        directory: string,
        output: Map<string, FileStamp>,
        depth = 0,
        maxDepth?: number,
      ): Promise<void> => {
        let entries;
        try {
          entries = await readdir(directory, { withFileTypes: true });
        } catch (error) {
          const code = (error as NodeJS.ErrnoException).code;
          if (code !== 'ENOENT') report(error instanceof Error ? error : new Error(String(error)));
          return;
        }
        await Promise.all(
          entries.map(async (entry) => {
            const fullPath = path.join(directory, entry.name);
            if (entry.isDirectory()) {
              if (watchOptions.recursive && (maxDepth === undefined || depth < maxDepth)) {
                await scanDirectory(fullPath, output, depth + 1, maxDepth);
              }
              return;
            }
            if (!entry.isFile() || !include(fullPath)) return;
            const stamp = await readStamp(fullPath);
            if (stamp) output.set(fullPath, stamp);
          }),
        );
      };

      const discover = async (initial: boolean, maxDepth?: number, partial = false): Promise<void> => {
        const now = Date.now();
        const next = new Map<string, FileStamp>();
        await scanDirectory(rootPath, next, 0, maxDepth);
        const events: WatchEvent[] = [];
        for (const [filePath, stamp] of next) {
          const previous = known.get(filePath);
          if (!previous) {
            if (!initial || stamp.mtimeMs >= now - initialLookbackMs) {
              events.push({ type: initial ? 'update' : 'create', path: filePath });
              activeUntil.set(filePath, now + activeWindowMs);
            }
          } else if (!sameStamp(previous, stamp)) {
            events.push({ type: 'update', path: filePath });
            activeUntil.set(filePath, now + activeWindowMs);
          }
        }
        if (!initial && !partial) {
          for (const filePath of known.keys()) {
            if (!next.has(filePath)) {
              events.push({ type: 'delete', path: filePath });
              activeUntil.delete(filePath);
            }
          }
        }
        if (!partial) known.clear();
        for (const [filePath, stamp] of next) known.set(filePath, stamp);
        if (partial) lastQuickDiscoveryAt = now;
        else lastDiscoveryAt = now;
        if (!stopped && events.length > 0) onEvents(events);
      };

      const pollActive = async (): Promise<void> => {
        const now = Date.now();
        const events: WatchEvent[] = [];
        await Promise.all(
          [...activeUntil].map(async ([filePath, until]) => {
            if (until < now) {
              activeUntil.delete(filePath);
              return;
            }
            const next = await readStamp(filePath);
            const previous = known.get(filePath);
            if (!next) {
              if (previous) events.push({ type: 'delete', path: filePath });
              known.delete(filePath);
              activeUntil.delete(filePath);
            } else if (!sameStamp(previous, next)) {
              known.set(filePath, next);
              activeUntil.set(filePath, now + activeWindowMs);
              events.push({ type: 'update', path: filePath });
            }
          }),
        );
        if (!stopped && events.length > 0) onEvents(events);
      };

      const tick = (): void => {
        if (stopped || running) return;
        const work = (async () => {
          await pollActive();
          if (
            options.quickDiscoveryDepth !== undefined &&
            Date.now() - lastQuickDiscoveryAt >= (options.quickDiscoveryIntervalMs ?? pollIntervalMs)
          ) {
            await discover(false, options.quickDiscoveryDepth, true);
          }
          if (Date.now() - lastDiscoveryAt >= discoveryIntervalMs) await discover(false);
        })()
          .catch((error: unknown) => report(error instanceof Error ? error : new Error(String(error))))
          .finally(() => {
            if (running === work) running = null;
          });
        running = work;
      };

      await discover(true);
      const timer = setInterval(tick, pollIntervalMs);
      return async () => {
        stopped = true;
        clearInterval(timer);
        await running;
      };
    },

    async writeSnapshot() {
      throw new Error('Polling watcher does not use persisted snapshots');
    },

    async getEventsSince() {
      throw new Error('Polling watcher does not use persisted snapshots');
    },
  };
}

export interface ResilientWatcherOptions extends PollingWatcherOptions {
  primary?: Watcher;
  onPrimaryUnavailable?: (error: Error) => void;
}

/** Native events for latency plus polling reconciliation for guaranteed recovery. */
export function createResilientWatcher(options: ResilientWatcherOptions = {}): Watcher {
  const primary = options.primary ?? createParcelWatcher();
  const polling = createPollingWatcher(options);
  return {
    async subscribe(rootPath, onEvents, watchOptions) {
      const stopPolling = await polling.subscribe(rootPath, onEvents, watchOptions);
      let stopPrimary: Unsubscribe | null = null;
      try {
        stopPrimary = await primary.subscribe(rootPath, onEvents, watchOptions);
      } catch (error) {
        options.onPrimaryUnavailable?.(error instanceof Error ? error : new Error(String(error)));
      }
      return async () => {
        try {
          await stopPrimary?.();
        } finally {
          await stopPolling();
        }
      };
    },
    writeSnapshot: (rootPath, snapshotFile) => primary.writeSnapshot(rootPath, snapshotFile),
    getEventsSince: (rootPath, snapshotFile) => primary.getEventsSince(rootPath, snapshotFile),
  };
}

// ═══════════════════════════════════════════════════════════════════════════
// PARCEL IMPL (default)
// ═══════════════════════════════════════════════════════════════════════════

/**
 * Thin wrapper over `@parcel/watcher`.
 *
 * Parcel's callback signature is `(err, events)`; on error we log to
 * stderr and drop the batch rather than propagate, mirroring how the
 * existing `file-service.ts` chokidar setup treats errors — live updates
 * should degrade, not crash the host process. `LiveUpdates` sits above
 * this and tracks saturation/error surfaces of its own.
 *
 * Parcel's own `Event.type` values (`'create' | 'update' | 'delete'`)
 * are identical to ours, so the mapping is a no-op clone.
 *
 * `recursive: false` is silently ignored — parcel is always recursive.
 * Callers that need non-recursive semantics (e.g. watching `~/.claude/`
 * without descending into `projects/`) must filter events via `ignore`
 * globs. This is acceptable because the non-recursive scopes in RFC 005
 * §Filesystem Watcher are all leaf-ish (`todos/`, `plans/`, top-level
 * `~/.claude/`) and wouldn't benefit from recursion anyway.
 */
export function createParcelWatcher(): Watcher {
  return {
    async subscribe(rootPath, onEvents, options) {
      const subscription = await parcelWatcher.subscribe(
        rootPath,
        (err, events) => {
          if (err) {
            // Surface to stderr. `LiveUpdates.onError` hook lives one
            // layer up — we don't have a reference to it here, and
            // pushing an error through `onEvents` would conflate
            // success/failure signalling.

            console.error('[spaghetti-sdk] parcel watcher error:', err);
            return;
          }
          if (events.length === 0) return;
          // Parcel's Event shape is `{ type, path }` — a structural
          // match for `WatchEvent`. Map-to-new-object to avoid leaking
          // any future extra fields parcel might add.
          onEvents(events.map((e) => ({ type: e.type, path: e.path })));
        },
        { ignore: options.ignore },
      );
      // `recursive: false` is accepted for interface compatibility but
      // not honoured — see the doc-comment on `createParcelWatcher`.
      void options.recursive;
      return () => subscription.unsubscribe();
    },

    async writeSnapshot(rootPath, snapshotFile) {
      await parcelWatcher.writeSnapshot(rootPath, snapshotFile);
    },

    async getEventsSince(rootPath, snapshotFile) {
      const events = await parcelWatcher.getEventsSince(rootPath, snapshotFile);
      return events.map((e) => ({ type: e.type, path: e.path }));
    },
  };
}

// ═══════════════════════════════════════════════════════════════════════════
// CHOKIDAR IMPL (fallback)
// ═══════════════════════════════════════════════════════════════════════════

const SNAPSHOT_UNSUPPORTED_MESSAGE = 'Chokidar backend does not support snapshots — upgrade to @parcel/watcher';

/**
 * Pure-JS fallback for platforms where `@parcel/watcher` fails to load.
 *
 * Event mapping:
 *
 *   chokidar `add`      → `create`   (new file observed)
 *   chokidar `change`   → `update`   (existing file content changed)
 *   chokidar `unlink`   → `delete`   (file removed)
 *   chokidar `addDir`   → (ignored — directory creates are noise; the
 *                         consumer only cares about file contents)
 *   chokidar `unlinkDir`→ (ignored — same rationale; individual file
 *                         unlinks inside fire separately)
 *
 * `ignoreInitial: true` matches parcel's semantics — we don't fire
 * synthetic events for files that already exist when the watcher
 * attaches; the warm-start path reconciles those on process startup.
 *
 * `writeSnapshot` / `getEventsSince` throw unconditionally. Consumers
 * relying on the snapshot-based recovery path must use the parcel
 * backend — documented in the throw message so runtime errors point at
 * the fix.
 */
export function createChokidarWatcher(): Watcher {
  return {
    async subscribe(rootPath, onEvents, options) {
      const watcher = chokidar.watch(rootPath, {
        ignored: options.ignore.length > 0 ? options.ignore : undefined,
        ignoreInitial: true,
        persistent: true,
        // `depth: 0` keeps chokidar from descending when the caller
        // explicitly asked for a non-recursive watch. Without this,
        // chokidar follows every subdirectory by default.
        depth: options.recursive ? undefined : 0,
      });

      const emit = (type: WatchEvent['type'], path: string): void => {
        onEvents([{ type, path }]);
      };

      watcher.on('add', (p) => emit('create', p));
      watcher.on('change', (p) => emit('update', p));
      watcher.on('unlink', (p) => emit('delete', p));
      // addDir / unlinkDir: intentionally unhandled — see doc-comment.

      // chokidar's `ready` event fires once the initial scan completes.
      // We wait for it so callers observing the returned `Unsubscribe`
      // don't race against pending bootstrapping work.
      await new Promise<void>((resolve, reject) => {
        const onReady = (): void => {
          watcher.off('error', onError);
          resolve();
        };
        const onError = (err: unknown): void => {
          watcher.off('ready', onReady);
          reject(err instanceof Error ? err : new Error(String(err)));
        };
        watcher.once('ready', onReady);
        watcher.once('error', onError);
      });

      return () => watcher.close();
    },

    async writeSnapshot(_rootPath, _snapshotFile) {
      throw new Error(SNAPSHOT_UNSUPPORTED_MESSAGE);
    },

    async getEventsSince(_rootPath, _snapshotFile) {
      throw new Error(SNAPSHOT_UNSUPPORTED_MESSAGE);
    },
  };
}

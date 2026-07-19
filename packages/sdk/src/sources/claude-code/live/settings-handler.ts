/**
 * settings-handler.ts — Settings re-parse + cache refresh + emit (RFC 005 C5.5).
 *
 * Extracted from `live-updates.ts` so the orchestrator focuses on
 * watcher / queue / writer-loop wiring. Watcher events for
 * `settings.json` and `settings.local.json` bypass the SQLite write
 * path entirely; this handler owns the trailing-edge debounce, the
 * fileService re-read, the in-memory `AgentConfig` cache refresh on
 * the store, and the `settings.changed` emit.
 *
 * Behavior identical to the inline pre-extraction version:
 *
 *   - 150 ms trailing-edge coalescer per absolute path absorbs the
 *     `delete + create` flicker that editors produce on atomic-rename
 *     saves (write-tmp → rename-over). See
 *     `docs/LIVE-UPDATES-DESIGN.md` §C5.5 for why 150 ms is the
 *     shortest reliable window on macOS APFS + parcel-watcher.
 *
 *   - Corrupt mid-write JSON is logged via the supplied error sink
 *     and discarded; the next event retries.
 *
 *   - `settings.json` populates `AgentConfig.settings`; if the store
 *     hasn't been seeded by cold-start yet (unit-test or live-only
 *     flow), a minimal empty AgentConfig is built so the new value is
 *     immediately queryable via `store.getConfig()`.
 *
 *   - `settings.local.json` has no AgentConfig slot today; the event
 *     fires with the parsed payload but the store cache is untouched.
 */

import type { FileService } from '../../../io/file-service.js';
import type { AgentDataStore } from '../../../data/agent-data-store.js';
import type { Change } from '../../../live/change-events.js';
import type { AgentConfig, SettingsFile } from '../../../types/index.js';

// ═══════════════════════════════════════════════════════════════════════════
// PUBLIC TYPES
// ═══════════════════════════════════════════════════════════════════════════

export type SettingsCategory = 'settings' | 'settings_local';

export interface SettingsHandlerDeps {
  fileService: FileService;
  /**
   * Returns the current store. A function (rather than a direct
   * reference) so the orchestrator's `attachStore()` seam can swap
   * the store after the handler is constructed without re-creating
   * it.
   */
  getStore: () => AgentDataStore;
  /**
   * Where corrupt-write / read failures surface. Matches the
   * orchestrator's `onError` discipline (errors degrade, never crash).
   */
  onError: (err: Error) => void;
  /**
   * Predicate read once per debounce-fire so an in-flight handler can
   * short-circuit if the orchestrator has already begun shutting down.
   * Returns `true` when it's still safe to read + emit.
   */
  isRunning: () => boolean;
}

export interface SettingsHandlerOptions {
  /**
   * Trailing-edge debounce window, ms. Defaults to 150 — the
   * shortest reliable window for collapsing atomic-rename
   * delete+create pairs on macOS APFS + parcel-watcher.
   */
  debounceMs?: number;
}

export interface SettingsHandler {
  /**
   * Schedule (or reschedule) a re-parse for `absPath`. Trailing-edge:
   * a fresh event for the same path resets the timer.
   */
  handle(absPath: string, category: SettingsCategory): void;
  /**
   * Cancel every pending debounce timer. Idempotent. Safe to call
   * multiple times during shutdown.
   */
  stop(): void;
}

// ═══════════════════════════════════════════════════════════════════════════
// DEFAULTS
// ═══════════════════════════════════════════════════════════════════════════

const DEFAULT_DEBOUNCE_MS = 150;

// ═══════════════════════════════════════════════════════════════════════════
// FACTORY
// ═══════════════════════════════════════════════════════════════════════════

export function createSettingsHandler(
  deps: SettingsHandlerDeps,
  options: SettingsHandlerOptions = {},
): SettingsHandler {
  const debounceMs = options.debounceMs ?? DEFAULT_DEBOUNCE_MS;
  const debounceByPath = new Map<string, ReturnType<typeof setTimeout>>();

  /**
   * Re-parse a settings file, refresh the in-memory AgentConfig on the
   * store, and emit `settings.changed`. Never throws — corrupt
   * mid-write JSON is logged via `onError` and discarded, so the next
   * event retries.
   *
   * We read the file ourselves rather than going through
   * `fileService.readJsonSync` because that helper emits `error` on its
   * EventEmitter surface when JSON.parse fails, which surfaces as an
   * unhandled `error` event in processes (like tests) that don't
   * register a listener on the file service.
   */
  function handleSettingsEvent(absPath: string, category: SettingsCategory): void {
    if (!deps.isRunning()) return;
    let parsed: SettingsFile;
    try {
      const content = deps.fileService.readFileSync(absPath);
      parsed = JSON.parse(content) as SettingsFile;
    } catch (err) {
      // Corrupt mid-write / missing / permission denied all land here.
      // Swallow, surface via onError, let the next event retry.
      deps.onError(
        err instanceof Error
          ? new Error(`[ClaudeCodeLiveUpdates] failed to parse ${category} at ${absPath}: ${err.message}`)
          : new Error(`[ClaudeCodeLiveUpdates] failed to parse ${category} at ${absPath}`),
      );
      return;
    }

    // `settings.json` populates AgentConfig.settings. If the store
    // hasn't been seeded by cold-start yet (unit-test flow, or a
    // live-only consumer), build a minimal AgentConfig so the new
    // settings are queryable via `getConfig()` immediately.
    // `settings.local.json` has no cold-start slot yet (see
    // PARSER-UNPARSED-DATA.md §1.5); for now we emit the event with
    // the parsed payload and leave the store cache alone — consumers
    // still get the live data through the event. Extending
    // AgentConfig with a `settingsLocal` field is a follow-up RFC.
    const store = deps.getStore();
    if (category === 'settings') {
      try {
        const current = store.hasConfig() ? store.getConfig() : buildEmptyAgentConfig();
        store.setConfig({ ...current, settings: parsed });
      } catch (err) {
        deps.onError(err instanceof Error ? err : new Error(String(err)));
      }
    }

    const change: Change = {
      type: 'settings.changed',
      sourceId: 'claude-code',
      seq: 0, // store.emit() stamps the real value
      ts: Date.now(),
      file: category === 'settings' ? 'settings' : 'settings.local',
      settings: parsed,
    };
    try {
      store.emit(change);
    } catch (err) {
      deps.onError(err instanceof Error ? err : new Error(String(err)));
    }
  }

  return {
    handle(absPath: string, category: SettingsCategory): void {
      const existing = debounceByPath.get(absPath);
      if (existing !== undefined) clearTimeout(existing);
      const timer = setTimeout(() => {
        debounceByPath.delete(absPath);
        handleSettingsEvent(absPath, category);
      }, debounceMs);
      debounceByPath.set(absPath, timer);
    },
    stop(): void {
      for (const timer of debounceByPath.values()) clearTimeout(timer);
      debounceByPath.clear();
    },
  };
}

// ═══════════════════════════════════════════════════════════════════════════
// HELPERS
// ═══════════════════════════════════════════════════════════════════════════

/**
 * Minimal `AgentConfig` shape used as a seed when settings live-update
 * lands before any cold-start has populated the store. Mirrors
 * `ConfigParserImpl.empty()` in `parser/config-parser.ts` — kept inline
 * to avoid a runtime dep from live/ into parser/.
 */
function buildEmptyAgentConfig(): AgentConfig {
  return {
    settings: { permissions: { allow: [] as string[] } },
    settingsLocal: null,
    plugins: {
      installedPlugins: { version: 2 as const, plugins: {} },
      knownMarketplaces: {},
      installCountsCache: { version: 1 as const, fetchedAt: '', counts: [] },
      cache: [],
      marketplaces: [],
      blocklist: null,
    },
    statsig: {},
    ide: { lockFiles: [] },
    shellSnapshots: { snapshots: [] },
    cache: {},
    statusLineCommand: null,
    teams: [],
    mcpNeedsAuth: null,
  };
}

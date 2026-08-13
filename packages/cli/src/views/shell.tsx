/**
 * Shell — Root Ink component managing the view stack
 *
 * Handles breadcrumb derivation, key dispatch (q/Ctrl-C quit),
 * and rendering the active view with Header + Footer chrome.
 *
 * Phase 3: Shows BootView during initialization, then transitions
 * to ProjectsView when the service is ready.
 */

import React, { useState, useCallback, useEffect, useRef } from 'react';
import { Box, Text, useApp, useInput } from 'ink';
import { createRequire } from 'node:module';
import type { ObservationService } from '@vibecook/spaghetti-sdk/observation';
import type { ViewEntry, ViewNav, ViewContext } from './types.js';
import { ViewNavProvider } from './context.js';
import { Header, Footer, HRule } from './chrome.js';
import { MenuView } from './menu-view.js';
import { SearchView } from './search-view.js';
import { BootView } from './boot-view.js';
import { SearchInput } from './search-input.js';
import { ErrorBoundary } from './error-boundary.js';
import { useAlternateScreen } from './hooks.js';

// ─── Version ──────────────────────────────────────────────────────────

const _require = createRequire(import.meta.url);
export const VERSION = (_require('../package.json') as { version: string }).version;

// ─── API Context ───────────────────────────────────────────────────────

const ApiContext = React.createContext<ObservationService>(null!);
export const ApiProvider = ApiContext.Provider;
export function useApi(): ObservationService {
  return React.useContext(ApiContext);
}

// ─── Context Derivation ────────────────────────────────────────────────

function deriveContext(stack: ViewEntry[]): ViewContext {
  const ctx: ViewContext = {};
  for (const entry of stack) {
    // Extract context from component closures via breadcrumb analysis
    // The actual context is set by views via nav.push() with the right data
    // We store it in a side-channel on the ViewEntry
    if ((entry as any)._project) ctx.project = (entry as any)._project;
    if ((entry as any)._session) ctx.session = (entry as any)._session;
    if ((entry as any)._sessionIndex !== undefined) ctx.sessionIndex = (entry as any)._sessionIndex;
  }
  return ctx;
}

// ─── Breadcrumb Builder ────────────────────────────────────────────────

function buildBreadcrumb(stack: ViewEntry[]): string {
  return stack.map((v) => v.breadcrumb).join(' \u203A ');
}

// ─── Default Hints ─────────────────────────────────────────────────────

function defaultHints(entry: ViewEntry, isRoot: boolean): string {
  if (entry.hints) return entry.hints;
  const parts: string[] = ['\u2191\u2193 navigate'];
  if (entry.type === 'messages') parts.push('1-6 filter');
  if (entry.type === 'detail') {
    parts[0] = '\u2191\u2193 scroll';
  } else {
    parts.push('\u23CE open');
  }
  parts.push('/ search');
  if (!isRoot) parts.push('Esc back');
  parts.push('q quit');
  return parts.join('  ');
}

// ─── Shell Component ───────────────────────────────────────────────────

export interface ShellProps {
  api: ObservationService;
}

export function Shell({ api }: ShellProps): React.ReactElement {
  const { exit } = useApp();

  // Enter alternate screen buffer for a clean full-screen TUI canvas
  useAlternateScreen();

  // RFC 011 makes the Rust observation engine the only production owner.
  const engine = 'rs' as const;

  // ── Initialization lifecycle ──────────────────────────────────────

  const [ready, setReady] = useState(() => api.isReady());
  const [progress, setProgress] = useState({ message: 'Initializing...', current: 0, total: 0 });
  const [elapsed, setElapsed] = useState(0);
  const [error, setError] = useState<string | null>(null);
  const startTimeRef = useRef(Date.now());

  useEffect(() => {
    // If already initialized (e.g. bin.ts called initialize() before rendering Shell),
    // skip the boot screen entirely.
    if (api.isReady()) {
      setReady(true);
      return;
    }

    // Core parsing yields the event loop between projects (via setImmediate),
    // so we can update progress state on each yield. Subscribe to progress
    // events and update React state directly — the setImmediate yields give
    // Ink a chance to re-render between projects.
    const unsub = api.onProgress((p) => {
      setProgress({
        message: p.message,
        current: p.current ?? 0,
        total: p.total ?? 0,
      });
      setElapsed((Date.now() - startTimeRef.current) / 1000);
    });

    // Delay init start so Ink can paint the initial boot screen frame
    const initTimer = setTimeout(() => {
      void (async () => {
        try {
          await api.initialize();
          // Probe a cheap read: a corrupt cache can survive initialize() and
          // only fail when the menu queries (SQLITE_CORRUPT). Auto-rebuild once.
          try {
            await api.getProjectList();
          } catch (probeErr) {
            const msg = probeErr instanceof Error ? probeErr.message : String(probeErr);
            if (/malformed|SQLITE_CORRUPT|corrupt/i.test(msg)) {
              setProgress({ message: 'Index corrupt — rebuilding…', current: 0, total: 0 });
              await api.rebuildIndex();
              await api.getProjectList();
            } else {
              throw probeErr;
            }
          }
          unsub();
          setReady(true);
        } catch (err) {
          unsub();
          const message = err instanceof Error ? err.message : String(err);
          const hint = /malformed|SQLITE_CORRUPT|corrupt/i.test(message)
            ? `${message} — delete ~/.spaghetti/cache/spaghetti-rs.db and retry`
            : message;
          setError(hint);
        }
      })();
    }, 100);

    return () => {
      clearTimeout(initTimer);
      unsub();
    };
  }, [api]);

  // ── View stack ────────────────────────────────────────────────────

  const initialView: ViewEntry = {
    type: 'menu',
    component: MenuView,
    breadcrumb: '',
  };

  const [stack, setStack] = useState<ViewEntry[]>([initialView]);

  const push = useCallback((view: ViewEntry) => {
    setStack((s) => [...s, view]);
  }, []);

  const pop = useCallback(() => {
    setStack((s) => (s.length > 1 ? s.slice(0, -1) : s));
  }, []);

  const replace = useCallback((view: ViewEntry) => {
    setStack((s) => [...s.slice(0, -1), view]);
  }, []);

  const popAndPush = useCallback((...views: ViewEntry[]) => {
    setStack((s) => [...s.slice(0, -1), ...views]);
  }, []);

  const quit = useCallback(() => {
    exit();
  }, [exit]);

  // ── Search mode ─────────────────────────────────────────────────

  const [searchMode, setSearchMode] = useState(false);
  const [flash, setFlash] = useState<string | null>(null);

  // Auto-dismiss flash messages after 2 seconds
  useEffect(() => {
    if (flash) {
      const timer = setTimeout(() => setFlash(null), 2000);
      return () => clearTimeout(timer);
    }
  }, [flash]);

  const enterSearchMode = useCallback(() => {
    setSearchMode(true);
    setFlash(null);
  }, []);

  const exitSearchMode = useCallback(() => {
    setSearchMode(false);
  }, []);

  const handleSearch = useCallback(
    (query: string) => {
      setSearchMode(false);
      const entry: ViewEntry = {
        type: 'search',
        component: () => <SearchView query={query} />,
        breadcrumb: `Search: "${query}"`,
        hints: '\u2191\u2193 navigate  \u23CE jump to message  Esc back  / new search',
      };
      push(entry);
    },
    [push],
  );

  const top = stack[stack.length - 1];
  const context = deriveContext(stack);

  const nav: ViewNav = {
    push,
    pop,
    replace,
    popAndPush,
    quit,
    enterSearchMode,
    context,
    searchMode,
  };

  const breadcrumb = buildBreadcrumb(stack);
  const isRoot = stack.length === 1;
  const hints = defaultHints(top, isRoot);

  // Global keys — handled at shell level so they work everywhere.
  // Suppressed when search mode is active (SearchInput handles input).
  useInput(
    (input, _key) => {
      if (input === 'q') {
        quit();
      }
      if (input === '/') {
        enterSearchMode();
      }
    },
    { isActive: !searchMode },
  );

  // Dismiss flash on any keypress when flash is showing and not in search mode
  useInput(
    () => {
      if (flash) {
        setFlash(null);
      }
    },
    { isActive: !searchMode && flash !== null },
  );

  // ── Render ────────────────────────────────────────────────────────

  // Show boot screen while initializing
  if (!ready) {
    return <BootView version={VERSION} progress={progress} elapsed={elapsed} error={error} onQuit={quit} />;
  }

  // Main TUI with view stack
  const TopView = top.component;

  // Determine what to render in the footer area
  let footerContent: React.ReactElement;
  if (searchMode) {
    footerContent = <SearchInput onSearch={handleSearch} onCancel={exitSearchMode} />;
  } else if (flash) {
    footerContent = (
      <Box flexDirection="column">
        <HRule />
        <Text> {flash}</Text>
        <HRule />
      </Box>
    );
  } else {
    footerContent = <Footer hints={hints} engine={engine} />;
  }

  return (
    <ApiProvider value={api}>
      <ViewNavProvider value={nav}>
        <Box flexDirection="column">
          {!isRoot && top.type !== 'project-tabs' && top.type !== 'session-tabs' && <Header breadcrumb={breadcrumb} />}
          <Box flexGrow={1} flexDirection="column">
            {/* Isolate the active view: a query-time throw shows a panel instead
                of crashing the whole TUI. resetKey clears it on navigation. */}
            <ErrorBoundary resetKey={stack.length} canGoBack={!isRoot} onBack={pop} onQuit={quit}>
              <TopView />
            </ErrorBoundary>
          </Box>
          {footerContent}
        </Box>
      </ViewNavProvider>
    </ApiProvider>
  );
}

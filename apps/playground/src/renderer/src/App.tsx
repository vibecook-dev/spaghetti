import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { Library, Moon, PanelLeft, Search, Sun } from 'lucide-react';
import { TrafficLights } from './components/TrafficLights.js';
import { SpaghettiProvider, type SpaghettiProviderProps } from '@vibecook/spaghetti-sdk/react';
import type { ProjectListItem, SessionListItem, StoreStats } from '@vibecook/spaghetti-sdk';
import { createIpcApi } from './ipc-api.js';
import { LoadingScreen } from './components/LoadingScreen.js';
import { SourceBadge } from './components/SourceBadge.js';
import { SessionMessagesView } from './components/SessionMessagesView.js';
import { SearchOverlay, type SearchNavigateTarget } from './components/SearchOverlay.js';
import { FileExplorerPanel } from './components/FileExplorerPanel.js';
import { Btn, Chip, Dot, EmptyState, Kbd } from './components/ui.js';
import {
  flattenPrompt,
  formatBytes,
  formatDuration,
  formatNumber,
  formatRelativeTime,
  formatTokenUsage,
} from './lib/format.js';
import {
  applyProgressEvent,
  initialSourceStates,
  sourceLabel,
  type ProgressSnapshot,
  type SourceProgressState,
} from './lib/source-progress.js';
import { paperStyle } from './lib/archive-theme.js';

/**
 * Electron playground shell — archive / paper design (spaghetti-ui-design).
 *
 * Never import runtime values from `@vibecook/spaghetti-sdk` (main entry) in
 * the renderer — that package pulls Node natives and will blank the window.
 */

interface ProjectKey {
  slug: string;
  sourceId: string;
}

function projectKey(p: ProjectKey): string {
  return `${p.sourceId}:${p.slug}`;
}

export function App() {
  let api: ReturnType<typeof createIpcApi>;
  try {
    api = createIpcApi();
  } catch (err) {
    return (
      <div className="p-6 font-mono text-xs h-full bg-chrome text-ink">
        <div className="font-medium mb-2 tracking-[0.12em] uppercase text-[10px] opacity-50">
          Preload bridge missing
        </div>
        <div className="opacity-80">{String(err)}</div>
      </div>
    );
  }

  return (
    <SpaghettiProvider api={api as SpaghettiProviderProps['api']}>
      <PlaygroundShell />
    </SpaghettiProvider>
  );
}

function PlaygroundShell() {
  const [ready, setReady] = useState(false);
  const [engine, setEngine] = useState<'rs' | 'ts' | null>(null);
  const [rebuilding, setRebuilding] = useState(false);
  const [projects, setProjects] = useState<ProjectListItem[]>([]);
  const [selected, setSelected] = useState<ProjectKey | null>(null);
  const [sessions, setSessions] = useState<SessionListItem[]>([]);
  const [selectedSession, setSelectedSession] = useState<{
    session: SessionListItem;
    index: number;
  } | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [changeNonce, setChangeNonce] = useState(0);
  const [projectPrompts, setProjectPrompts] = useState<Record<string, string>>({});
  const [stats, setStats] = useState<StoreStats | null>(null);
  const [sourceFilter, setSourceFilter] = useState<string | null>(null);
  const [searchOpen, setSearchOpen] = useState(false);
  const [filesOpen, setFilesOpen] = useState(true);
  const [leftOpen, setLeftOpen] = useState(true);
  // The reference opens on warm paper; dark parchment is the alternate illumination.
  const [isDark, setIsDark] = useState(false);
  const pendingSessionId = useRef<string | null>(null);

  const [sources, setSources] = useState<SourceProgressState[]>(() => initialSourceStates());
  const [progress, setProgress] = useState<ProgressSnapshot | null>(null);
  const [elapsedMs, setElapsedMs] = useState(0);
  const [loadHeadline, setLoadHeadline] = useState('Indexing agent history');
  const [retrying, setRetrying] = useState(false);
  const loadStartedAt = useRef(Date.now());

  // Persist theme class on <html>
  useEffect(() => {
    document.documentElement.classList.toggle('dark', isDark);
  }, [isDark]);

  useEffect(() => {
    if (ready && !rebuilding && !retrying) return;
    const id = window.setInterval(() => {
      setElapsedMs(Date.now() - loadStartedAt.current);
    }, 250);
    return () => window.clearInterval(id);
  }, [ready, rebuilding, retrying]);

  useEffect(() => {
    if (!ready) return;
    const onKey = (e: KeyboardEvent) => {
      if (!(e.metaKey || e.ctrlKey)) return;
      const k = e.key.toLowerCase();
      if (k === 'k') {
        e.preventDefault();
        setSearchOpen(true);
      } else if (k === 'b') {
        e.preventDefault();
        setFilesOpen((v) => !v);
      } else if (k === '\\') {
        e.preventDefault();
        setLeftOpen((v) => !v);
      }
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [ready]);

  useEffect(() => {
    const bridge = window.spaghetti;

    const unsubProgress = bridge.onProgress((p) => {
      const snap: ProgressSnapshot = {
        phase: p.phase,
        message: p.message,
        current: p.current,
        total: p.total,
      };
      const msg = p.message.toLowerCase();
      if (
        msg.includes('malformed') ||
        msg.includes('wiped for re-ingest') ||
        msg.includes('re-ingesting from disk') ||
        msg.includes('corrupted')
      ) {
        setError(null);
        setRetrying(false);
        setSources(initialSourceStates());
        setLoadHeadline('Recovering index');
      }
      setProgress(snap);
      setSources((prev) => applyProgressEvent(prev, snap));
    });

    const unsubReady = bridge.onReady((info) => {
      setSources((prev) =>
        prev.map((s) =>
          s.stage === 'pending' || s.stage === 'active' ? { ...s, stage: 'done' as const, fraction: 1 } : s,
        ),
      );
      setReady(true);
      setRebuilding(false);
      setRetrying(false);
      setError(null);
      setProgress({
        phase: 'indexing',
        message: `Ready in ${info.durationMs}ms`,
        current: 1,
        total: 1,
      });
    });

    const unsubChange = bridge.onChange(() => {
      setChangeNonce((n) => n + 1);
    });

    const unsubInitError = bridge.onInitError((message) => {
      setRetrying(false);
      setError(message);
    });

    let cancelled = false;
    const pollReady = async () => {
      for (let i = 0; i < 600 && !cancelled; i++) {
        try {
          if (await bridge.isReady()) {
            if (!cancelled) {
              setSources((prev) => prev.map((s) => ({ ...s, stage: 'done' as const, fraction: s.fraction ?? 1 })));
              setReady(true);
            }
            return;
          }
        } catch {
          /* not created yet */
        }
        await new Promise((r) => setTimeout(r, 500));
      }
    };
    void pollReady();
    void bridge
      .getEngine()
      .then(setEngine)
      .catch((e: unknown) => setError(String(e)));

    return () => {
      cancelled = true;
      unsubProgress();
      unsubReady();
      unsubChange();
      unsubInitError();
    };
  }, []);

  useEffect(() => {
    if (!ready) return;
    window.spaghetti
      .getProjectList()
      .then(setProjects)
      .catch((e: unknown) => setError(String(e)));
    window.spaghetti
      .getStats()
      .then(setStats)
      .catch(() => setStats(null));
  }, [ready, changeNonce]);

  useEffect(() => {
    if (!ready || projects.length === 0) return;
    let cancelled = false;
    const run = async () => {
      const next: Record<string, string> = {};
      const queue = [...projects];
      const workers = Array.from({ length: Math.min(6, queue.length) }, async () => {
        while (queue.length > 0 && !cancelled) {
          const p = queue.shift()!;
          const key = projectKey(p);
          try {
            const sess = await window.spaghetti.getSessionList(p.slug, { sourceId: p.sourceId });
            next[key] = sess[0]?.firstPrompt || sess[0]?.summary || '';
          } catch {
            next[key] = '';
          }
        }
      });
      await Promise.all(workers);
      if (!cancelled) setProjectPrompts((prev) => ({ ...prev, ...next }));
    };
    void run();
    return () => {
      cancelled = true;
    };
  }, [ready, projects, changeNonce]);

  useEffect(() => {
    if (!selected) {
      setSessions([]);
      return;
    }
    window.spaghetti
      .getSessionList(selected.slug, { sourceId: selected.sourceId })
      .then((list) => {
        setSessions(list);
        const want = pendingSessionId.current;
        if (want) {
          pendingSessionId.current = null;
          const idx = list.findIndex((s) => s.sessionId === want);
          if (idx >= 0) {
            setSelectedSession({ session: list[idx], index: idx });
          }
        }
      })
      .catch((e: unknown) => setError(String(e)));
  }, [selected, changeNonce]);

  const onRebuild = async () => {
    if (rebuilding || retrying) return;
    setRebuilding(true);
    setError(null);
    setLoadHeadline('Rebuilding index');
    loadStartedAt.current = Date.now();
    setElapsedMs(0);
    setSources(initialSourceStates());
    setProgress({ phase: 'parsing', message: 'Starting full rebuild…' });
    try {
      const { durationMs } = await window.spaghetti.rebuildIndex();
      setSources((prev) => prev.map((s) => ({ ...s, stage: 'done' as const, fraction: 1 })));
      setProgress({ phase: 'indexing', message: `Rebuilt in ${durationMs}ms`, current: 1, total: 1 });
      setReady(true);
      setChangeNonce((n) => n + 1);
    } catch (e: unknown) {
      setError(String(e));
    } finally {
      setRebuilding(false);
    }
  };

  const onRetryInit = async () => {
    if (retrying || rebuilding) return;
    setRetrying(true);
    setError(null);
    setReady(false);
    setLoadHeadline('Recovering index');
    loadStartedAt.current = Date.now();
    setElapsedMs(0);
    setSources(initialSourceStates());
    setProgress({ phase: 'parsing', message: 'Deleting cache and re-ingesting from disk…' });
    try {
      await window.spaghetti.retryInit();
      if (await window.spaghetti.isReady()) {
        setReady(true);
        setRetrying(false);
        setChangeNonce((n) => n + 1);
      }
    } catch (e: unknown) {
      setError(String(e));
      setRetrying(false);
    }
  };

  const onSearchNavigate = useCallback((target: SearchNavigateTarget) => {
    setSelected({ slug: target.projectSlug, sourceId: target.sourceId });
    setSelectedSession(null);
    pendingSessionId.current = target.sessionId ?? null;
  }, []);

  const sourceIds = useMemo(() => [...new Set(projects.map((p) => p.sourceId))].sort(), [projects]);

  const filteredProjects = useMemo(() => {
    if (!sourceFilter) return projects;
    return projects.filter((p) => p.sourceId === sourceFilter);
  }, [projects, sourceFilter]);

  const selectedKey = selected ? projectKey(selected) : null;
  const selectedProject = selected ? projects.find((p) => projectKey(p) === selectedKey) : null;

  const scopeProject = selectedProject
    ? { slug: selectedProject.slug, sourceId: selectedProject.sourceId, folderName: selectedProject.folderName }
    : null;

  const modKey = typeof navigator !== 'undefined' && /Mac|iPhone|iPad/.test(navigator.platform) ? '⌘' : 'Ctrl+';

  if (!ready || rebuilding || retrying || error) {
    return (
      <LoadingScreen
        sources={sources}
        progress={progress}
        elapsedMs={elapsedMs}
        engine={engine}
        error={error}
        headline={loadHeadline}
        onRetry={error && !retrying ? () => void onRetryInit() : undefined}
        retrying={retrying}
        isDark={isDark}
      />
    );
  }

  return (
    <div
      className={`h-full w-full flex flex-col overflow-hidden relative rounded-none border border-[color:var(--archive-ink-line-outer)] transition-colors duration-500 ${
        isDark
          ? 'dark text-[#d4cbbd] selection:bg-[#d4cbbd] selection:text-[#0a0908]'
          : 'text-[#2b2623] selection:bg-[#2b2623] selection:text-[#d1ccc0]'
      }`}
      style={paperStyle(isDark)}
    >
      {/* GLOBAL HEADER — drag region; preserve Electron controls without changing the archive rhythm. */}
      <header className="titlebar-drag h-12 border-b border-[color:var(--archive-ink-line-header)] flex items-center justify-between px-6 shrink-0 bg-transparent gap-4">
        <div className="flex items-center gap-6 min-w-0">
          <TrafficLights />
          <h1 className="text-[11px] font-serif tracking-[0.2em] flex items-center gap-3 shrink-0">
            <Library size={14} className="opacity-80" />
            spaghetti
          </h1>
          <div className="hidden md:flex gap-4 text-[9px] font-mono tracking-widest uppercase opacity-70">
            <span className="flex items-center gap-1">
              <span className="w-1.5 h-1.5 bg-ink inline-block" />
              {engine === 'ts' ? 'TypeScript' : 'Native'}
            </span>
            <span>Ref: Local</span>
          </div>
        </div>

        <div className="titlebar-no-drag flex items-center gap-4 font-mono text-[9px] uppercase tracking-widest min-w-0">
          {stats && (
            <span className="hidden sm:inline-block opacity-60 truncate">
              {formatNumber(stats.totalSegments)} segs · {formatBytes(stats.dbSizeBytes)} ·{' '}
              {formatNumber(stats.searchIndexed)} indexed
            </span>
          )}

          <div className="flex items-center gap-2 ml-2 shrink-0">
            <button
              type="button"
              onClick={() => setIsDark((v) => !v)}
              className="p-1 border border-transparent hover:border-[color:var(--archive-ink-line-header)] transition-colors rounded-none bg-transparent text-ink cursor-pointer"
              title="Toggle illumination"
            >
              {isDark ? <Sun size={12} /> : <Moon size={12} />}
            </button>
            <span className="opacity-30">|</span>
            <button
              type="button"
              onClick={() => setSearchOpen(true)}
              className="flex items-center gap-2 hover:bg-ink hover:text-paper transition-colors px-2 py-0.5 bg-transparent text-ink cursor-pointer border-0 font-mono text-[9px] tracking-widest uppercase"
              title={`Search (${modKey}K)`}
            >
              <Search size={10} /> Search
            </button>
            <button
              type="button"
              onClick={() => void onRebuild()}
              disabled={rebuilding}
              className="hidden lg:inline-flex items-center hover:bg-ink hover:text-paper transition-colors px-2 py-0.5 bg-transparent text-ink border-0 font-mono text-[9px] tracking-widest uppercase cursor-pointer disabled:opacity-30"
              title="Force full rebuild"
            >
              Rebuild
            </button>
          </div>
        </div>
      </header>

      <div className="flex-1 flex overflow-hidden relative min-h-0">
        {/* LEFT: Projects */}
        {leftOpen ? (
          <aside className="w-64 border-r border-[color:var(--archive-ink-line)] flex flex-col shrink-0 bg-transparent min-h-0">
            {/* Title row, then filters on the next line (design: quiet section headers). */}
            <div className="shrink-0 border-b border-[color:var(--archive-ink-line-soft)]">
              <div className="h-10 px-4 flex items-center">
                <span className="font-serif text-[10px] uppercase tracking-[0.15em] opacity-80">Projects</span>
                <span className="ml-auto font-mono text-[8px] tracking-widest opacity-45">
                  {filteredProjects.length}
                </span>
              </div>
              {sourceIds.length > 1 ? (
                <div className="px-4 pb-2.5 flex items-center gap-1.5 flex-wrap">
                  <Chip active={sourceFilter === null} onClick={() => setSourceFilter(null)}>
                    all
                  </Chip>
                  {sourceIds.map((id) => (
                    <Chip key={id} active={sourceFilter === id} onClick={() => setSourceFilter(id)} title={id}>
                      {sourceLabel(id).slice(0, 6)}
                    </Chip>
                  ))}
                </div>
              ) : null}
            </div>

            <div className="flex-1 overflow-y-auto scrollbar-hide py-2 space-y-px">
              {filteredProjects.length === 0 ? (
                <EmptyState
                  title="No projects"
                  detail={sourceFilter ? `No ${sourceLabel(sourceFilter)} projects.` : 'Index is empty.'}
                />
              ) : (
                filteredProjects.map((p) => {
                  const key = projectKey(p);
                  const isSelected = selectedKey === key;
                  const prompt = flattenPrompt(projectPrompts[key], 72);
                  const tok = formatTokenUsage(p.tokenUsage, p.sourceId, p.tokensEstimated);
                  return (
                    <button
                      key={key}
                      type="button"
                      onClick={() => {
                        setSelected({ slug: p.slug, sourceId: p.sourceId });
                        setSelectedSession(null);
                      }}
                      className={`w-full text-left px-4 py-3 cursor-pointer transition-colors border-0 border-l-2 ${
                        isSelected
                          ? 'bg-ink/[0.05] border-l-ink'
                          : 'bg-transparent border-l-transparent hover:bg-ink/[0.05]'
                      }`}
                    >
                      <div className="mb-1.5 flex items-start justify-between gap-2">
                        <span className="min-w-0 truncate font-mono text-[10px] tracking-tight text-ink">
                          {p.folderName}
                        </span>
                        <SourceBadge sourceId={p.sourceId} isDark={isDark} />
                      </div>
                      <p className="mb-2 line-clamp-2 font-serif text-[11px] leading-[1.35] opacity-65">
                        {prompt || '\u00A0'}
                      </p>
                      <div className="flex items-center justify-between gap-2 font-mono text-[8px] uppercase tracking-[0.08em] opacity-60">
                        <span className="truncate">
                          {formatNumber(p.sessionCount)} sess · {formatNumber(p.messageCount)} msg · {tok} tok
                        </span>
                        <span className="shrink-0">{formatRelativeTime(p.lastActiveAt)}</span>
                      </div>
                    </button>
                  );
                })
              )}
            </div>
          </aside>
        ) : null}

        {/* MAIN */}
        <main className="flex-1 flex flex-col min-w-0 bg-transparent relative min-h-0">
          {selected && selectedSession ? (
            <SessionMessagesView
              projectSlug={selected.slug}
              sourceId={selected.sourceId}
              session={selectedSession.session}
              sessionIndex={selectedSession.index}
              hasMemory={selectedProject?.hasMemory}
              isDark={isDark}
              leftOpen={leftOpen}
              onToggleLeft={() => setLeftOpen(true)}
              filesOpen={filesOpen}
              onToggleFiles={() => setFilesOpen(true)}
              onBack={() => setSelectedSession(null)}
            />
          ) : (
            <>
              <div className="h-10 border-b border-[color:var(--archive-ink-line)] flex items-center px-6 justify-between shrink-0">
                <div className="flex items-center gap-3 font-serif text-[10px] tracking-[0.15em]">
                  {!leftOpen && (
                    <button
                      type="button"
                      onClick={() => setLeftOpen(true)}
                      className="opacity-50 hover:opacity-100 bg-transparent border-0 text-ink cursor-pointer p-0"
                      title="Show projects"
                    >
                      <PanelLeft size={14} />
                    </button>
                  )}
                  <span className="opacity-80">{selected ? `Sessions · ${sessions.length}` : 'Select a project'}</span>
                  {selected ? <SourceBadge sourceId={selected.sourceId} isDark={isDark} /> : null}
                </div>
              </div>
              <div className="flex-1 overflow-y-auto scrollbar-hide py-1 space-y-px">
                {!selected && (
                  <EmptyState
                    title="Select a project"
                    detail={`Browse sessions, open a transcript, or press ${modKey}K to search.`}
                    action={
                      <Btn onClick={() => setSearchOpen(true)}>
                        Search <Kbd>{modKey}K</Kbd>
                      </Btn>
                    }
                  />
                )}
                {selected && sessions.length === 0 && (
                  <EmptyState title="No sessions" detail="This project has no indexed sessions yet." />
                )}
                {sessions.map((s, index) => {
                  const prompt = flattenPrompt(s.firstPrompt || s.summary, 96);
                  const tok = formatTokenUsage(s.tokenUsage, s.sourceId, s.tokensEstimated);
                  return (
                    <button
                      key={`${s.sourceId}:${s.sessionId}`}
                      type="button"
                      onClick={() => setSelectedSession({ session: s, index })}
                      className="block w-full text-left px-6 py-3.5 border-0 bg-transparent text-inherit cursor-pointer hover:bg-ink/[0.05] transition-colors"
                    >
                      <div className="flex items-center gap-2.5 mb-1.5">
                        <span className="font-mono text-[10px] tracking-[0.1em] opacity-70">#{index + 1}</span>
                        {s.gitBranch ? (
                          <span className="font-mono text-[10px] text-sanguine/90">{s.gitBranch}</span>
                        ) : (
                          <span className="font-mono text-[10px] opacity-30">no branch</span>
                        )}
                        <span className="flex-1" />
                        <span className="font-mono text-[9px] opacity-40">{s.sessionId.slice(0, 8)}</span>
                        <SourceBadge sourceId={s.sourceId} isDark={isDark} />
                      </div>
                      {/* Session prompt quote — design thought scale: 13px serif italic */}
                      <div className="font-serif text-[13px] italic leading-relaxed opacity-70 mb-1.5 truncate">
                        {prompt ? `"${prompt}"` : '(no prompt)'}
                      </div>
                      <div className="font-mono text-[8px] uppercase tracking-[0.08em] opacity-55">
                        {formatNumber(s.messageCount)} msgs
                        <Dot />
                        {tok} tokens
                        <Dot />
                        {formatDuration(s.lifespanMs)}
                        <Dot />
                        {formatRelativeTime(s.lastUpdate)}
                        {s.todoCount > 0 ? (
                          <>
                            <Dot />
                            {s.todoCount} todos
                          </>
                        ) : null}
                        {s.planSlug ? (
                          <>
                            <Dot />
                            plan
                          </>
                        ) : null}
                      </div>
                    </button>
                  );
                })}
              </div>
            </>
          )}
        </main>

        {/* RIGHT: Structure / Files */}
        <FileExplorerPanel
          open={filesOpen}
          onClose={() => setFilesOpen(false)}
          projectPath={selectedProject?.absolutePath ?? null}
          projectLabel={selectedProject?.folderName ?? null}
          isDark={isDark}
          sessionArtifacts={
            selected && selectedSession
              ? {
                  projectSlug: selected.slug,
                  sourceId: selected.sourceId,
                  sessionId: selectedSession.session.sessionId,
                  hints: {
                    todoCount: selectedSession.session.todoCount,
                    planSlug: selectedSession.session.planSlug,
                    hasTask: selectedSession.session.hasTask,
                    hasMemory: selectedProject?.hasMemory,
                  },
                }
              : null
          }
        />
      </div>

      <SearchOverlay
        open={searchOpen}
        onClose={() => setSearchOpen(false)}
        sourceIds={sourceIds}
        scopeProject={scopeProject}
        onNavigate={onSearchNavigate}
        isDark={isDark}
      />
    </div>
  );
}

import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { SpaghettiProvider, type SpaghettiProviderProps } from '@vibecook/spaghetti-sdk/react';
import type { ProjectListItem, SessionListItem, StoreStats } from '@vibecook/spaghetti-sdk';
import { createIpcApi } from './ipc-api.js';
import { LoadingScreen } from './components/LoadingScreen.js';
import { SourceBadge } from './components/SourceBadge.js';
import { SessionMessagesView } from './components/SessionMessagesView.js';
import { SearchOverlay, type SearchNavigateTarget } from './components/SearchOverlay.js';
import { FileExplorerPanel } from './components/FileExplorerPanel.js';
import { Btn, Chip, Dot, EmptyState, Kbd, SectionLabel } from './components/ui.js';
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

/**
 * Electron playground shell.
 *
 * Never import runtime values from `@vibecook/spaghetti-sdk` (main entry) in
 * the renderer — that package pulls Node natives and will blank the window.
 */

/** Project identity is (sourceId, slug) after multi-source schema v6. */
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
      <div className="p-6 text-[#f2f2f2] font-mono text-xs bg-[#050505] h-full">
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
  /** Rightmost mille file explorer panel. */
  const [filesOpen, setFilesOpen] = useState(false);
  /** Open this session after sessions list loads (search navigation). */
  const pendingSessionId = useRef<string | null>(null);

  const [sources, setSources] = useState<SourceProgressState[]>(() => initialSourceStates());
  const [progress, setProgress] = useState<ProgressSnapshot | null>(null);
  const [elapsedMs, setElapsedMs] = useState(0);
  const [loadHeadline, setLoadHeadline] = useState('Indexing agent history');
  const [retrying, setRetrying] = useState(false);
  const loadStartedAt = useRef(Date.now());

  useEffect(() => {
    if (ready && !rebuilding && !retrying) return;
    const id = window.setInterval(() => {
      setElapsedMs(Date.now() - loadStartedAt.current);
    }, 250);
    return () => window.clearInterval(id);
  }, [ready, rebuilding, retrying]);

  // Global shortcuts: ⌘K search, ⌘B files panel
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

  // TUI project cards show the latest session's firstPrompt — hydrate lazily.
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
      />
    );
  }

  return (
    <div className="flex flex-col h-full bg-[#0a0a0a] text-[#f2f2f2]">
      <header className="px-4 py-2.5 border-b border-white/10 bg-white/[0.015] flex items-center gap-3 flex-wrap shrink-0">
        <strong className="text-xs font-medium tracking-[0.16em] lowercase opacity-85">spaghetti</strong>
        {engine && (
          <span
            className="text-[9px] px-1.5 py-0.5 rounded border border-white/12 text-white/50 font-mono tracking-wide uppercase"
            title={engine === 'rs' ? 'Native Rust ingest engine' : 'TypeScript ingest engine'}
          >
            {engine === 'rs' ? 'native' : 'typescript'}
          </span>
        )}
        {sourceIds.length > 0 && (
          <span className="inline-flex items-center gap-1.5">
            {sourceIds.map((id) => (
              <SourceBadge key={id} sourceId={id} />
            ))}
          </span>
        )}
        {stats && (
          <span className="text-[10px] text-white/30 font-mono hidden sm:inline">
            {formatNumber(stats.totalSegments)} segs
            <Dot />
            {formatBytes(stats.dbSizeBytes)}
            <Dot />
            {formatNumber(stats.searchIndexed)} indexed
          </span>
        )}
        <span className="flex-1" />
        <Btn onClick={() => setSearchOpen(true)} title={`Search (${modKey}K)`}>
          Search
          <Kbd>{modKey}K</Kbd>
        </Btn>
        <Btn
          variant={filesOpen ? 'solid' : 'ghost'}
          onClick={() => setFilesOpen((v) => !v)}
          title={`Project files (${modKey}B)`}
        >
          Files
          <Kbd>{modKey}B</Kbd>
        </Btn>
        <Btn
          onClick={() => void onRebuild()}
          disabled={rebuilding}
          title="Force a full cold rebuild of the SQLite index"
        >
          Rebuild
        </Btn>
      </header>

      <main className="flex flex-1 min-h-0">
        {/* Projects */}
        <section className="w-[360px] shrink-0 border-r border-white/10 flex flex-col min-h-0">
          <SectionLabel
            trailing={
              sourceIds.length > 1 ? (
                <span className="flex items-center gap-1 normal-case tracking-normal">
                  <Chip active={sourceFilter === null} onClick={() => setSourceFilter(null)}>
                    all
                  </Chip>
                  {sourceIds.map((id) => (
                    <Chip key={id} active={sourceFilter === id} onClick={() => setSourceFilter(id)} title={id}>
                      {sourceLabel(id)}
                    </Chip>
                  ))}
                </span>
              ) : null
            }
          >
            Projects · {filteredProjects.length}
            {sourceFilter ? ` / ${projects.length}` : ''}
          </SectionLabel>

          <div className="flex-1 overflow-y-auto">
            {filteredProjects.length === 0 ? (
              <EmptyState
                title="No projects"
                detail={sourceFilter ? `No ${sourceLabel(sourceFilter)} projects in the index.` : 'Index is empty.'}
              />
            ) : (
              filteredProjects.map((p) => {
                const key = projectKey(p);
                const isSelected = selectedKey === key;
                const prompt = flattenPrompt(projectPrompts[key], 64);
                const tok = formatTokenUsage(p.tokenUsage, p.sourceId, p.tokensEstimated);
                return (
                  <button
                    key={key}
                    type="button"
                    onClick={() => {
                      setSelected({ slug: p.slug, sourceId: p.sourceId });
                      setSelectedSession(null);
                    }}
                    className={`block w-full text-left px-3.5 py-3 cursor-pointer text-xs transition-colors border-0 border-b border-solid border-b-white/[0.04] ${
                      isSelected
                        ? 'bg-white/[0.06] border-l-2 border-l-solid border-l-white/55'
                        : 'bg-transparent border-l-2 border-l-solid border-l-transparent hover:bg-white/[0.03]'
                    }`}
                  >
                    <div className="flex items-center gap-2 mb-1">
                      <span
                        className={`flex-1 min-w-0 truncate tracking-tight ${isSelected ? 'font-medium' : 'font-normal'}`}
                      >
                        {p.folderName}
                      </span>
                      {p.latestGitBranch ? (
                        <span
                          className={`text-[10px] font-mono shrink-0 max-w-[100px] truncate ${
                            isSelected ? 'text-sky-200/75' : 'text-white/35'
                          }`}
                          title={p.latestGitBranch}
                        >
                          {p.latestGitBranch}
                        </span>
                      ) : null}
                      <SourceBadge sourceId={p.sourceId} />
                    </div>
                    <div className="text-[11px] italic text-white/38 mb-1 truncate min-h-[15px]">
                      {prompt ? `"${prompt}"` : '\u00A0'}
                    </div>
                    <div className="text-[10px] text-white/38 tabular-nums">
                      {formatNumber(p.sessionCount)} sessions
                      <Dot />
                      {formatNumber(p.messageCount)} msgs
                      <Dot />
                      {tok} tokens
                      <Dot />
                      {formatRelativeTime(p.lastActiveAt)}
                      {p.hasMemory ? (
                        <>
                          <Dot />
                          memory
                        </>
                      ) : null}
                    </div>
                  </button>
                );
              })
            )}
          </div>
        </section>

        {/* Sessions list or message view */}
        <section className="flex-1 min-w-0 flex flex-col min-h-0">
          {selected && selectedSession ? (
            <SessionMessagesView
              projectSlug={selected.slug}
              sourceId={selected.sourceId}
              session={selectedSession.session}
              sessionIndex={selectedSession.index}
              hasMemory={selectedProject?.hasMemory}
              onBack={() => setSelectedSession(null)}
            />
          ) : (
            <>
              <SectionLabel trailing={selected ? <SourceBadge sourceId={selected.sourceId} /> : null}>
                {selected ? `Sessions · ${sessions.length}` : 'Select a project'}
              </SectionLabel>
              <div className="flex-1 overflow-y-auto">
                {!selected && (
                  <EmptyState
                    title="Select a project"
                    detail={`Browse sessions, open a transcript, or press ${modKey}K to search.`}
                    action={
                      <Btn onClick={() => setSearchOpen(true)}>
                        Open search <Kbd>{modKey}K</Kbd>
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
                      className="block w-full text-left px-4 py-3 border-0 border-b border-solid border-b-white/[0.04] bg-transparent text-inherit cursor-pointer text-xs hover:bg-white/[0.03] transition-colors"
                    >
                      <div className="flex items-center gap-2.5 mb-1">
                        <span className="font-medium text-white/75 tabular-nums min-w-[28px]">#{index + 1}</span>
                        {s.gitBranch ? (
                          <span className="text-[11px] text-amber-200/75 font-mono">{s.gitBranch}</span>
                        ) : (
                          <span className="text-[11px] text-white/25">no branch</span>
                        )}
                        <span className="flex-1" />
                        <span className="font-mono text-[10px] opacity-40">{s.sessionId.slice(0, 8)}</span>
                        <SourceBadge sourceId={s.sourceId} />
                      </div>
                      <div className="text-xs italic text-white/45 mb-1 truncate">
                        {prompt ? `"${prompt}"` : '(no prompt)'}
                      </div>
                      <div className="text-[10px] text-white/38 tabular-nums">
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
                        {s.hasTask ? (
                          <>
                            <Dot />
                            task
                          </>
                        ) : null}
                        {s.isSidechain ? (
                          <>
                            <Dot />
                            sidechain
                          </>
                        ) : null}
                      </div>
                    </button>
                  );
                })}
              </div>
            </>
          )}
        </section>

        {/* Rightmost: mille file explorer for selected project folder */}
        <FileExplorerPanel
          open={filesOpen}
          onClose={() => setFilesOpen(false)}
          projectPath={selectedProject?.absolutePath ?? null}
          projectLabel={selectedProject?.folderName ?? null}
        />
      </main>

      <SearchOverlay
        open={searchOpen}
        onClose={() => setSearchOpen(false)}
        sourceIds={sourceIds}
        scopeProject={scopeProject}
        onNavigate={onSearchNavigate}
      />
    </div>
  );
}

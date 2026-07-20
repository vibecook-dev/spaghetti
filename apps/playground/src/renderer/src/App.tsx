import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from 'react';
import { Library, Moon, PanelLeft, Search, Settings, Sun } from 'lucide-react';
import { TrafficLights } from './components/TrafficLights.js';
import { SpaghettiProvider, type SpaghettiProviderProps } from '@vibecook/spaghetti-sdk/react';
import type { ProjectListItem, SegmentChangeBatch, SessionListItem, StoreStats } from '@vibecook/spaghetti-sdk';
import { createIpcApi } from './ipc-api.js';
import { LoadingScreen } from './components/LoadingScreen.js';
import { SourceBadge, SourceBadges } from './components/SourceBadge.js';
import { SessionMessagesView } from './components/SessionMessagesView.js';
import { TokenActivityGraph } from './components/TokenActivityGraph.js';
import { SearchOverlay, type SearchNavigateTarget } from './components/SearchOverlay.js';
import { FileExplorerPanel } from './components/FileExplorerPanel.js';
import { SettingsDialog } from './components/SettingsDialog.js';
import { Btn, Chip, Dot, EmptyState, Kbd, LiveDot } from './components/ui.js';
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

const DEBUG_MODE = (import.meta as ImportMeta & { env: { DEV: boolean } }).env.DEV;
type DebugSessionModule = typeof import('./dev/debug-session.js');

/**
 * Electron playground shell — archive / paper design (spaghetti-ui-design).
 *
 * Never import runtime values from `@vibecook/spaghetti-sdk` (main entry) in
 * the renderer — that package pulls Node natives and will blank the window.
 */

interface ProjectKey {
  projectId: string;
}

interface LiveSessionState {
  sourceId: string;
  projectSlug: string;
  sessionId: string;
  revision: number;
  lastActivityAt: number;
  unreadCount: number;
}

/** A session stays live until a full minute passes without another update. */
const SESSION_LIVE_TTL_MS = 60_000;

function liveSessionKey(sourceId: string, projectSlug: string, sessionId: string): string {
  return JSON.stringify([sourceId, projectSlug, sessionId]);
}

function liveProjectMemberKey(sourceId: string, projectSlug: string): string {
  return JSON.stringify([sourceId, projectSlug]);
}

function projectKey(p: ProjectKey): string {
  return p.projectId;
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
  const [debugSession, setDebugSession] = useState<DebugSessionModule | null>(null);
  const [projects, setProjects] = useState<ProjectListItem[]>([]);
  const [selected, setSelected] = useState<ProjectKey | null>(null);
  const [sessions, setSessions] = useState<SessionListItem[]>([]);
  const [sessionListProjectKey, setSessionListProjectKey] = useState<string | null>(null);
  const [selectedSession, setSelectedSession] = useState<{
    session: SessionListItem;
    index: number;
  } | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [projectChangeNonce, setProjectChangeNonce] = useState(0);
  const [sessionChangeNonce, setSessionChangeNonce] = useState(0);
  const [stats, setStats] = useState<StoreStats | null>(null);
  const [sessionSourceFilter, setSessionSourceFilter] = useState<string | null>(null);
  const [searchOpen, setSearchOpen] = useState(false);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [filesOpen, setFilesOpen] = useState(true);
  const [leftOpen, setLeftOpen] = useState(true);
  // The reference opens on warm paper; dark parchment is the alternate illumination.
  const [isDark, setIsDark] = useState(false);
  const pendingSessionId = useRef<{ sessionId: string; sourceId?: string } | null>(null);
  const projectChangeTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const sessionChangeTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const [branchNavigateTarget, setBranchNavigateTarget] = useState<SearchNavigateTarget | null>(null);
  const [liveSessions, setLiveSessions] = useState<Record<string, LiveSessionState>>({});
  const [liveClock, setLiveClock] = useState(Date.now());
  const selectedSessionRef = useRef<typeof selectedSession>(null);
  const selectedProjectMembersRef = useRef<Set<string>>(new Set());
  const projectItemRefs = useRef(new Map<string, HTMLButtonElement>());
  const projectItemPositionsRef = useRef(new Map<string, number>());
  const projectItemAnimationsRef = useRef(new Map<string, Animation>());

  const [sources, setSources] = useState<SourceProgressState[]>(() => initialSourceStates());
  const [progress, setProgress] = useState<ProgressSnapshot | null>(null);
  const [elapsedMs, setElapsedMs] = useState(0);
  const [loadHeadline, setLoadHeadline] = useState('Indexing agent history');
  const [retrying, setRetrying] = useState(false);
  const loadStartedAt = useRef(Date.now());

  // Keep the large synthetic gallery out of production bundles entirely.
  useEffect(() => {
    if (!DEBUG_MODE) return;
    let cancelled = false;
    void import('./dev/debug-session.js').then((gallery) => {
      if (cancelled) return;
      setDebugSession(gallery);
      setProjects((current) => [
        gallery.DEBUG_PROJECT,
        ...current.filter((project) => projectKey(project) !== projectKey(gallery.DEBUG_PROJECT)),
      ]);
      setSelected({ ...gallery.DEBUG_PROJECT_KEY });
      setSessions([gallery.DEBUG_SESSION]);
      setSelectedSession({ session: gallery.DEBUG_SESSION, index: 0 });
    });
    return () => {
      cancelled = true;
    };
  }, []);

  // Persist theme class on <html>
  useEffect(() => {
    document.documentElement.classList.toggle('dark', isDark);
  }, [isDark]);

  useEffect(() => {
    selectedSessionRef.current = selectedSession;
    if (!selectedSession) return;
    const { sourceId, projectSlug, sessionId } = selectedSession.session;
    const key = liveSessionKey(sourceId, projectSlug, sessionId);
    setLiveSessions((current) => {
      const state = current[key];
      return state?.unreadCount ? { ...current, [key]: { ...state, unreadCount: 0 } } : current;
    });
  }, [selectedSession]);

  useLayoutEffect(() => {
    if (!leftOpen) {
      projectItemPositionsRef.current.clear();
      for (const animation of projectItemAnimationsRef.current.values()) animation.cancel();
      projectItemAnimationsRef.current.clear();
      return;
    }

    const nextPositions = new Map<string, number>();
    const reduceMotion = window.matchMedia('(prefers-reduced-motion: reduce)').matches;

    for (const [key, element] of projectItemRefs.current) {
      projectItemAnimationsRef.current.get(key)?.cancel();
      projectItemAnimationsRef.current.delete(key);
      const nextTop = element.offsetTop;
      nextPositions.set(key, nextTop);
      const previousTop = projectItemPositionsRef.current.get(key);
      const deltaY = previousTop == null ? 0 : previousTop - nextTop;
      if (reduceMotion || Math.abs(deltaY) < 1) continue;

      const animation = element.animate([{ transform: `translateY(${deltaY}px)` }, { transform: 'translateY(0)' }], {
        duration: 420,
        easing: 'cubic-bezier(0.22, 1, 0.36, 1)',
      });
      projectItemAnimationsRef.current.set(key, animation);
      animation.addEventListener(
        'finish',
        () => {
          if (projectItemAnimationsRef.current.get(key) === animation) {
            projectItemAnimationsRef.current.delete(key);
          }
        },
        { once: true },
      );
    }

    projectItemPositionsRef.current = nextPositions;
  }, [leftOpen, projects]);

  useEffect(
    () => () => {
      for (const animation of projectItemAnimationsRef.current.values()) animation.cancel();
    },
    [],
  );

  useEffect(() => {
    if (!ready) return;
    const timer = window.setInterval(() => setLiveClock(Date.now()), 1_000);
    return () => window.clearInterval(timer);
  }, [ready]);

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
        msg.includes('corrupted') ||
        msg.includes('sdk utility exited')
      ) {
        setError(null);
        setReady(false);
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

    const unsubChange = bridge.onChange((batch: SegmentChangeBatch) => {
      const selectedNow = selectedSessionRef.current?.session;
      const receivedAt = Date.now();
      setLiveSessions((current) => {
        let next = current;
        for (const change of batch.changes ?? []) {
          if (!change.sourceId || !change.projectSlug || !change.sessionId) continue;
          const key = liveSessionKey(change.sourceId, change.projectSlug, change.sessionId);
          const previous = current[key] ?? {
            sourceId: change.sourceId,
            projectSlug: change.projectSlug,
            sessionId: change.sessionId,
            revision: 0,
            lastActivityAt: 0,
            unreadCount: 0,
          };
          const isSelected =
            selectedNow?.sourceId === change.sourceId &&
            selectedNow.projectSlug === change.projectSlug &&
            selectedNow.sessionId === change.sessionId;
          const added = change.type === 'message' && !isSelected ? 1 : 0;
          if (next === current) next = { ...current };
          next[key] = {
            ...previous,
            revision: Math.max(previous.revision, change.revision ?? batch.timestamp),
            lastActivityAt: receivedAt,
            unreadCount: isSelected ? 0 : previous.unreadCount + added,
          };
        }
        return next;
      });
      setLiveClock(receivedAt);
      // Session-list metadata is cheap and useful while browsing one project.
      // Whole-library counts are much broader, so refresh those on a slower
      // lane. The visible transcript has its own exact, session-scoped lane.
      const touchesSelectedProject = (batch.changes ?? []).some(
        (change) =>
          !!change.sourceId &&
          !!change.projectSlug &&
          selectedProjectMembersRef.current.has(liveProjectMemberKey(change.sourceId, change.projectSlug)),
      );
      if (touchesSelectedProject && !sessionChangeTimerRef.current) {
        sessionChangeTimerRef.current = setTimeout(() => {
          sessionChangeTimerRef.current = null;
          setSessionChangeNonce((n) => n + 1);
        }, 1000);
      }
      if (!projectChangeTimerRef.current) {
        projectChangeTimerRef.current = setTimeout(() => {
          projectChangeTimerRef.current = null;
          setProjectChangeNonce((n) => n + 1);
        }, 5000);
      }
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
      if (sessionChangeTimerRef.current) clearTimeout(sessionChangeTimerRef.current);
      if (projectChangeTimerRef.current) clearTimeout(projectChangeTimerRef.current);
    };
  }, []);

  useEffect(() => {
    if (!ready) return;
    window.spaghetti
      .getProjectList()
      .then((list) =>
        setProjects(
          debugSession
            ? [
                debugSession.DEBUG_PROJECT,
                ...list.filter((project) => projectKey(project) !== projectKey(debugSession.DEBUG_PROJECT)),
              ]
            : list,
        ),
      )
      .catch((e: unknown) => setError(String(e)));
    window.spaghetti
      .getStats()
      .then(setStats)
      .catch(() => setStats(null));
  }, [ready, projectChangeNonce, debugSession]);

  useEffect(() => {
    if (!selected) {
      setSessions([]);
      setSessionListProjectKey(null);
      return;
    }
    const project = projects.find((candidate) => projectKey(candidate) === projectKey(selected));
    if (!project) {
      setSessions([]);
      setSessionListProjectKey(null);
      setSelectedSession(null);
      return;
    }
    const requestedProjectKey = projectKey(project);
    if (debugSession && projectKey(project) === projectKey(debugSession.DEBUG_PROJECT)) {
      setSessions([debugSession.DEBUG_SESSION]);
      setSessionListProjectKey(requestedProjectKey);
      return;
    }
    let cancelled = false;
    void window.spaghetti
      .getSessionList(project)
      .then((list) => {
        if (cancelled) return;
        setSessions(list);
        setSessionListProjectKey(requestedProjectKey);
        const want = pendingSessionId.current;
        if (want) {
          pendingSessionId.current = null;
          const idx = list.findIndex(
            (s) => s.sessionId === want.sessionId && (!want.sourceId || s.sourceId === want.sourceId),
          );
          if (idx >= 0) {
            setSelectedSession({ session: list[idx], index: idx });
          }
          return;
        }
        setSelectedSession((current) => {
          if (!current) return null;
          const idx = list.findIndex(
            (session) =>
              session.sessionId === current.session.sessionId && session.sourceId === current.session.sourceId,
          );
          return idx >= 0 ? { session: list[idx], index: idx } : current;
        });
      })
      .catch((e: unknown) => {
        if (!cancelled) setError(String(e));
      });
    return () => {
      cancelled = true;
    };
  }, [selected, projects, sessionChangeNonce, debugSession]);

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
      setProjectChangeNonce((n) => n + 1);
      setSessionChangeNonce((n) => n + 1);
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
        setProjectChangeNonce((n) => n + 1);
        setSessionChangeNonce((n) => n + 1);
      }
    } catch (e: unknown) {
      setError(String(e));
      setRetrying(false);
    }
  };

  const onSearchNavigate = useCallback(
    (target: SearchNavigateTarget) => {
      const project = projects.find((candidate) =>
        candidate.members.some((member) => member.sourceId === target.sourceId && member.slug === target.projectSlug),
      );
      if (!project) {
        setError(`Project for ${target.sourceId}:${target.projectSlug} is no longer indexed.`);
        return;
      }
      setSelected({ projectId: project.projectId });
      setSelectedSession(null);
      setSessionSourceFilter(null);
      setBranchNavigateTarget(target.agentId ? target : null);
      pendingSessionId.current = target.sessionId ? { sessionId: target.sessionId, sourceId: target.sourceId } : null;
    },
    [projects],
  );

  const sourceIds = useMemo(() => [...new Set(projects.flatMap((p) => p.sourceIds))].sort(), [projects]);

  const liveProjectMembers = useMemo(
    () =>
      new Set(
        Object.values(liveSessions)
          .filter((activity) => liveClock - activity.lastActivityAt < SESSION_LIVE_TTL_MS)
          .map((activity) => liveProjectMemberKey(activity.sourceId, activity.projectSlug)),
      ),
    [liveClock, liveSessions],
  );

  const filteredSessions = useMemo(
    () => (sessionSourceFilter ? sessions.filter((session) => session.sourceId === sessionSourceFilter) : sessions),
    [sessions, sessionSourceFilter],
  );

  const selectedKey = selected ? projectKey(selected) : null;
  const sessionPanelLoading = selectedKey !== null && sessionListProjectKey !== selectedKey;
  const selectedProject = selected ? projects.find((p) => projectKey(p) === selectedKey) : null;
  const selectedActivity = selectedSession
    ? liveSessions[
        liveSessionKey(
          selectedSession.session.sourceId,
          selectedSession.session.projectSlug,
          selectedSession.session.sessionId,
        )
      ]
    : undefined;
  const selectedSessionIsLive = !!selectedActivity && liveClock - selectedActivity.lastActivityAt < SESSION_LIVE_TTL_MS;

  const scopeProject = useMemo(
    () =>
      selectedProject
        ? {
            projectId: selectedProject.projectId,
            members: selectedProject.members,
            folderName: selectedProject.folderName,
          }
        : null,
    [selectedProject],
  );

  useEffect(() => {
    selectedProjectMembersRef.current = new Set(
      selectedProject?.members.map((member) => liveProjectMemberKey(member.sourceId, member.slug)) ?? [],
    );
  }, [selectedProject]);

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

  const sessionHeading = (
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
      <span className="opacity-80">{selected ? `Sessions · ${filteredSessions.length}` : 'Select a project'}</span>
    </div>
  );

  const sessionSourceControls =
    selectedProject && selectedProject.sourceIds.length > 1 ? (
      <div className="flex flex-wrap items-center justify-end gap-1.5">
        <Chip active={sessionSourceFilter === null} onClick={() => setSessionSourceFilter(null)}>
          all
        </Chip>
        {selectedProject.sourceIds.map((id) => (
          <Chip
            key={id}
            active={sessionSourceFilter === id}
            onClick={() => setSessionSourceFilter(id)}
            title={sourceLabel(id)}
          >
            <SourceBadge sourceId={id} isDark={isDark} />
          </Chip>
        ))}
      </div>
    ) : null;

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
          <button
            type="button"
            onClick={() => setSearchOpen(true)}
            className="flex cursor-pointer items-center gap-2 border-0 bg-transparent px-2 py-0.5 font-mono text-[9px] uppercase tracking-widest text-ink transition-colors hover:bg-ink hover:text-paper"
            title={`Search (${modKey}K)`}
          >
            <Search size={10} /> Search
          </button>
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
              onClick={() => setSettingsOpen(true)}
              className="inline-flex cursor-pointer items-center justify-center border-0 bg-transparent p-1 text-ink opacity-50 transition-opacity hover:opacity-100"
              title="Open settings"
              aria-label="Open settings"
            >
              <Settings size={12} />
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
                <span className="ml-auto font-mono text-[8px] tracking-widest opacity-45">{projects.length}</span>
              </div>
            </div>

            <div className="flex-1 overflow-y-auto scrollbar-hide py-2 space-y-px">
              {projects.length === 0 ? (
                <EmptyState title="No projects" detail="Index is empty." />
              ) : (
                projects.map((p) => {
                  const key = projectKey(p);
                  const isSelected = selectedKey === key;
                  const isLive = p.members.some((member) =>
                    liveProjectMembers.has(liveProjectMemberKey(member.sourceId, member.slug)),
                  );
                  const prompt = flattenPrompt(p.latestPrompt, 72);
                  const tok = formatTokenUsage(p.tokenUsage, undefined, p.tokensEstimated);
                  return (
                    <button
                      key={key}
                      ref={(element) => {
                        if (element) projectItemRefs.current.set(key, element);
                        else projectItemRefs.current.delete(key);
                      }}
                      type="button"
                      onClick={() => {
                        setSelected({ projectId: p.projectId });
                        setSelectedSession(null);
                        setSessionSourceFilter(null);
                      }}
                      className={`w-full text-left px-4 py-3 cursor-pointer transition-colors border-0 border-l-2 font-normal text-inherit ${
                        isSelected
                          ? 'bg-ink/[0.05] border-l-ink'
                          : 'bg-transparent border-l-transparent hover:bg-ink/[0.05]'
                      }`}
                    >
                      <div className="mb-1.5 flex items-start justify-between gap-2">
                        <div className="flex min-w-0 flex-1 items-center gap-2">
                          <span className="min-w-0 truncate font-mono text-[10px] tracking-tight text-ink">
                            {p.folderName}
                          </span>
                          {isLive ? <LiveDot active /> : null}
                        </div>
                        <SourceBadges sourceIds={p.sourceIds} isDark={isDark} />
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
        <main className="flex-1 flex flex-col min-w-0 bg-transparent relative min-h-0" aria-busy={sessionPanelLoading}>
          {selected && selectedSession ? (
            <SessionMessagesView
              projectSlug={selectedSession.session.projectSlug}
              sourceId={selectedSession.session.sourceId}
              session={selectedSession.session}
              sessionIndex={selectedSession.index}
              isLive={selectedSessionIsLive}
              hasMemory={selectedProject?.hasMemory}
              isDark={isDark}
              leftOpen={leftOpen}
              onToggleLeft={() => setLeftOpen(true)}
              filesOpen={filesOpen}
              onToggleFiles={() => setFilesOpen(true)}
              onBack={() => {
                setBranchNavigateTarget(null);
                setSelectedSession(null);
              }}
              initialBranchTarget={
                branchNavigateTarget?.sessionId === selectedSession.session.sessionId &&
                branchNavigateTarget.sourceId === selectedSession.session.sourceId &&
                branchNavigateTarget.agentId
                  ? {
                      agentId: branchNavigateTarget.agentId,
                      workflowId: branchNavigateTarget.workflowId,
                      spawnToolId: branchNavigateTarget.spawnToolId,
                      agentTimelineIndex: branchNavigateTarget.agentTimelineIndex,
                    }
                  : undefined
              }
              onInitialBranchTargetConsumed={() => setBranchNavigateTarget(null)}
              debugMessages={
                debugSession && selectedSession.session.sessionId === debugSession.DEBUG_SESSION.sessionId
                  ? debugSession.DEBUG_SESSION_MESSAGES
                  : undefined
              }
            />
          ) : (
            <>
              {scopeProject ? (
                <TokenActivityGraph
                  project={scopeProject}
                  sourceId={sessionSourceFilter}
                  activityRevision={sessionChangeNonce}
                  header={sessionHeading}
                  controls={sessionSourceControls}
                />
              ) : (
                <div className="flex min-h-10 shrink-0 items-center justify-between gap-4 border-b border-[color:var(--archive-ink-line)] px-6 py-2">
                  {sessionHeading}
                </div>
              )}
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
                {selected && filteredSessions.length === 0 && (
                  <EmptyState
                    title="No sessions"
                    detail={
                      sessionSourceFilter
                        ? `No ${sourceLabel(sessionSourceFilter)} sessions in this project.`
                        : 'This project has no indexed sessions yet.'
                    }
                  />
                )}
                {filteredSessions.map((s) => {
                  const index = sessions.findIndex(
                    (session) => session.sourceId === s.sourceId && session.sessionId === s.sessionId,
                  );
                  const prompt = flattenPrompt(s.firstPrompt || s.summary, 96);
                  const tok = formatTokenUsage(s.tokenUsage, s.sourceId, s.tokensEstimated);
                  const activityKey = liveSessionKey(s.sourceId, s.projectSlug, s.sessionId);
                  const activity = liveSessions[activityKey];
                  const isLive = !!activity && liveClock - activity.lastActivityAt < SESSION_LIVE_TTL_MS;
                  return (
                    <button
                      key={`${s.sourceId}:${s.sessionId}`}
                      type="button"
                      onClick={() => {
                        setBranchNavigateTarget(null);
                        if (activity?.unreadCount) {
                          setLiveSessions((current) => ({
                            ...current,
                            [activityKey]: { ...current[activityKey]!, unreadCount: 0 },
                          }));
                        }
                        setSelectedSession({ session: s, index });
                      }}
                      className="block w-full text-left px-6 py-3.5 border-0 bg-transparent text-inherit cursor-pointer hover:bg-ink/[0.05] transition-colors"
                    >
                      <div className="flex items-center gap-2.5 mb-1.5">
                        <span className="font-mono text-[10px] tracking-[0.1em] opacity-70">#{index + 1}</span>
                        {isLive ? <LiveDot active /> : null}
                        {s.gitBranch ? (
                          <span className="font-mono text-[10px] text-sanguine/90">{s.gitBranch}</span>
                        ) : (
                          <span className="font-mono text-[10px] opacity-30">no branch</span>
                        )}
                        <span className="flex-1" />
                        <span className="font-mono text-[9px] opacity-40">{s.sessionId.slice(0, 8)}</span>
                        <SourceBadge sourceId={s.sourceId} isDark={isDark} />
                        {activity?.unreadCount ? (
                          <span className="font-mono text-[8px] tabular-nums text-sanguine">
                            +{activity.unreadCount}
                          </span>
                        ) : null}
                      </div>
                      {/* Session prompt quote — design thought scale: 13px serif */}
                      <div className="mb-1.5 truncate font-serif text-[13px] leading-relaxed opacity-70">
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
              <div
                className={`session-panel-loading absolute inset-0 z-10 flex items-center justify-center bg-paper/30 backdrop-blur-[2px] transition-opacity duration-300 ease-out ${
                  sessionPanelLoading ? 'session-panel-loading--active opacity-100' : 'pointer-events-none opacity-0'
                }`}
                aria-hidden={!sessionPanelLoading}
              >
                <div className="w-52 max-w-[70%] text-center" role="status" aria-live="polite">
                  <div className="session-panel-loading__track" aria-hidden="true" />
                  <div className="mt-3 font-mono text-[8px] uppercase tracking-[0.16em] opacity-65">
                    Loading sessions
                  </div>
                  {selectedProject ? (
                    <div className="mt-1 truncate font-serif text-[10px] opacity-45">{selectedProject.folderName}</div>
                  ) : null}
                </div>
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
                  projectSlug: selectedSession.session.projectSlug,
                  sourceId: selectedSession.session.sourceId,
                  memoryProject: selectedProject ?? undefined,
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
      <SettingsDialog
        open={settingsOpen}
        onOpenChange={setSettingsOpen}
        onRebuild={() => void onRebuild()}
        rebuilding={rebuilding}
        engine={engine}
        stats={stats}
      />
    </div>
  );
}

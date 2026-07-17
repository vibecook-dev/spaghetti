import { useEffect, useRef, useState } from 'react';
import { SpaghettiProvider, type SpaghettiProviderProps } from '@vibecook/spaghetti-sdk/react';
import type { ProjectListItem, SessionListItem } from '@vibecook/spaghetti-sdk';
import { createIpcApi } from './ipc-api.js';
import { LoadingScreen } from './components/LoadingScreen.js';
import { SourceBadge } from './components/SourceBadge.js';
import { SessionMessagesView } from './components/SessionMessagesView.js';
import { flattenPrompt, formatDuration, formatNumber, formatRelativeTime, formatTokenUsage } from './lib/format.js';
import {
  applyProgressEvent,
  initialSourceStates,
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
      <div
        style={{
          padding: 24,
          color: '#f2f2f2',
          fontFamily: 'ui-monospace, monospace',
          fontSize: 12,
          background: '#050505',
          height: '100%',
        }}
      >
        <div
          style={{
            fontWeight: 500,
            marginBottom: 8,
            letterSpacing: '0.12em',
            textTransform: 'uppercase',
            fontSize: 10,
            opacity: 0.5,
          }}
        >
          Preload bridge missing
        </div>
        <div style={{ opacity: 0.8 }}>{String(err)}</div>
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
  /** Open session for message view (null = session list). */
  const [selectedSession, setSelectedSession] = useState<{
    session: SessionListItem;
    index: number;
  } | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [changeNonce, setChangeNonce] = useState(0);
  /** Latest session firstPrompt per project key — mirrors TUI project card line 2. */
  const [projectPrompts, setProjectPrompts] = useState<Record<string, string>>({});

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

  useEffect(() => {
    const bridge = window.spaghetti;

    const unsubProgress = bridge.onProgress((p) => {
      const snap: ProgressSnapshot = {
        phase: p.phase,
        message: p.message,
        current: p.current,
        total: p.total,
      };
      // SDK self-recovery (or manual retry) — clear error UI and reset source rows.
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
  }, [ready, changeNonce]);

  // TUI project cards show the latest session's firstPrompt — hydrate lazily.
  useEffect(() => {
    if (!ready || projects.length === 0) return;
    let cancelled = false;
    const run = async () => {
      const next: Record<string, string> = {};
      // Limit concurrency so IPC doesn't flood the main process.
      const queue = [...projects];
      const workers = Array.from({ length: Math.min(6, queue.length) }, async () => {
        while (queue.length > 0 && !cancelled) {
          const p = queue.shift()!;
          const key = projectKey(p);
          try {
            const sess = await window.spaghetti.getSessionList(p.slug, { sourceId: p.sourceId });
            // Sessions are sorted by last update desc in the SDK.
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
      .then(setSessions)
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

  /** Wipe corrupt cache + full re-init (from error screen). */
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
      // ready event may have already fired; poll as safety net
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

  const sourceIds = [...new Set(projects.map((p) => p.sourceId))].sort();
  const selectedKey = selected ? projectKey(selected) : null;

  return (
    <div style={{ display: 'flex', flexDirection: 'column', height: '100%', background: '#0a0a0a' }}>
      <header
        style={{
          padding: '10px 18px',
          borderBottom: '1px solid rgba(255,255,255,0.08)',
          background: 'rgba(255,255,255,0.015)',
          display: 'flex',
          alignItems: 'center',
          gap: 12,
          flexWrap: 'wrap',
        }}
      >
        <strong
          style={{
            fontSize: 12,
            fontWeight: 500,
            letterSpacing: '0.16em',
            textTransform: 'lowercase',
            opacity: 0.85,
          }}
        >
          spaghetti
        </strong>
        {engine && (
          <span
            style={{
              fontSize: 9,
              padding: '2px 7px',
              borderRadius: 2,
              border: '1px solid rgba(255,255,255,0.12)',
              color: 'rgba(255,255,255,0.5)',
              fontFamily: 'ui-monospace, Menlo, monospace',
              letterSpacing: 0.8,
              textTransform: 'uppercase',
            }}
            title={engine === 'rs' ? 'Native Rust ingest engine' : 'TypeScript ingest engine'}
          >
            {engine === 'rs' ? 'native' : 'typescript'}
          </span>
        )}
        {sourceIds.length > 0 && (
          <span
            style={{
              display: 'inline-flex',
              alignItems: 'center',
              gap: 5,
              fontSize: 10,
              color: 'rgba(255,255,255,0.4)',
            }}
          >
            {sourceIds.map((id) => (
              <SourceBadge key={id} sourceId={id} />
            ))}
          </span>
        )}
        <span style={{ flex: 1 }} />
        <button
          type="button"
          onClick={() => void onRebuild()}
          disabled={rebuilding}
          style={{
            fontSize: 11,
            padding: '4px 12px',
            borderRadius: 2,
            border: '1px solid rgba(255,255,255,0.14)',
            background: 'transparent',
            color: 'rgba(255,255,255,0.75)',
            cursor: 'pointer',
            letterSpacing: 0.3,
          }}
          title="Force a full cold rebuild of the SQLite index"
        >
          Rebuild index
        </button>
        {error && (
          <span style={{ fontSize: 11, color: 'rgba(255,180,180,0.9)' }} title={error}>
            error: {error.slice(0, 80)}
          </span>
        )}
      </header>

      <main style={{ flex: 1, display: 'flex', minHeight: 0 }}>
        {/* Projects — TUI ProjectCard parity */}
        <section
          style={{
            width: 380,
            borderRight: '1px solid rgba(255,255,255,0.08)',
            overflowY: 'auto',
          }}
        >
          <div
            style={{
              padding: '8px 14px',
              fontSize: 10,
              letterSpacing: '0.12em',
              textTransform: 'uppercase',
              color: 'rgba(255,255,255,0.35)',
              borderBottom: '1px solid rgba(255,255,255,0.05)',
            }}
          >
            Projects · {projects.length}
          </div>
          {projects.map((p) => {
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
                style={{
                  display: 'block',
                  width: '100%',
                  textAlign: 'left',
                  padding: '12px 14px',
                  background: isSelected ? 'rgba(255,255,255,0.06)' : 'transparent',
                  color: 'inherit',
                  border: 'none',
                  borderLeft: isSelected ? '2px solid rgba(255,255,255,0.55)' : '2px solid transparent',
                  borderBottom: '1px solid rgba(255,255,255,0.04)',
                  cursor: 'pointer',
                  fontSize: 12,
                }}
              >
                {/* Line 1: name · branch · badge */}
                <div
                  style={{
                    display: 'flex',
                    alignItems: 'center',
                    gap: 8,
                    marginBottom: 4,
                  }}
                >
                  <span
                    style={{
                      fontWeight: isSelected ? 560 : 450,
                      flex: 1,
                      minWidth: 0,
                      overflow: 'hidden',
                      textOverflow: 'ellipsis',
                      whiteSpace: 'nowrap',
                      letterSpacing: '-0.01em',
                    }}
                  >
                    {p.folderName}
                  </span>
                  {p.latestGitBranch ? (
                    <span
                      style={{
                        fontSize: 10,
                        color: isSelected ? 'rgba(200,230,255,0.75)' : 'rgba(255,255,255,0.35)',
                        fontFamily: 'ui-monospace, Menlo, monospace',
                        flexShrink: 0,
                        maxWidth: 100,
                        overflow: 'hidden',
                        textOverflow: 'ellipsis',
                      }}
                      title={p.latestGitBranch}
                    >
                      {p.latestGitBranch}
                    </span>
                  ) : null}
                  <SourceBadge sourceId={p.sourceId} />
                </div>
                {/* Line 2: first prompt (TUI italic quote) */}
                <div
                  style={{
                    fontSize: 11,
                    fontStyle: 'italic',
                    color: 'rgba(255,255,255,0.38)',
                    marginBottom: 5,
                    overflow: 'hidden',
                    textOverflow: 'ellipsis',
                    whiteSpace: 'nowrap',
                    minHeight: 15,
                  }}
                >
                  {prompt ? `"${prompt}"` : '\u00A0'}
                </div>
                {/* Line 3: sessions · msgs · tokens · relative time */}
                <div
                  style={{
                    fontSize: 10,
                    color: 'rgba(255,255,255,0.38)',
                    fontVariantNumeric: 'tabular-nums',
                    letterSpacing: 0.1,
                  }}
                >
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
          })}
        </section>

        {/* Sessions list or message view */}
        <section style={{ flex: 1, minWidth: 0, display: 'flex', flexDirection: 'column', minHeight: 0 }}>
          {selected && selectedSession ? (
            <SessionMessagesView
              projectSlug={selected.slug}
              sourceId={selected.sourceId}
              session={selectedSession.session}
              sessionIndex={selectedSession.index}
              onBack={() => setSelectedSession(null)}
            />
          ) : (
            <>
              <div
                style={{
                  padding: '8px 14px',
                  fontSize: 10,
                  letterSpacing: '0.12em',
                  textTransform: 'uppercase',
                  color: 'rgba(255,255,255,0.35)',
                  borderBottom: '1px solid rgba(255,255,255,0.05)',
                  display: 'flex',
                  alignItems: 'center',
                  gap: 8,
                }}
              >
                {selected ? (
                  <>
                    <span>Sessions · {sessions.length}</span>
                    <SourceBadge sourceId={selected.sourceId} />
                  </>
                ) : (
                  'Select a project'
                )}
              </div>
              <div style={{ flex: 1, overflowY: 'auto' }}>
                {!selected && (
                  <div style={{ padding: 24, color: 'rgba(255,255,255,0.3)', fontSize: 12 }}>
                    Select a project to browse sessions.
                  </div>
                )}
                {selected && sessions.length === 0 && (
                  <div style={{ padding: 24, color: 'rgba(255,255,255,0.3)', fontSize: 12 }}>No sessions found.</div>
                )}
                {sessions.map((s, index) => {
                  const prompt = flattenPrompt(s.firstPrompt || s.summary, 96);
                  const tok = formatTokenUsage(s.tokenUsage, s.sourceId, s.tokensEstimated);
                  return (
                    <button
                      key={`${s.sourceId}:${s.sessionId}`}
                      type="button"
                      onClick={() => setSelectedSession({ session: s, index })}
                      style={{
                        display: 'block',
                        width: '100%',
                        textAlign: 'left',
                        padding: '12px 16px',
                        borderBottom: '1px solid rgba(255,255,255,0.04)',
                        fontSize: 12,
                        background: 'transparent',
                        color: 'inherit',
                        border: 'none',
                        borderBottomWidth: 1,
                        borderBottomStyle: 'solid',
                        borderBottomColor: 'rgba(255,255,255,0.04)',
                        cursor: 'pointer',
                      }}
                    >
                      <div
                        style={{
                          display: 'flex',
                          alignItems: 'center',
                          gap: 10,
                          marginBottom: 4,
                        }}
                      >
                        <span
                          style={{
                            fontWeight: 500,
                            color: 'rgba(255,255,255,0.75)',
                            fontVariantNumeric: 'tabular-nums',
                            minWidth: 28,
                          }}
                        >
                          #{index + 1}
                        </span>
                        {s.gitBranch ? (
                          <span
                            style={{
                              fontSize: 11,
                              color: 'rgba(255, 210, 120, 0.75)',
                              fontFamily: 'ui-monospace, Menlo, monospace',
                            }}
                          >
                            {s.gitBranch}
                          </span>
                        ) : (
                          <span style={{ fontSize: 11, color: 'rgba(255,255,255,0.25)' }}>no branch</span>
                        )}
                        <span style={{ flex: 1 }} />
                        <span
                          style={{
                            fontFamily: 'ui-monospace, Menlo, monospace',
                            fontSize: 10,
                            opacity: 0.4,
                          }}
                        >
                          {s.sessionId.slice(0, 8)}
                        </span>
                        <SourceBadge sourceId={s.sourceId} />
                      </div>
                      <div
                        style={{
                          fontSize: 12,
                          fontStyle: 'italic',
                          color: 'rgba(255,255,255,0.45)',
                          marginBottom: 5,
                          overflow: 'hidden',
                          textOverflow: 'ellipsis',
                          whiteSpace: 'nowrap',
                        }}
                      >
                        {prompt ? `"${prompt}"` : '(no prompt)'}
                      </div>
                      <div
                        style={{
                          fontSize: 10,
                          color: 'rgba(255,255,255,0.38)',
                          fontVariantNumeric: 'tabular-nums',
                        }}
                      >
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
      </main>
    </div>
  );
}

function Dot() {
  return <span style={{ opacity: 0.35 }}> · </span>;
}

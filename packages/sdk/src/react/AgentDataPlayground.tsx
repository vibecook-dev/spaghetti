import React, { useEffect, useState, useCallback, useRef } from 'react';
import type {
  ProjectListItem,
  SessionListItem,
  MessagePage,
  SubagentListItem,
  StoreStats,
  SearchResultSet,
  InitProgress,
} from '../index.js';
import { useSpaghettiAPI } from './context.js';
import { ProjectCard } from './components/ProjectCard.js';
import { SessionCard } from './components/SessionCard.js';
import { DetailOverlay } from './components/DetailOverlay.js';
import { MessageEntry, buildMessageContext, isToolResultOnlyMessage } from './components/MessageEntry.js';
import { formatBytes } from './utils/formatters.js';

type AnyMsg = Record<string, any>;

export function AgentDataPlayground() {
  const api = useSpaghettiAPI();

  const [ready, setReady] = useState(false);
  const [initProgress, setInitProgress] = useState<string>('Waiting for init...');
  const [initPhase, setInitPhase] = useState<string>('');
  const [initCurrent, setInitCurrent] = useState(0);
  const [initTotal, setInitTotal] = useState(0);
  const [initDurationMs, setInitDurationMs] = useState(0);

  const [projects, setProjects] = useState<ProjectListItem[]>([]);
  const [selectedProjectId, setSelectedProjectId] = useState<string | null>(null);
  const [sessions, setSessions] = useState<SessionListItem[]>([]);
  const [selectedSessionId, setSelectedSessionId] = useState<string | null>(null);
  const [messagePage, setMessagePage] = useState<MessagePage | null>(null);
  const [allMessages, setAllMessages] = useState<AnyMsg[]>([]);
  const [stats, setStats] = useState<StoreStats | null>(null);

  const [searchText, setSearchText] = useState('');
  const [searchResults, setSearchResults] = useState<SearchResultSet | null>(null);

  const [loadingProjects, setLoadingProjects] = useState(false);
  const [loadingSessions, setLoadingSessions] = useState(false);
  const [loadingMessages, setLoadingMessages] = useState(false);

  const offsetRef = useRef(0);
  const [pendingChanges, setPendingChanges] = useState(0);

  const [detailOverlay, setDetailOverlay] = useState<{
    type: 'memory' | 'todos' | 'plan' | 'task';
    title: string;
    content: string | null;
    todos?: unknown[];
    plan?: unknown;
    task?: unknown;
  } | null>(null);

  const [expandedToolResults, setExpandedToolResults] = useState<Record<string, string>>({});

  const [subagents, setSubagents] = useState<SubagentListItem[]>([]);
  const [expandedSubagentId, setExpandedSubagentId] = useState<string | null>(null);
  const [subagentMessages, setSubagentMessages] = useState<AnyMsg[]>([]);
  const [loadingSubagent, setLoadingSubagent] = useState(false);
  const [subagentHasMore, setSubagentHasMore] = useState(false);
  const subagentOffsetRef = useRef(0);

  const fetchProjectsAndStats = useCallback(() => {
    setLoadingProjects(true);
    try {
      const projectList = api.getProjectList();
      const storeStats = api.getStats();
      setProjects(projectList);
      setStats(storeStats);
    } catch (err) {
      console.error('Failed to fetch projects/stats', err);
    } finally {
      setLoadingProjects(false);
    }
  }, [api]);

  useEffect(() => {
    const unsubs: Array<() => void> = [];

    unsubs.push(
      api.onProgress((progress: InitProgress) => {
        setInitPhase(progress.phase);
        setInitProgress(progress.message);
        if (progress.current != null) setInitCurrent(progress.current);
        if (progress.total != null && progress.total > 0) setInitTotal(progress.total);
      }),
    );

    unsubs.push(
      api.onReady((info: any) => {
        setReady(true);
        setInitDurationMs(info.durationMs);
        setInitProgress(`Ready in ${info.durationMs}ms`);
        setInitPhase('ready');
      }),
    );

    unsubs.push(
      api.onChange(() => {
        setPendingChanges((c) => c + 1);
      }),
    );

    if (api.isReady()) {
      setReady(true);
      setInitProgress('Ready (was already initialized)');
      fetchProjectsAndStats();
    }

    return () => {
      unsubs.forEach((u) => u());
    };
  }, [api, fetchProjectsAndStats]);

  useEffect(() => {
    if (ready) fetchProjectsAndStats();
  }, [ready, fetchProjectsAndStats]);

  const handleSelectProject = useCallback(
    (project: ProjectListItem) => {
      setSelectedProjectId(project.projectId);
      setSelectedSessionId(null);
      setMessagePage(null);
      setAllMessages([]);
      setLoadingSessions(true);

      try {
        const list = api.getSessionList(project);
        setSessions(list);
      } catch (err) {
        console.error('Failed to fetch sessions', err);
      } finally {
        setLoadingSessions(false);
      }
    },
    [api],
  );

  const handleSelectSession = useCallback(
    (sessionId: string) => {
      const session = sessions.find((candidate) => candidate.sessionId === sessionId);
      if (!session) return;
      setSelectedSessionId(sessionId);
      setLoadingMessages(true);
      setAllMessages([]);
      setMessagePage(null);
      offsetRef.current = 0;

      try {
        const probe = api.getSessionMessages(session.projectSlug, sessionId, 1, 0, {
          sourceId: session.sourceId,
        });
        const total = probe.total;
        const startOffset = Math.max(0, total - 30);
        const page = api.getSessionMessages(session.projectSlug, sessionId, 30, startOffset, {
          sourceId: session.sourceId,
        });
        setMessagePage({ ...page, hasMore: startOffset > 0 });
        setAllMessages(page.messages as AnyMsg[]);
        offsetRef.current = startOffset;
      } catch (err) {
        console.error('Failed to fetch messages', err);
      } finally {
        setLoadingMessages(false);
      }
    },
    [api, sessions],
  );

  const handleLoadMore = useCallback(() => {
    if (!selectedSessionId) return;
    const session = sessions.find((candidate) => candidate.sessionId === selectedSessionId);
    if (!session || offsetRef.current <= 0) return;
    setLoadingMessages(true);

    try {
      const newOffset = Math.max(0, offsetRef.current - 30);
      const limit = offsetRef.current - newOffset;
      const page = api.getSessionMessages(session.projectSlug, selectedSessionId, limit, newOffset, {
        sourceId: session.sourceId,
      });
      setMessagePage({ ...page, hasMore: newOffset > 0 });
      setAllMessages((prev) => [...(page.messages as AnyMsg[]), ...prev]);
      offsetRef.current = newOffset;
    } catch (err) {
      console.error('Failed to load more messages', err);
    } finally {
      setLoadingMessages(false);
    }
  }, [api, sessions, selectedSessionId]);

  const handleSearch = useCallback(() => {
    if (!searchText.trim()) return;
    try {
      const results = api.search({ text: searchText.trim(), limit: 20 });
      setSearchResults(results);
    } catch (err) {
      console.error('Failed to search', err);
    }
  }, [api, searchText]);

  const handleViewMemory = useCallback(
    (project: ProjectListItem) => {
      try {
        const content = api.getProjectMemory(project);
        setDetailOverlay({ type: 'memory', title: `Project Memory - ${project.folderName}`, content });
      } catch (err) {
        console.error('Failed to fetch memory', err);
      }
    },
    [api],
  );

  const handleExpandToolResult = useCallback(
    (toolUseId: string) => {
      if (!selectedSessionId) return;
      const session = sessions.find((candidate) => candidate.sessionId === selectedSessionId);
      if (!session) return;
      if (expandedToolResults[toolUseId]) {
        setExpandedToolResults((prev) => {
          const next = { ...prev };
          delete next[toolUseId];
          return next;
        });
        return;
      }
      try {
        const result = api.getToolResult(session.projectSlug, selectedSessionId, toolUseId);
        if (result) setExpandedToolResults((prev) => ({ ...prev, [toolUseId]: result }));
      } catch (err) {
        console.error('Failed to fetch tool result', err);
      }
    },
    [api, sessions, selectedSessionId, expandedToolResults],
  );

  useEffect(() => {
    const session = sessions.find((candidate) => candidate.sessionId === selectedSessionId);
    if (!session || !selectedSessionId) {
      setSubagents([]);
      return;
    }
    try {
      const list = api.getSessionSubagents(session.projectSlug, selectedSessionId);
      setSubagents(list);
    } catch {
      setSubagents([]);
    }
  }, [api, sessions, selectedSessionId]);

  const handleExpandSubagent = useCallback(
    (agentId: string) => {
      const session = sessions.find((candidate) => candidate.sessionId === selectedSessionId);
      if (!session || !selectedSessionId) return;
      if (expandedSubagentId === agentId) {
        setExpandedSubagentId(null);
        setSubagentMessages([]);
        return;
      }
      setExpandedSubagentId(agentId);
      setLoadingSubagent(true);
      setSubagentMessages([]);
      subagentOffsetRef.current = 0;
      try {
        const page = api.getSubagentMessages(session.projectSlug, selectedSessionId, agentId, 30, 0);
        setSubagentMessages(page.messages as AnyMsg[]);
        setSubagentHasMore(page.hasMore);
        subagentOffsetRef.current = page.messages.length;
      } catch (err) {
        console.error('Failed to fetch subagent messages', err);
      } finally {
        setLoadingSubagent(false);
      }
    },
    [api, sessions, selectedSessionId, expandedSubagentId],
  );

  const handleLoadMoreSubagent = useCallback(() => {
    const session = sessions.find((candidate) => candidate.sessionId === selectedSessionId);
    if (!session || !selectedSessionId || !expandedSubagentId) return;
    setLoadingSubagent(true);
    try {
      const page = api.getSubagentMessages(
        session.projectSlug,
        selectedSessionId,
        expandedSubagentId,
        30,
        subagentOffsetRef.current,
      );
      setSubagentMessages((prev) => [...prev, ...(page.messages as AnyMsg[])]);
      setSubagentHasMore(page.hasMore);
      subagentOffsetRef.current += page.messages.length;
    } catch (err) {
      console.error('Failed to load more subagent messages', err);
    } finally {
      setLoadingSubagent(false);
    }
  }, [api, sessions, selectedSessionId, expandedSubagentId]);

  const selectedProject = projects.find((p) => p.projectId === selectedProjectId) ?? null;
  const progressPct = initTotal > 0 ? Math.min(100, Math.round((initCurrent / initTotal) * 100)) : 0;

  if (!ready) {
    return (
      <div className="flex flex-col h-full text-white">
        <div className="flex-1 flex items-center justify-center">
          <div className="w-96 space-y-4">
            <h1 className="text-sm font-bold text-white/90 text-center">Spaghetti - Agent Data Playground</h1>
            <p className="text-xs text-white/50 text-center">Initializing agent data service...</p>
            {initPhase && (
              <div className="flex justify-center">
                <span className="text-[10px] bg-yellow-500/20 text-yellow-300 px-2 py-0.5 rounded">{initPhase}</span>
              </div>
            )}
            <div className="w-full bg-white/10 rounded-full h-2 overflow-hidden">
              <div
                className="bg-blue-500 h-full rounded-full transition-all duration-300"
                style={{ width: initTotal > 0 ? `${progressPct}%` : '0%' }}
              />
            </div>
            {initTotal > 0 && (
              <p className="text-xs text-white/60 font-mono text-center">
                {initCurrent} / {initTotal} ({progressPct}%)
              </p>
            )}
            <p className="text-[11px] text-white/40 truncate text-center">{initProgress}</p>
          </div>
        </div>
      </div>
    );
  }

  return (
    <div className="flex flex-col h-full text-white relative">
      <div className="px-4 py-1.5 border-b border-white/10 bg-white/5 flex items-center justify-between">
        <div className="flex items-center gap-4">
          <h1 className="text-xs font-bold text-white/90">Spaghetti - Agent Data</h1>
          <span className="text-[10px] px-1.5 py-0.5 rounded bg-green-500/20 text-green-300">
            Ready{initDurationMs > 0 ? ` in ${(initDurationMs / 1000).toFixed(1)}s` : ''}
          </span>
        </div>
        <div className="flex items-center gap-3 text-[10px] text-white/40">
          {stats && (
            <>
              <span>{stats.totalSegments} segments</span>
              <span>{formatBytes(stats.dbSizeBytes)} db</span>
              <span>{stats.searchIndexed} indexed</span>
            </>
          )}
          <button
            onClick={() => {
              setPendingChanges(0);
              fetchProjectsAndStats();
            }}
            disabled={loadingProjects}
            className="text-white/60 bg-white/5 px-2 py-0.5 rounded border border-white/10 hover:bg-white/10 cursor-pointer disabled:opacity-50"
          >
            Refresh{pendingChanges > 0 ? ` (${pendingChanges})` : ''}
          </button>
        </div>
      </div>

      <div className="px-4 py-1.5 border-b border-white/10 bg-white/[0.02] flex items-center gap-2">
        <input
          type="text"
          value={searchText}
          onChange={(e) => setSearchText(e.target.value)}
          onKeyDown={(e) => e.key === 'Enter' && handleSearch()}
          placeholder="Search segments..."
          className="flex-1 bg-white/5 text-xs text-white/80 px-2 py-1 rounded border border-white/10 outline-none focus:border-white/20"
        />
        <button
          onClick={handleSearch}
          className="text-xs text-white/60 bg-white/5 px-2 py-1 rounded border border-white/10 hover:bg-white/10 cursor-pointer"
        >
          Search
        </button>
        {searchResults && (
          <>
            <span className="text-[10px] text-white/40">
              {searchResults.total} results{searchResults.hasMore ? '+' : ''}
            </span>
            <button
              onClick={() => setSearchResults(null)}
              className="text-[10px] text-white/40 hover:text-white/60 cursor-pointer"
            >
              Clear
            </button>
          </>
        )}
      </div>

      {searchResults && searchResults.results.length > 0 && (
        <div className="px-4 py-2 border-b border-white/10 bg-white/[0.03] max-h-48 overflow-y-auto">
          {searchResults.results.map((r: any, i: number) => (
            <div key={i} className="flex items-center gap-2 text-[11px] py-0.5">
              <span className="text-purple-300 w-16 shrink-0 font-mono">{r.type}</span>
              {r.projectSlug && <span className="text-blue-300/60">{r.projectSlug}</span>}
              <span className="text-white/50 truncate flex-1">{r.snippet}</span>
            </div>
          ))}
        </div>
      )}

      <div className="flex flex-1 min-h-0">
        <div className="w-1/4 border-r border-white/10 flex flex-col min-w-0">
          <div className="px-3 py-2 border-b border-white/10">
            <h2 className="text-xs font-semibold text-white/80">
              Projects{!loadingProjects && ` (${projects.length})`}
            </h2>
          </div>
          <div className="flex-1 overflow-y-auto">
            {loadingProjects ? (
              <div className="p-3 text-xs text-white/40">Loading...</div>
            ) : projects.length === 0 ? (
              <div className="p-3 text-xs text-white/40">No projects found</div>
            ) : (
              projects.map((p) => (
                <ProjectCard
                  key={p.projectId}
                  project={p}
                  isSelected={selectedProjectId === p.projectId}
                  onClick={() => handleSelectProject(p)}
                  onMemoryClick={() => handleViewMemory(p)}
                />
              ))
            )}
          </div>
        </div>

        <div className="w-1/4 border-r border-white/10 flex flex-col min-w-0">
          <div className="px-3 py-2 border-b border-white/10">
            <h2 className="text-xs font-semibold text-white/80 truncate">
              {selectedProject
                ? `${selectedProject.folderName}${!loadingSessions ? ` (${sessions.length})` : ''}`
                : 'Select a project'}
            </h2>
          </div>
          <div className="flex-1 overflow-y-auto">
            {!selectedProjectId ? (
              <div className="p-3 text-xs text-white/40">Click a project</div>
            ) : loadingSessions ? (
              <div className="p-3 text-xs text-white/40">Loading...</div>
            ) : (
              sessions.map((s) => (
                <SessionCard
                  key={s.sessionId}
                  session={s}
                  isSelected={selectedSessionId === s.sessionId}
                  onClick={() => handleSelectSession(s.sessionId)}
                />
              ))
            )}
          </div>
        </div>

        <div className="w-1/2 flex flex-col min-w-0">
          <div className="px-3 py-2 border-b border-white/10">
            <h2 className="text-xs font-semibold text-white/80 truncate">
              {selectedSessionId
                ? `Messages ${selectedSessionId.slice(0, 8)}${messagePage ? ` (${allMessages.length}/${messagePage.total})` : ''}`
                : 'Select a session'}
            </h2>
          </div>
          <div className="flex-1 overflow-y-auto">
            {!selectedSessionId ? (
              <div className="p-3 text-xs text-white/40">Click a session to view messages</div>
            ) : loadingMessages && allMessages.length === 0 ? (
              <div className="p-3 text-xs text-white/40">Loading messages...</div>
            ) : allMessages.length === 0 ? (
              <div className="p-3 text-xs text-white/40">No messages</div>
            ) : (
              <>
                {messagePage?.hasMore && (
                  <button
                    onClick={handleLoadMore}
                    disabled={loadingMessages}
                    className="w-full py-2 text-xs text-white/50 hover:text-white/80 hover:bg-white/5 border-b border-white/5 cursor-pointer disabled:opacity-50"
                  >
                    {loadingMessages ? 'Loading...' : `Load Earlier (${allMessages.length}/${messagePage.total})`}
                  </button>
                )}
                {(() => {
                  const ctx = buildMessageContext(allMessages, subagents);
                  return allMessages
                    .filter((m) => !isToolResultOnlyMessage(m))
                    .map((m, i) => (
                      <MessageEntry
                        key={i}
                        msg={m}
                        ctx={ctx}
                        expandedToolResults={expandedToolResults}
                        onExpandToolResult={handleExpandToolResult}
                        expandedSubagentId={expandedSubagentId}
                        subagentMessages={subagentMessages}
                        loadingSubagent={loadingSubagent}
                        subagentHasMore={subagentHasMore}
                        onExpandSubagent={handleExpandSubagent}
                        onLoadMoreSubagent={handleLoadMoreSubagent}
                      />
                    ));
                })()}
              </>
            )}
          </div>
        </div>
      </div>

      {detailOverlay && (
        <DetailOverlay title={detailOverlay.title} onClose={() => setDetailOverlay(null)}>
          {detailOverlay.type === 'memory' &&
            (detailOverlay.content ? (
              <pre className="text-xs text-white/70 whitespace-pre-wrap font-mono">{detailOverlay.content}</pre>
            ) : (
              <p className="text-xs text-white/40">No memory content</p>
            ))}
        </DetailOverlay>
      )}
    </div>
  );
}

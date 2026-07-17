/**
 * Session message browser — timeline chat with:
 * - reverse pagination (scroll up for older)
 * - live tail appends via onChange
 * - artifact drawer (plan / todos / task / subagents / memory)
 * - type/tool solo·mute filters + in-transcript text filter (ProjectPage parity)
 */

import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from 'react';
import {
  TimelineMessageRenderer,
  TimeGroupSeparator,
  shouldShowTimestamp,
  isTimelineType,
  transformRawMessagesToTimeline,
  type ChatSessionMessage,
} from '@vibecook/spaghetti-sdk/react';
import type { MessagePage, SegmentChangeBatch, SessionListItem } from '@vibecook/spaghetti-sdk';
import { SourceBadge } from './SourceBadge.js';
import { ArtifactPanel, type ArtifactTab } from './ArtifactPanel.js';
import { MessageFilterBar, useMessageFilters, countTimelineMessages, filterTimelineMessages } from './filters/index.js';
import { Btn, Dot, LiveDot, Spinner } from './ui.js';
import { flattenPrompt, formatDuration, formatNumber, formatRelativeTime, formatTokenUsage } from '../lib/format.js';

type AnyMsg = Record<string, unknown>;

const PAGE_SIZE = 30;
const LOADING_DELAY_MS = 280;
const NEAR_TOP_PX = 80;
const NEAR_BOTTOM_PX = 140;
const LIVE_DEBOUNCE_MS = 180;

export interface SessionMessagesViewProps {
  projectSlug: string;
  sourceId: string;
  session: SessionListItem;
  sessionIndex: number;
  /** Project has MEMORY.md (from ProjectListItem). */
  hasMemory?: boolean;
  onBack: () => void;
}

export function SessionMessagesView({
  projectSlug,
  sourceId,
  session,
  sessionIndex,
  hasMemory = false,
  onBack,
}: SessionMessagesViewProps) {
  const [rawMessages, setRawMessages] = useState<AnyMsg[]>([]);
  const [pageMeta, setPageMeta] = useState<Pick<MessagePage, 'total' | 'hasMore'> | null>(null);
  const [loading, setLoading] = useState(true);
  const [loadingMore, setLoadingMore] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [livePulse, setLivePulse] = useState(false);
  const [pendingNew, setPendingNew] = useState(0);
  const [artifactsOpen, setArtifactsOpen] = useState(false);
  const [artifactTab, setArtifactTab] = useState<ArtifactTab>('plan');

  const offsetRef = useRef(0);
  const scrollContainerRef = useRef<HTMLDivElement>(null);
  const prevScrollHeightRef = useRef(0);
  const isPrependingRef = useRef(false);
  const shouldScrollToBottomRef = useRef(false);
  const loadMoreInFlightRef = useRef(false);
  /** True when the viewport is near the conversation tail. */
  const nearBottomRef = useRef(true);
  const totalRef = useRef(0);
  const liveTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const pulseTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  // Full timeline (pre-filter) — filters apply after transform
  const timeline: ChatSessionMessage[] = useMemo(() => transformRawMessagesToTimeline(rawMessages), [rawMessages]);

  const { messageCounts, toolCounts } = useMemo(() => countTimelineMessages(timeline), [timeline]);
  const toolNames = useMemo(() => Object.keys(toolCounts), [toolCounts]);

  const {
    typeFilters,
    toolFilters,
    searchQuery,
    visibleTypes,
    visibleTools,
    anySoloActive,
    toggleTypeSolo,
    toggleTypeMute,
    toggleToolSolo,
    toggleToolMute,
    clearAllSolos,
    setSearchQuery,
    resetFilters,
  } = useMessageFilters({ toolNames });

  const chatMessages = useMemo(
    () =>
      filterTimelineMessages({
        messages: timeline,
        visibleTypes,
        visibleTools,
        typeFilters,
        toolFilters,
        anySoloActive,
        searchQuery,
      }),
    [timeline, visibleTypes, visibleTools, typeFilters, toolFilters, anySoloActive, searchQuery],
  );

  const filterTotalCount = useMemo(() => {
    const types = Object.values(messageCounts).reduce((a, b) => a + b, 0);
    const tools = Object.values(toolCounts).reduce((a, b) => a + b, 0);
    return types + tools;
  }, [messageCounts, toolCounts]);

  // Reset when session changes
  useEffect(() => {
    isPrependingRef.current = false;
    shouldScrollToBottomRef.current = false;
    prevScrollHeightRef.current = 0;
    loadMoreInFlightRef.current = false;
    nearBottomRef.current = true;
    offsetRef.current = 0;
    totalRef.current = 0;
    setPendingNew(0);
    setArtifactsOpen(false);
    setError(null);
    resetFilters();
  }, [session.sessionId, resetFilters]);

  // ── Initial load: last PAGE_SIZE messages ─────────────────────────────
  useEffect(() => {
    let cancelled = false;
    setLoading(true);
    setError(null);
    setRawMessages([]);
    setPageMeta(null);
    offsetRef.current = 0;
    const scope = { sourceId };

    void (async () => {
      try {
        await delay(LOADING_DELAY_MS);
        if (cancelled) return;

        const probe = await window.spaghetti.getSessionMessages(projectSlug, session.sessionId, 1, 0, scope);
        if (cancelled) return;
        const total = probe.total;
        const startOffset = Math.max(0, total - PAGE_SIZE);
        const page = await window.spaghetti.getSessionMessages(
          projectSlug,
          session.sessionId,
          PAGE_SIZE,
          startOffset,
          scope,
        );
        if (cancelled) return;

        shouldScrollToBottomRef.current = true;
        setRawMessages(page.messages as unknown as AnyMsg[]);
        setPageMeta({ total: page.total, hasMore: startOffset > 0 });
        offsetRef.current = startOffset;
        totalRef.current = page.total;
      } catch (e: unknown) {
        if (!cancelled) setError(String(e));
      } finally {
        if (!cancelled) setLoading(false);
      }
    })();

    return () => {
      cancelled = true;
    };
  }, [projectSlug, session.sessionId, sourceId]);

  // ── Live tail: append new messages when the index reports changes ─────
  const appendLiveTail = useCallback(async () => {
    const scope = { sourceId };
    try {
      const probe = await window.spaghetti.getSessionMessages(projectSlug, session.sessionId, 1, 0, scope);
      const newTotal = probe.total;
      const oldTotal = totalRef.current;

      if (newTotal <= oldTotal) {
        // Content may have been rewritten in place — soft-refresh the visible tail.
        if (newTotal === 0 || oldTotal === 0) return;
        return;
      }

      const delta = newTotal - oldTotal;
      const page = await window.spaghetti.getSessionMessages(projectSlug, session.sessionId, delta, oldTotal, scope);
      const fresh = page.messages as unknown as AnyMsg[];
      if (fresh.length === 0) {
        totalRef.current = newTotal;
        setPageMeta((m) => (m ? { ...m, total: newTotal } : { total: newTotal, hasMore: false }));
        return;
      }

      if (nearBottomRef.current) {
        shouldScrollToBottomRef.current = true;
        setRawMessages((prev) => [...prev, ...fresh]);
        setPendingNew(0);
      } else {
        setRawMessages((prev) => [...prev, ...fresh]);
        setPendingNew((n) => n + fresh.length);
      }

      totalRef.current = newTotal;
      setPageMeta((m) => (m ? { ...m, total: newTotal } : { total: newTotal, hasMore: offsetRef.current > 0 }));

      setLivePulse(true);
      if (pulseTimerRef.current) clearTimeout(pulseTimerRef.current);
      pulseTimerRef.current = setTimeout(() => setLivePulse(false), 1200);
    } catch {
      /* live refresh is best-effort */
    }
  }, [projectSlug, session.sessionId, sourceId]);

  useEffect(() => {
    const unsub = window.spaghetti.onChange((batch: SegmentChangeBatch) => {
      const relevant =
        !batch.changes?.length ||
        batch.changes.some((c) => !c.sessionId || c.sessionId === session.sessionId || c.projectSlug === projectSlug);
      if (!relevant) return;

      if (liveTimerRef.current) clearTimeout(liveTimerRef.current);
      liveTimerRef.current = setTimeout(() => {
        void appendLiveTail();
      }, LIVE_DEBOUNCE_MS);
    });

    return () => {
      unsub();
      if (liveTimerRef.current) clearTimeout(liveTimerRef.current);
      if (pulseTimerRef.current) clearTimeout(pulseTimerRef.current);
    };
  }, [session.sessionId, projectSlug, appendLiveTail]);

  // ── Scroll restoration ────────────────────────────────────────────────
  useLayoutEffect(() => {
    const container = scrollContainerRef.current;
    if (!container) return;

    if (isPrependingRef.current) {
      const newScrollHeight = container.scrollHeight;
      const diff = newScrollHeight - prevScrollHeightRef.current;
      container.scrollTop += diff;
      isPrependingRef.current = false;
    } else if (shouldScrollToBottomRef.current) {
      container.scrollTop = container.scrollHeight;
      shouldScrollToBottomRef.current = false;
      nearBottomRef.current = true;
    }
  }, [rawMessages]);

  // ── Auto-load older messages when near top ────────────────────────────
  const loadMoreMessages = useCallback(async () => {
    if (loadMoreInFlightRef.current) return;
    if (loadingMore) return;
    if (!pageMeta?.hasMore) return;
    if (offsetRef.current <= 0) return;

    const container = scrollContainerRef.current;
    if (container) {
      prevScrollHeightRef.current = container.scrollHeight;
    }

    loadMoreInFlightRef.current = true;
    setLoadingMore(true);
    try {
      await delay(LOADING_DELAY_MS);

      const newOffset = Math.max(0, offsetRef.current - PAGE_SIZE);
      const limit = offsetRef.current - newOffset;
      if (limit <= 0) return;

      const page = await window.spaghetti.getSessionMessages(projectSlug, session.sessionId, limit, newOffset, {
        sourceId,
      });

      if (page.messages.length > 0) {
        isPrependingRef.current = true;
        setRawMessages((prev) => [...(page.messages as unknown as AnyMsg[]), ...prev]);
        setPageMeta({ total: page.total, hasMore: newOffset > 0 });
        offsetRef.current = newOffset;
        totalRef.current = page.total;
      } else {
        setPageMeta((m) => (m ? { ...m, hasMore: false } : m));
      }
    } catch (e: unknown) {
      setError(String(e));
      isPrependingRef.current = false;
    } finally {
      setLoadingMore(false);
      loadMoreInFlightRef.current = false;
    }
  }, [projectSlug, session.sessionId, sourceId, loadingMore, pageMeta?.hasMore]);

  const handleScroll = useCallback(
    (e: React.UIEvent<HTMLDivElement>) => {
      const el = e.currentTarget;
      const { scrollTop, scrollHeight, clientHeight } = el;
      nearBottomRef.current = scrollHeight - scrollTop - clientHeight < NEAR_BOTTOM_PX;

      if (nearBottomRef.current && pendingNew > 0) {
        setPendingNew(0);
      }

      if (scrollTop < NEAR_TOP_PX && !loadingMore && pageMeta?.hasMore && rawMessages.length > 0) {
        void loadMoreMessages();
      }
    },
    [loadingMore, pageMeta?.hasMore, rawMessages.length, loadMoreMessages, pendingNew],
  );

  const jumpToLatest = useCallback(() => {
    const container = scrollContainerRef.current;
    if (container) {
      container.scrollTop = container.scrollHeight;
    }
    nearBottomRef.current = true;
    setPendingNew(0);
  }, []);

  const openArtifacts = (tab: ArtifactTab) => {
    setArtifactTab(tab);
    setArtifactsOpen(true);
  };

  const prompt = flattenPrompt(session.firstPrompt || session.summary, 80);
  const tok = formatTokenUsage(session.tokenUsage, session.sourceId, session.tokensEstimated);
  const hasMore = pageMeta?.hasMore ?? false;
  const filtersActive =
    anySoloActive ||
    searchQuery.trim().length > 0 ||
    Object.values(typeFilters).some((f) => f.mute) ||
    Object.values(toolFilters).some((f) => f.mute);

  const artifactHints = {
    todoCount: session.todoCount,
    planSlug: session.planSlug,
    hasTask: session.hasTask,
    hasMemory,
  };

  return (
    <div className="flex h-full min-h-0 bg-[#0a0a0a] text-[#f2f2f2]">
      <div className="flex flex-col flex-1 min-w-0 min-h-0">
        {/* Header */}
        <div className="shrink-0 px-4 py-2.5 border-b border-white/10 bg-white/[0.02] flex items-start gap-3">
          <Btn onClick={onBack} className="mt-0.5">
            ← Sessions
          </Btn>
          <div className="min-w-0 flex-1">
            <div className="flex items-center gap-2 flex-wrap">
              <span className="text-xs font-medium text-white/85">#{sessionIndex + 1}</span>
              {session.gitBranch ? (
                <span className="text-[11px] font-mono text-amber-200/70">{session.gitBranch}</span>
              ) : null}
              <span className="text-[10px] font-mono text-white/30">{session.sessionId.slice(0, 8)}</span>
              <SourceBadge sourceId={session.sourceId} />
              <LiveDot active={livePulse || pendingNew > 0} />
              {pageMeta && (
                <span className="text-[10px] text-white/35 font-mono ml-auto">
                  {rawMessages.length}/{pageMeta.total} msgs
                  {filtersActive ? ` · ${chatMessages.length} shown` : ''}
                </span>
              )}
            </div>
            <div className="text-[11px] italic text-white/40 truncate mt-0.5">
              {prompt ? `"${prompt}"` : '(no prompt)'}
            </div>
            <div className="text-[10px] text-white/35 font-mono mt-1">
              {formatNumber(session.messageCount)} msgs
              <Dot />
              {tok} tokens
              <Dot />
              {formatDuration(session.lifespanMs)}
              <Dot />
              {formatRelativeTime(session.lastUpdate)}
            </div>
          </div>

          {/* Artifact shortcuts */}
          <div className="flex items-center gap-1.5 shrink-0 flex-wrap justify-end max-w-[220px]">
            <Btn
              variant={artifactsOpen ? 'solid' : 'ghost'}
              onClick={() => (artifactsOpen ? setArtifactsOpen(false) : openArtifacts('plan'))}
              title="Session artifacts"
            >
              Artifacts
            </Btn>
            {session.planSlug ? (
              <Btn className="!px-1.5" onClick={() => openArtifacts('plan')} title="Plan">
                Plan
              </Btn>
            ) : null}
            {session.todoCount > 0 ? (
              <Btn className="!px-1.5" onClick={() => openArtifacts('todos')} title="Todos">
                {session.todoCount} todos
              </Btn>
            ) : null}
            {session.hasTask ? (
              <Btn className="!px-1.5" onClick={() => openArtifacts('task')} title="Task">
                Task
              </Btn>
            ) : null}
          </div>
        </div>

        {error && (
          <div className="shrink-0 px-4 py-2 text-[11px] text-red-300/90 border-b border-red-500/20 bg-red-500/[0.04]">
            {error}
          </div>
        )}

        {/* Solo/mute type + tool filters (ProjectPage parity) */}
        {timeline.length > 0 && !loading ? (
          <MessageFilterBar
            typeFilters={typeFilters}
            toolFilters={toolFilters}
            searchQuery={searchQuery}
            visibleTypes={visibleTypes}
            visibleTools={visibleTools}
            anySoloActive={anySoloActive}
            messageCounts={messageCounts}
            toolCounts={toolCounts}
            filteredCount={chatMessages.length}
            totalCount={filterTotalCount}
            toggleTypeSolo={toggleTypeSolo}
            toggleTypeMute={toggleTypeMute}
            toggleToolSolo={toggleToolSolo}
            toggleToolMute={toggleToolMute}
            clearAllSolos={clearAllSolos}
            setSearchQuery={setSearchQuery}
          />
        ) : null}

        {/* Timeline scroll container */}
        <div className="relative flex-1 min-h-0">
          <div ref={scrollContainerRef} onScroll={handleScroll} className="h-full overflow-y-auto">
            {loading && rawMessages.length === 0 ? (
              <div className="flex items-center justify-center h-full gap-3 py-12">
                <Spinner className="h-5 w-5" />
                <span className="text-xs text-white/40">Loading messages…</span>
              </div>
            ) : timeline.length === 0 ? (
              <div className="flex flex-col items-center justify-center h-full opacity-50 py-12">
                <p className="text-sm">No messages in this session</p>
                <p className="text-[11px] text-white/40 mt-1">New turns will appear here live.</p>
              </div>
            ) : chatMessages.length === 0 ? (
              <div className="flex flex-col items-center justify-center h-full py-12 gap-2">
                <p className="text-sm text-white/45">No messages match filters</p>
                <p className="text-[11px] text-white/30 max-w-xs text-center">
                  Unpin solos, unmute types/tools, or clear the filter text.
                </p>
                {anySoloActive ? (
                  <Btn onClick={clearAllSolos} className="mt-2">
                    Clear solos
                  </Btn>
                ) : searchQuery ? (
                  <Btn onClick={() => setSearchQuery('')} className="mt-2">
                    Clear text filter
                  </Btn>
                ) : null}
              </div>
            ) : (
              <div className="py-3 max-w-3xl mx-auto w-full">
                {loadingMore && (
                  <div className="flex items-center justify-center py-6 gap-3">
                    <Spinner className="h-5 w-5" />
                    <span className="text-sm text-white/50">Loading older messages…</span>
                  </div>
                )}

                {!hasMore && chatMessages.length > 0 && (
                  <div className="flex items-center justify-center py-4 text-xs text-white/25">
                    — Beginning of conversation —
                  </div>
                )}

                {hasMore && !loadingMore && (
                  <div className="flex items-center justify-center py-3 text-xs text-white/35">
                    ↑ Scroll up for older messages
                  </div>
                )}

                {chatMessages.map((msg, i) => {
                  const prev = i > 0 ? chatMessages[i - 1] : undefined;
                  const next = i < chatMessages.length - 1 ? chatMessages[i + 1] : undefined;
                  const isLast = i === chatMessages.length - 1;
                  const connectToNext = !!(next && isTimelineType(next.type));
                  const showSep = shouldShowTimestamp(msg.timestamp, prev?.timestamp ?? null);

                  const taskSpawnedAgent =
                    msg.type === 'tool_use' &&
                    msg.toolUse?.toolName === 'Task' &&
                    Boolean((msg.toolUse as { result?: { content?: string } })?.result);

                  const nextIsSidechain = taskSpawnedAgent ? true : next?.isSidechain;

                  return (
                    <div key={msg.uuid} className="px-4">
                      {showSep && <TimeGroupSeparator timestamp={msg.timestamp} />}
                      <TimelineMessageRenderer
                        message={msg}
                        isLast={isLast}
                        connectToNext={connectToNext}
                        prevTimestamp={prev?.timestamp}
                        prevAgentId={prev?.agentId}
                        nextAgentId={next?.agentId}
                        nextIsSidechain={nextIsSidechain}
                      />
                    </div>
                  );
                })}
              </div>
            )}
          </div>

          {/* Jump to latest pill when scrolled up during live appends */}
          {pendingNew > 0 ? (
            <div className="absolute bottom-4 left-1/2 -translate-x-1/2 z-10">
              <button
                type="button"
                onClick={jumpToLatest}
                className="text-[11px] px-3 py-1.5 rounded-full border border-orange-400/40 bg-[#141414]/95 text-orange-200/90 shadow-lg cursor-pointer hover:bg-[#1a1a1a] transition-colors"
              >
                ↓ {pendingNew} new message{pendingNew === 1 ? '' : 's'}
              </button>
            </div>
          ) : null}
        </div>
      </div>

      <ArtifactPanel
        open={artifactsOpen}
        onClose={() => setArtifactsOpen(false)}
        projectSlug={projectSlug}
        sourceId={sourceId}
        sessionId={session.sessionId}
        hints={artifactHints}
        initialTab={artifactTab}
      />
    </div>
  );
}

function delay(ms: number): Promise<void> {
  return new Promise((r) => setTimeout(r, ms));
}

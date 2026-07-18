/**
 * Session message browser — timeline chat with:
 * - reverse pagination (scroll up for older)
 * - live tail appends via onChange
 * - artifact drawer (plan / todos / task / subagents / memory)
 * - type/tool solo·mute filters + in-transcript text filter (ProjectPage parity)
 */

import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from 'react';
import { ArrowLeft, PanelLeft, PanelRight } from 'lucide-react';
import { transformRawMessagesToTimeline, type ChatSessionMessage } from '@vibecook/spaghetti-sdk/react';
import type { MessagePage, SegmentChangeBatch, SessionListItem } from '@vibecook/spaghetti-sdk';
import { SourceBadge } from './SourceBadge.js';
import { ArchiveTranscript } from './ArchiveTranscript.js';
import { MessageFilterBar, useMessageFilters, countTimelineMessages, filterTimelineMessages } from './filters/index.js';
import { Btn, LiveDot, Spinner } from './ui.js';

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
  isDark?: boolean;
  leftOpen?: boolean;
  onToggleLeft?: () => void;
  filesOpen?: boolean;
  onToggleFiles?: () => void;
  onBack: () => void;
  /** Development gallery records; bypasses IPC/pagination when supplied. */
  debugMessages?: readonly AnyMsg[];
}

export function SessionMessagesView({
  projectSlug,
  sourceId,
  session,
  sessionIndex,
  hasMemory = false,
  isDark = true,
  leftOpen = true,
  onToggleLeft,
  filesOpen = true,
  onToggleFiles,
  onBack,
  debugMessages,
}: SessionMessagesViewProps) {
  const [rawMessages, setRawMessages] = useState<AnyMsg[]>([]);
  const [pageMeta, setPageMeta] = useState<Pick<MessagePage, 'total' | 'hasMore'> | null>(null);
  const [loading, setLoading] = useState(true);
  const [loadingMore, setLoadingMore] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [livePulse, setLivePulse] = useState(false);
  const [pendingNew, setPendingNew] = useState(0);

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

  // Full timeline (pre-filter) — filters apply after transform.
  // Codex/Grok rows are RolloutLine / chat_history JSON; adapt via sourceId.
  const timeline: ChatSessionMessage[] = useMemo(
    () => transformRawMessagesToTimeline(rawMessages, { sourceId }),
    [rawMessages, sourceId],
  );

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

  // The reference tray reports visible transcript rows, not pill-count totals
  // (tool-use rows already have a per-tool pill and must not be double-counted).
  const filterTotalCount = timeline.length;

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
    if (debugMessages) {
      const messages = [...debugMessages];
      setRawMessages(messages);
      setPageMeta({ total: messages.length, hasMore: false });
      totalRef.current = messages.length;
      setLoading(false);
      return;
    }
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
  }, [projectSlug, session.sessionId, sourceId, debugMessages]);

  // ── Live tail: append new messages when the index reports changes ─────
  const appendLiveTail = useCallback(async () => {
    if (debugMessages) return;
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
  }, [projectSlug, session.sessionId, sourceId, debugMessages]);

  useEffect(() => {
    if (debugMessages) return;
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
  }, [session.sessionId, projectSlug, appendLiveTail, debugMessages]);

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

  void hasMemory; // artifacts live in Structure panel
  const hasMore = pageMeta?.hasMore ?? false;
  const filtersActive =
    anySoloActive ||
    searchQuery.trim().length > 0 ||
    Object.values(typeFilters).some((f) => f.mute) ||
    Object.values(toolFilters).some((f) => f.mute);

  return (
    <div className="flex flex-col h-full min-h-0 bg-transparent text-ink">
      {/* Reading header — design: serif tray + mono meta (App.tsx ~748–763) */}
      <div className="h-10 border-b border-[color:var(--archive-ink-line)] flex items-center px-6 justify-between shrink-0 bg-transparent gap-3">
        <div className="flex items-center gap-4 min-w-0 text-[10px] font-serif tracking-[0.15em]">
          {!leftOpen && onToggleLeft ? (
            <button
              type="button"
              onClick={onToggleLeft}
              className="opacity-50 hover:opacity-100 bg-transparent border-0 text-ink cursor-pointer p-0"
              title="Show projects"
            >
              <PanelLeft size={14} />
            </button>
          ) : null}
          <button
            type="button"
            onClick={onBack}
            className="opacity-70 hover:opacity-100 bg-transparent border-0 text-ink cursor-pointer p-0"
            aria-label="Back to sessions"
          >
            <ArrowLeft size={14} />
          </button>
          <span className="font-mono text-[10px] tracking-[0.1em] opacity-70">#{sessionIndex + 1}</span>
          <span className="font-mono text-[10px] tracking-[0.1em] opacity-70">{session.sessionId.slice(0, 8)}</span>
          <SourceBadge sourceId={session.sourceId} isDark={isDark} size="md" />
          <LiveDot active={livePulse || pendingNew > 0} />
          {pageMeta && (
            <span className="font-mono text-[9px] tracking-[0.08em] opacity-60 truncate">
              {rawMessages.length}/{pageMeta.total} msgs
              {filtersActive ? ` · ${chatMessages.length} shown` : ''}
            </span>
          )}
        </div>
        <div className="flex items-center gap-4 shrink-0 font-mono text-[9px] uppercase tracking-widest opacity-70">
          {session.gitBranch ? (
            <span className="text-sanguine normal-case tracking-normal">{session.gitBranch}</span>
          ) : null}
          {!filesOpen && onToggleFiles ? (
            <button
              type="button"
              onClick={onToggleFiles}
              className="opacity-50 hover:opacity-100 bg-transparent border-0 text-ink cursor-pointer p-0"
              title="Show structure"
            >
              <PanelRight size={14} />
            </button>
          ) : null}
        </div>
      </div>

      {error && (
        <div className="shrink-0 px-6 py-2 font-mono text-[10px] text-sanguine border-b border-sanguine/20 bg-sanguine/[0.04]">
          {error}
        </div>
      )}

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
          isDark={isDark}
        />
      ) : null}

      <div className="relative flex-1 min-h-0 archive-transcript">
        <div ref={scrollContainerRef} onScroll={handleScroll} className="h-full overflow-y-auto scrollbar-hide">
          {loading && rawMessages.length === 0 ? (
            <div className="flex items-center justify-center h-full gap-3 py-12">
              <Spinner className="h-5 w-5" />
              <span className="font-mono text-[10px] tracking-widest uppercase opacity-50">Transcribing…</span>
            </div>
          ) : timeline.length === 0 ? (
            <div className="flex flex-col items-center justify-center h-full opacity-50 py-12">
              <p className="font-serif text-[14px] leading-relaxed">No messages in this session</p>
              <p className="font-mono text-[10px] tracking-[0.08em] uppercase opacity-60 mt-1">
                New turns will appear here live.
              </p>
            </div>
          ) : chatMessages.length === 0 ? (
            <div className="flex flex-col items-center justify-center h-full py-12 gap-2">
              <p className="font-serif text-[14px] leading-relaxed opacity-50">No messages match filters</p>
              <p className="font-mono text-[10px] tracking-[0.08em] uppercase opacity-40 max-w-xs text-center">
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
            <div className="px-4 pb-4 pt-0 md:px-8 md:pb-8 lg:px-16">
              <div className="max-w-3xl mx-auto flex flex-col pt-4 pb-8">
                {loadingMore && (
                  <div className="flex items-center justify-center py-6 gap-3">
                    <Spinner className="h-5 w-5" />
                    <span className="font-mono text-[10px] uppercase tracking-widest opacity-50">
                      Loading older messages…
                    </span>
                  </div>
                )}

                {!hasMore && chatMessages.length > 0 && (
                  <div className="flex items-center justify-center py-4 font-mono text-[9px] tracking-widest uppercase opacity-30">
                    — Beginning of conversation —
                  </div>
                )}

                {hasMore && !loadingMore && (
                  <div className="flex items-center justify-center py-3 font-mono text-[9px] tracking-widest uppercase opacity-40">
                    ↑ Scroll up for older messages
                  </div>
                )}

                <ArchiveTranscript messages={chatMessages} isDark={isDark} />
              </div>
            </div>
          )}
        </div>

        {pendingNew > 0 ? (
          <div className="absolute bottom-4 left-1/2 -translate-x-1/2 z-10">
            <button
              type="button"
              onClick={jumpToLatest}
              className="font-mono text-[10px] uppercase tracking-widest px-3 py-1.5 border border-[color:var(--archive-ink-line)] bg-paper text-ink shadow-lg cursor-pointer hover:bg-ink hover:text-paper transition-colors"
            >
              ↓ {pendingNew} new message{pendingNew === 1 ? '' : 's'}
            </button>
          </div>
        ) : null}
      </div>
    </div>
  );
}

function delay(ms: number): Promise<void> {
  return new Promise((r) => setTimeout(r, ms));
}

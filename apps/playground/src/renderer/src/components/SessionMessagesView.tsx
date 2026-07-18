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
import type {
  SegmentChangeBatch,
  SessionListItem,
  TimelineFacets,
  TimelinePage,
  TimelinePageRequest,
} from '@vibecook/spaghetti-sdk';
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
  const [timeline, setTimeline] = useState<ChatSessionMessage[]>([]);
  const [facets, setFacets] = useState<TimelineFacets | null>(null);
  const [pageMeta, setPageMeta] = useState<Pick<TimelinePage, 'total' | 'hasMore'> | null>(null);
  const [loading, setLoading] = useState(true);
  const [loadingMore, setLoadingMore] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [livePulse, setLivePulse] = useState(false);
  const [pendingNew, setPendingNew] = useState(0);

  const cursorRef = useRef<number | undefined>(undefined);
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
  const [debouncedSearch, setDebouncedSearch] = useState('');
  const cacheRef = useRef(
    new Map<
      string,
      {
        messages: ChatSessionMessage[];
        pageMeta: Pick<TimelinePage, 'total' | 'hasMore'>;
        cursor?: number;
        scrollTop: number;
      }
    >(),
  );
  const activeQueryKeyRef = useRef('');
  const restoreScrollTopRef = useRef<number | null>(null);

  const debugTimeline = useMemo(
    () => (debugMessages ? transformRawMessagesToTimeline([...debugMessages], { sourceId }) : []),
    [debugMessages, sourceId],
  );
  const debugCounts = useMemo(() => countTimelineMessages(debugTimeline), [debugTimeline]);
  const messageCounts = facets?.messageCounts ?? debugCounts.messageCounts;
  const toolCounts = facets?.toolCounts ?? debugCounts.toolCounts;
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

  useEffect(() => {
    const timer = setTimeout(() => setDebouncedSearch(searchQuery.trim()), 180);
    return () => clearTimeout(timer);
  }, [searchQuery]);

  const timelineRequest = useMemo<TimelinePageRequest>(() => {
    const request: TimelinePageRequest = { sourceId, limit: PAGE_SIZE };
    if (anySoloActive) {
      request.includeTypes = Object.entries(typeFilters)
        .filter(([, state]) => state.solo)
        .map(([type]) => type);
      request.includeTools = Object.entries(toolFilters)
        .filter(([, state]) => state.solo)
        .map(([tool]) => tool);
    } else {
      request.excludeTypes = Object.entries(typeFilters)
        .filter(([, state]) => state.mute)
        .map(([type]) => type);
      request.excludeTools = Object.entries(toolFilters)
        .filter(([, state]) => state.mute)
        .map(([tool]) => tool);
    }
    if (debouncedSearch) request.search = debouncedSearch;
    return request;
  }, [sourceId, anySoloActive, typeFilters, toolFilters, debouncedSearch]);
  const queryKey = useMemo(() => JSON.stringify(timelineRequest), [timelineRequest]);

  const chatMessages = useMemo(() => {
    if (!debugMessages) return timeline;
    return filterTimelineMessages({
      messages: debugTimeline,
      visibleTypes,
      visibleTools,
      typeFilters,
      toolFilters,
      anySoloActive,
      searchQuery,
    });
  }, [
    debugMessages,
    timeline,
    debugTimeline,
    visibleTypes,
    visibleTools,
    typeFilters,
    toolFilters,
    anySoloActive,
    searchQuery,
  ]);

  const filterTotalCount = facets?.total ?? debugTimeline.length;

  // Reset when session changes
  useEffect(() => {
    isPrependingRef.current = false;
    shouldScrollToBottomRef.current = false;
    prevScrollHeightRef.current = 0;
    loadMoreInFlightRef.current = false;
    nearBottomRef.current = true;
    cursorRef.current = undefined;
    totalRef.current = 0;
    cacheRef.current.clear();
    activeQueryKeyRef.current = '';
    setFacets(null);
    setPendingNew(0);
    setError(null);
    resetFilters();
  }, [session.sessionId, resetFilters]);

  // Session-wide facets come from normalized DB rows, never the loaded page.
  useEffect(() => {
    let cancelled = false;
    if (debugMessages) {
      return;
    }
    void (async () => {
      try {
        const next = await window.spaghetti.getSessionTimelineFacets(projectSlug, session.sessionId, { sourceId });
        if (!cancelled) setFacets(next);
      } catch (e: unknown) {
        if (!cancelled) setError(String(e));
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [projectSlug, session.sessionId, sourceId, debugMessages]);

  // Query-keyed pages: changing solo/mute/search starts a DB query across the
  // entire session. Returning to a prior key restores its page and scroll.
  useEffect(() => {
    if (debugMessages) {
      setTimeline(debugTimeline);
      setPageMeta({ total: debugTimeline.length, hasMore: false });
      totalRef.current = debugTimeline.length;
      setLoading(false);
      return;
    }
    const previousKey = activeQueryKeyRef.current;
    if (previousKey && previousKey !== queryKey && pageMeta) {
      cacheRef.current.set(previousKey, {
        messages: timeline,
        pageMeta,
        cursor: cursorRef.current,
        scrollTop: scrollContainerRef.current?.scrollTop ?? 0,
      });
    }
    activeQueryKeyRef.current = queryKey;
    const cached = cacheRef.current.get(queryKey);
    if (cached) {
      setTimeline(cached.messages);
      setPageMeta(cached.pageMeta);
      cursorRef.current = cached.cursor;
      totalRef.current = cached.pageMeta.total;
      restoreScrollTopRef.current = cached.scrollTop;
      setLoading(false);
      return;
    }

    let cancelled = false;
    setLoading(true);
    setError(null);
    setTimeline([]);
    setPageMeta(null);
    cursorRef.current = undefined;
    void (async () => {
      try {
        await delay(LOADING_DELAY_MS);
        const page = await window.spaghetti.getSessionTimeline(projectSlug, session.sessionId, timelineRequest);
        if (cancelled) return;
        shouldScrollToBottomRef.current = true;
        setTimeline(page.messages as ChatSessionMessage[]);
        setPageMeta({ total: page.total, hasMore: page.hasMore });
        cursorRef.current = page.nextCursor;
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
    // pageMeta/timeline are intentionally snapshots saved when queryKey changes.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [projectSlug, session.sessionId, queryKey, debugMessages, debugTimeline]);

  const appendLiveTail = useCallback(async () => {
    if (debugMessages) return;
    try {
      cacheRef.current.clear();
      const [nextFacets, page] = await Promise.all([
        window.spaghetti.getSessionTimelineFacets(projectSlug, session.sessionId, { sourceId }),
        window.spaghetti.getSessionTimeline(projectSlug, session.sessionId, timelineRequest),
      ]);
      setFacets(nextFacets);
      const delta = Math.max(0, page.total - totalRef.current);
      totalRef.current = page.total;
      if (nearBottomRef.current) {
        shouldScrollToBottomRef.current = true;
        setTimeline(page.messages as ChatSessionMessage[]);
        setPageMeta({ total: page.total, hasMore: page.hasMore });
        cursorRef.current = page.nextCursor;
        setPendingNew(0);
      } else if (delta > 0) {
        setPendingNew((count) => count + delta);
      }
      setLivePulse(true);
      if (pulseTimerRef.current) clearTimeout(pulseTimerRef.current);
      pulseTimerRef.current = setTimeout(() => setLivePulse(false), 1200);
    } catch {
      /* live refresh is best-effort */
    }
  }, [projectSlug, session.sessionId, sourceId, timelineRequest, debugMessages]);

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
    if (restoreScrollTopRef.current != null) {
      container.scrollTop = restoreScrollTopRef.current;
      restoreScrollTopRef.current = null;
    }
  }, [timeline]);

  // ── Auto-load older messages when near top ────────────────────────────
  const loadMoreMessages = useCallback(async () => {
    if (loadMoreInFlightRef.current) return;
    if (loadingMore) return;
    if (!pageMeta?.hasMore) return;
    if (cursorRef.current == null) return;

    const container = scrollContainerRef.current;
    if (container) {
      prevScrollHeightRef.current = container.scrollHeight;
    }

    loadMoreInFlightRef.current = true;
    setLoadingMore(true);
    try {
      await delay(LOADING_DELAY_MS);

      const page = await window.spaghetti.getSessionTimeline(projectSlug, session.sessionId, {
        ...timelineRequest,
        before: cursorRef.current,
      });

      if (page.messages.length > 0) {
        isPrependingRef.current = true;
        setTimeline((prev) => [...(page.messages as ChatSessionMessage[]), ...prev]);
        setPageMeta({ total: page.total, hasMore: page.hasMore });
        cursorRef.current = page.nextCursor;
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
  }, [projectSlug, session.sessionId, timelineRequest, loadingMore, pageMeta?.hasMore]);

  const handleScroll = useCallback(
    (e: React.UIEvent<HTMLDivElement>) => {
      const el = e.currentTarget;
      const { scrollTop, scrollHeight, clientHeight } = el;
      nearBottomRef.current = scrollHeight - scrollTop - clientHeight < NEAR_BOTTOM_PX;

      if (nearBottomRef.current && pendingNew > 0) {
        setPendingNew(0);
      }

      if (scrollTop < NEAR_TOP_PX && !loadingMore && pageMeta?.hasMore && timeline.length > 0) {
        void loadMoreMessages();
      }
    },
    [loadingMore, pageMeta?.hasMore, timeline.length, loadMoreMessages, pendingNew],
  );

  const jumpToLatest = useCallback(() => {
    nearBottomRef.current = true;
    setPendingNew(0);
    void appendLiveTail();
  }, [appendLiveTail]);

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
              {timeline.length}/{pageMeta.total} loaded
              {filtersActive && facets ? ` · ${pageMeta.total}/${facets.total} match` : ''}
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

      {filterTotalCount > 0 && !loading ? (
        <MessageFilterBar
          typeFilters={typeFilters}
          toolFilters={toolFilters}
          searchQuery={searchQuery}
          visibleTypes={visibleTypes}
          visibleTools={visibleTools}
          anySoloActive={anySoloActive}
          messageCounts={messageCounts}
          toolCounts={toolCounts}
          filteredCount={debugMessages ? chatMessages.length : (pageMeta?.total ?? 0)}
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
          {loading && timeline.length === 0 ? (
            <div className="flex items-center justify-center h-full gap-3 py-12">
              <Spinner className="h-5 w-5" />
              <span className="font-mono text-[10px] tracking-widest uppercase opacity-50">Transcribing…</span>
            </div>
          ) : filterTotalCount === 0 ? (
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

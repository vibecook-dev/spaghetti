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
  SessionListItem,
  SubagentListItem,
  SubagentTimelinePageRequest,
  TimelineFacets,
  TimelinePage,
  TimelinePageRequest,
} from '@vibecook/spaghetti-sdk';
import type { ActiveSessionChange } from '../../../shared/ipc.js';
import { SourceBadge } from './SourceBadge.js';
import { ArchiveTranscript } from './ArchiveTranscript.js';
import { MessageFilterBar, useMessageFilters, countTimelineMessages, filterTimelineMessages } from './filters/index.js';
import { Btn, LiveDot, Spinner } from './ui.js';

type AnyMsg = Record<string, unknown>;

const PAGE_SIZE = 30;
const NEAR_TOP_PX = 80;
const NEAR_BOTTOM_PX = 140;
const LIVE_DEBOUNCE_MS = 80;
const BRANCH_PAGE_SIZE = 80;
const timelineFingerprintCache = new WeakMap<object, string>();

function timelineFingerprint(message: ChatSessionMessage): string {
  const cached = timelineFingerprintCache.get(message);
  if (cached) return cached;
  let fingerprint: string;
  try {
    fingerprint = JSON.stringify(message);
  } catch {
    fingerprint = `${message.timelineId}:${message.timestamp}:${message.type}:${message.content ?? ''}`;
  }
  timelineFingerprintCache.set(message, fingerprint);
  return fingerprint;
}

/** Preserve object identity for unchanged rows so virtualized Markdown does not re-render. */
function reconcileTimeline(
  current: readonly ChatSessionMessage[],
  incoming: readonly ChatSessionMessage[],
): ChatSessionMessage[] {
  const byId = new Map(
    incoming.filter((message) => message.timelineId).map((message) => [message.timelineId, message]),
  );
  const currentIds = new Set(current.map((message) => message.timelineId).filter(Boolean));
  const preserved = current.map((previous) => {
    const next = previous.timelineId ? byId.get(previous.timelineId) : undefined;
    return next && timelineFingerprint(previous) !== timelineFingerprint(next) ? next : previous;
  });
  const additions = incoming.filter((message) => !message.timelineId || !currentIds.has(message.timelineId));
  return [...preserved, ...additions];
}

interface BranchPageState {
  messages: ChatSessionMessage[];
  total: number;
  offset: number;
  hasMore: boolean;
  loading: boolean;
}

export interface InitialBranchTarget {
  agentId: string;
  workflowId?: string;
  spawnToolId?: string;
  agentTimelineIndex?: number;
}

function branchKey(thread: SubagentListItem): string {
  return JSON.stringify([thread.sourceId, thread.workflowId, thread.agentId]);
}

function threadToolId(thread: SubagentListItem): string {
  return thread.spawnToolId ?? `unlinked-agent:${branchKey(thread)}`;
}

export interface SessionMessagesViewProps {
  projectSlug: string;
  sourceId: string;
  session: SessionListItem;
  sessionIndex: number;
  isLive?: boolean;
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
  initialBranchTarget?: InitialBranchTarget;
  onInitialBranchTargetConsumed?: () => void;
}

export function SessionMessagesView({
  projectSlug,
  sourceId,
  session,
  sessionIndex,
  isLive = false,
  hasMemory = false,
  isDark = true,
  leftOpen = true,
  onToggleLeft,
  filesOpen = true,
  onToggleFiles,
  onBack,
  debugMessages,
  initialBranchTarget,
  onInitialBranchTargetConsumed,
}: SessionMessagesViewProps) {
  const [timeline, setTimeline] = useState<ChatSessionMessage[]>([]);
  const [facets, setFacets] = useState<TimelineFacets | null>(null);
  const [pageMeta, setPageMeta] = useState<Pick<TimelinePage, 'total' | 'hasMore'> | null>(null);
  const [loading, setLoading] = useState(true);
  const [loadingMore, setLoadingMore] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [pendingNew, setPendingNew] = useState(0);
  const [subagents, setSubagents] = useState<SubagentListItem[]>([]);
  const [expandedBranchToolIds, setExpandedBranchToolIds] = useState<Set<string>>(() => new Set());
  const [branchPages, setBranchPages] = useState<Map<string, BranchPageState>>(() => new Map());
  const [pendingBranchScroll, setPendingBranchScroll] = useState<string | null>(null);

  const cursorRef = useRef<number | undefined>(undefined);
  const scrollContainerRef = useRef<HTMLDivElement>(null);
  const prevScrollHeightRef = useRef(0);
  const isPrependingRef = useRef(false);
  const shouldScrollToBottomRef = useRef(false);
  const scrollToBottomBehaviorRef = useRef<ScrollBehavior>('auto');
  const tailScrollInProgressRef = useRef(false);
  const tailScrollEndTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const loadMoreInFlightRef = useRef(false);
  /** True when the viewport is near the conversation tail. */
  const nearBottomRef = useRef(true);
  const totalRef = useRef(0);
  const liveTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  /** Invalidates overlapping live refreshes and refreshes from a prior session. */
  const liveRefreshSeqRef = useRef(0);
  const liveNeedsSubagentRefreshRef = useRef(false);
  const activeStreamIdRef = useRef<string | null>(null);
  const lastActiveRevisionRef = useRef(0);
  const preOpenActiveChangeRef = useRef<ActiveSessionChange | null>(null);
  const requestLiveRefreshRef = useRef<(refreshSubagents: boolean, reset: boolean) => void>(() => {});
  const liveResetPendingRef = useRef(false);
  const consumedBranchTargetRef = useRef<string | null>(null);
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

  const branchRequest = useMemo<SubagentTimelinePageRequest>(() => {
    return {
      sourceId,
      includeTypes: timelineRequest.includeTypes,
      includeTools: timelineRequest.includeTools,
      excludeTypes: timelineRequest.excludeTypes,
      excludeTools: timelineRequest.excludeTools,
      search: timelineRequest.search,
      limit: BRANCH_PAGE_SIZE,
      offset: 0,
    };
  }, [timelineRequest, sourceId]);

  const loadBranchPage = useCallback(
    async (thread: SubagentListItem, append: boolean) => {
      const key = branchKey(thread);
      const current = branchPages.get(key);
      if (current?.loading) return;
      const offset = append ? (current?.offset ?? 0) + (current?.messages.length ?? 0) : 0;
      setBranchPages((pages) => {
        const next = new Map(pages);
        next.set(key, {
          messages: append ? (pages.get(key)?.messages ?? []) : [],
          total: pages.get(key)?.total ?? thread.messageCount,
          offset: append ? (pages.get(key)?.offset ?? 0) : 0,
          hasMore: pages.get(key)?.hasMore ?? false,
          loading: true,
        });
        return next;
      });
      try {
        const page = await window.spaghetti.getSubagentTimeline(projectSlug, session.sessionId, thread.agentId, {
          ...branchRequest,
          workflowId: thread.workflowId,
          offset,
        });
        setBranchPages((pages) => {
          const next = new Map(pages);
          const previous = next.get(key);
          next.set(key, {
            messages: append
              ? [...(previous?.messages ?? []), ...(page.messages as ChatSessionMessage[])]
              : (page.messages as ChatSessionMessage[]),
            total: page.total,
            offset: append ? (previous?.offset ?? 0) : page.offset,
            hasMore: page.hasMore,
            loading: false,
          });
          return next;
        });
      } catch (e: unknown) {
        setError(String(e));
        setBranchPages((pages) => {
          const next = new Map(pages);
          const previous = next.get(key);
          if (previous) next.set(key, { ...previous, loading: false });
          return next;
        });
      }
    },
    [branchPages, branchRequest, projectSlug, session.sessionId],
  );

  const toggleBranch = useCallback(
    (toolUseId: string) => {
      const opening = !expandedBranchToolIds.has(toolUseId);
      setExpandedBranchToolIds((current) => {
        const next = new Set(current);
        if (opening) next.add(toolUseId);
        else next.delete(toolUseId);
        return next;
      });
      if (opening) {
        for (const thread of subagents.filter((candidate) => threadToolId(candidate) === toolUseId)) {
          const page = branchPages.get(branchKey(thread));
          if (!page || page.messages.length === 0) void loadBranchPage(thread, false);
        }
      }
    },
    [branchPages, expandedBranchToolIds, loadBranchPage, subagents],
  );

  const loadMoreBranch = useCallback(
    (key: string) => {
      const thread = subagents.find((candidate) => branchKey(candidate) === key);
      if (thread) void loadBranchPage(thread, true);
    },
    [loadBranchPage, subagents],
  );

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

  const branchCountsByToolId = useMemo(() => {
    const result = new Map<string, { threads: number; messages: number; loaded: number; loading: boolean }>();
    for (const thread of subagents) {
      const toolId = threadToolId(thread);
      const page = branchPages.get(branchKey(thread));
      const current = result.get(toolId) ?? { threads: 0, messages: 0, loaded: 0, loading: false };
      current.threads += 1;
      current.messages += page?.total ?? thread.messageCount;
      current.loaded += page?.messages.length ?? 0;
      current.loading ||= page?.loading ?? false;
      result.set(toolId, current);
    }
    return result;
  }, [branchPages, subagents]);

  const embeddedMessages = useMemo(() => {
    if (debugMessages) return chatMessages;
    const result: ChatSessionMessage[] = [];
    const appendThread = (thread: SubagentListItem, toolId: string, parent: ChatSessionMessage) => {
      const key = branchKey(thread);
      const page = branchPages.get(key);
      if (!page) return;
      result.push(...page.messages.map((branchMessage) => ({ ...branchMessage, branchToolId: toolId })));
      if (page.hasMore && !page.loading) {
        result.push({
          timelineId: `branch-more:${key}:${page.offset + page.messages.length}`,
          uuid: `branch-more:${key}`,
          parentUuid: null,
          type: 'system',
          timestamp: page.messages.at(-1)?.timestamp ?? parent.timestamp,
          sessionId: session.sessionId,
          content: `Load more · ${page.messages.length}/${page.total}`,
          agentId: thread.agentId,
          isSidechain: true,
          branchKey: key,
          branchToolId: toolId,
          systemSubtype: 'subagent_load_more',
        });
      }
    };
    for (const message of chatMessages) {
      result.push(message);
      const toolId = message.type === 'tool_use' ? message.toolUse?.toolId : undefined;
      if (!toolId || !expandedBranchToolIds.has(toolId)) continue;
      for (const thread of subagents.filter((candidate) => threadToolId(candidate) === toolId)) {
        appendThread(thread, toolId, message);
      }
    }
    // Keep orphaned/legacy sidecars accessible instead of silently dropping
    // them when no trustworthy Task result or ordinal anchor exists.
    for (const thread of subagents.filter((candidate) => !candidate.spawnToolId)) {
      const toolId = threadToolId(thread);
      const key = branchKey(thread);
      const anchor: ChatSessionMessage = {
        timelineId: `branch-anchor:${key}`,
        uuid: `branch-anchor:${key}`,
        parentUuid: null,
        type: 'tool_use',
        timestamp: chatMessages.at(-1)?.timestamp ?? session.lastUpdate,
        sessionId: session.sessionId,
        toolUse: {
          toolName: 'Agent',
          toolId,
          input: { description: `${thread.agentType} · unlinked transcript`, subagent_type: thread.agentType },
        },
      };
      result.push(anchor);
      if (expandedBranchToolIds.has(toolId)) appendThread(thread, toolId, anchor);
    }
    return result;
  }, [
    branchPages,
    chatMessages,
    debugMessages,
    expandedBranchToolIds,
    session.lastUpdate,
    session.sessionId,
    subagents,
  ]);

  const filterTotalCount = facets?.total ?? debugTimeline.length;

  // Reset when session changes
  useEffect(() => {
    liveRefreshSeqRef.current += 1;
    isPrependingRef.current = false;
    shouldScrollToBottomRef.current = false;
    scrollToBottomBehaviorRef.current = 'auto';
    tailScrollInProgressRef.current = false;
    if (tailScrollEndTimerRef.current) clearTimeout(tailScrollEndTimerRef.current);
    tailScrollEndTimerRef.current = null;
    prevScrollHeightRef.current = 0;
    loadMoreInFlightRef.current = false;
    nearBottomRef.current = true;
    cursorRef.current = undefined;
    totalRef.current = 0;
    cacheRef.current.clear();
    activeQueryKeyRef.current = '';
    setFacets(null);
    setSubagents([]);
    setExpandedBranchToolIds(new Set());
    setBranchPages(new Map());
    setPendingBranchScroll(null);
    consumedBranchTargetRef.current = null;
    setPendingNew(0);
    setError(null);
    resetFilters();
  }, [session.sessionId, resetFilters]);

  // Register the sole visible transcript in the UtilityProcess before using
  // its snapshot. Later commits are source-aware stream notifications, so
  // inactive sessions never trigger detailed timeline pulls.
  useEffect(() => {
    if (debugMessages) return;
    let cancelled = false;
    let openedStreamId: string | null = null;
    const queryKeyAtOpen = queryKey;
    void window.spaghetti
      .openSessionStream(projectSlug, session.sessionId, { sourceId, limit: PAGE_SIZE })
      .then((snapshot) => {
        openedStreamId = snapshot.streamId;
        if (cancelled) {
          void window.spaghetti.closeSessionStream(snapshot.streamId);
          return;
        }
        activeStreamIdRef.current = snapshot.streamId;
        lastActiveRevisionRef.current = 0;
        if (activeQueryKeyRef.current !== queryKeyAtOpen) return;
        shouldScrollToBottomRef.current = true;
        scrollToBottomBehaviorRef.current = 'auto';
        setTimeline(snapshot.page.messages as ChatSessionMessage[]);
        setPageMeta({ total: snapshot.page.total, hasMore: snapshot.page.hasMore });
        cursorRef.current = snapshot.page.nextCursor;
        totalRef.current = snapshot.page.total;
        setFacets(snapshot.facets);
        setSubagents(snapshot.subagents);
        setLoading(false);
        const pending = preOpenActiveChangeRef.current;
        preOpenActiveChangeRef.current = null;
        if (pending?.streamId === snapshot.streamId) {
          lastActiveRevisionRef.current = pending.revision;
          requestLiveRefreshRef.current(
            pending.reason === 'subagent' || pending.reason === 'reset',
            pending.reason === 'reset',
          );
        }
      })
      .catch((e: unknown) => {
        if (!cancelled) setError(String(e));
      });
    return () => {
      cancelled = true;
      activeStreamIdRef.current = null;
      lastActiveRevisionRef.current = 0;
      preOpenActiveChangeRef.current = null;
      if (openedStreamId) void window.spaghetti.closeSessionStream(openedStreamId);
    };
  }, [debugMessages, projectSlug, session.sessionId, sourceId]);

  useEffect(() => {
    if (debugMessages) return;
    let cancelled = false;
    void window.spaghetti
      .getSessionSubagents(projectSlug, session.sessionId, { sourceId, includeNested: true })
      .then((threads) => {
        if (!cancelled) setSubagents(threads);
      })
      .catch((e: unknown) => {
        if (!cancelled) setError(String(e));
      });
    return () => {
      cancelled = true;
    };
  }, [debugMessages, projectSlug, session.sessionId, sourceId]);

  // Global FTS hits can land inside a subagent transcript. Open the owning
  // Task/Agent branch and fetch a small window containing the matched row.
  useEffect(() => {
    if (!initialBranchTarget || debugMessages || subagents.length === 0) return;
    const identity = JSON.stringify([
      session.sessionId,
      initialBranchTarget.workflowId ?? '',
      initialBranchTarget.agentId,
      initialBranchTarget.agentTimelineIndex ?? 0,
    ]);
    if (consumedBranchTargetRef.current === identity) return;
    const thread = subagents.find(
      (candidate) =>
        candidate.agentId === initialBranchTarget.agentId &&
        (initialBranchTarget.workflowId === undefined || candidate.workflowId === initialBranchTarget.workflowId),
    );
    if (!thread) return;
    consumedBranchTargetRef.current = identity;
    const toolUseId = initialBranchTarget.spawnToolId ?? threadToolId(thread);
    setExpandedBranchToolIds((current) => new Set(current).add(toolUseId));
    const offset = Math.max(0, (initialBranchTarget.agentTimelineIndex ?? 0) - 12);
    const key = branchKey(thread);
    setBranchPages((pages) => {
      const next = new Map(pages);
      next.set(key, { messages: [], total: thread.messageCount, offset, hasMore: false, loading: true });
      return next;
    });
    void window.spaghetti
      .getSubagentTimeline(projectSlug, session.sessionId, thread.agentId, {
        ...branchRequest,
        workflowId: thread.workflowId,
        offset,
      })
      .then((page) => {
        setBranchPages((pages) => {
          const next = new Map(pages);
          next.set(key, {
            messages: page.messages as ChatSessionMessage[],
            total: page.total,
            offset: page.offset,
            hasMore: page.hasMore,
            loading: false,
          });
          return next;
        });
      })
      .catch((e: unknown) => {
        setError(String(e));
        setBranchPages((pages) => {
          const next = new Map(pages);
          const previous = next.get(key);
          if (previous) next.set(key, { ...previous, loading: false });
          return next;
        });
      })
      .finally(() => onInitialBranchTargetConsumed?.());

    if (thread.spawnToolId) {
      void (async () => {
        let before: number | undefined;
        let accumulated: ChatSessionMessage[] = [];
        // Search navigation must also reveal an old parent Task that is not in
        // the newest page. Walk in large DB pages, stopping as soon as its
        // normalized tool id is present.
        for (let pageNumber = 0; pageNumber < 40; pageNumber++) {
          const parentPage = await window.spaghetti.getSessionTimeline(projectSlug, session.sessionId, {
            sourceId,
            limit: 500,
            before,
          });
          const parentMessages = parentPage.messages as ChatSessionMessage[];
          accumulated = [...parentMessages, ...accumulated];
          const found = parentMessages.some(
            (message) => message.type === 'tool_use' && message.toolUse?.toolId === toolUseId,
          );
          if (found || !parentPage.hasMore || parentPage.nextCursor == null) {
            setTimeline(accumulated);
            setPageMeta({ total: parentPage.total, hasMore: parentPage.hasMore });
            cursorRef.current = parentPage.nextCursor;
            totalRef.current = parentPage.total;
            setPendingBranchScroll(toolUseId);
            return;
          }
          before = parentPage.nextCursor;
        }
      })().catch((e: unknown) => setError(String(e)));
    } else {
      setPendingBranchScroll(toolUseId);
    }
  }, [
    branchRequest,
    debugMessages,
    initialBranchTarget,
    onInitialBranchTargetConsumed,
    projectSlug,
    session.sessionId,
    sourceId,
    subagents,
  ]);

  // A branch page is filtered independently from its parent page. Collapse
  // and invalidate loaded branch rows whenever the active DB filter changes.
  useEffect(() => {
    setExpandedBranchToolIds(new Set());
    setBranchPages(new Map());
  }, [queryKey]);

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
        const page = await window.spaghetti.getSessionTimeline(projectSlug, session.sessionId, timelineRequest);
        if (cancelled) return;
        shouldScrollToBottomRef.current = true;
        scrollToBottomBehaviorRef.current = 'auto';
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
  }, [projectSlug, session.sessionId, queryKey, debugMessages, debugTimeline]);

  const appendLiveTail = useCallback(
    async (refreshSubagents = false, reset = false) => {
      if (debugMessages) return;
      const refreshSeq = ++liveRefreshSeqRef.current;
      try {
        const [nextFacets, page] = await Promise.all([
          window.spaghetti.getSessionTimelineFacets(projectSlug, session.sessionId, { sourceId }),
          window.spaghetti.getSessionTimeline(projectSlug, session.sessionId, timelineRequest),
        ]);
        if (refreshSeq !== liveRefreshSeqRef.current) return;

        cacheRef.current.clear();
        setFacets(nextFacets);
        const delta = Math.max(0, page.total - totalRef.current);
        totalRef.current = page.total;
        if (nearBottomRef.current) {
          shouldScrollToBottomRef.current = true;
          scrollToBottomBehaviorRef.current = 'smooth';
          setTimeline((current) =>
            reset
              ? (page.messages as ChatSessionMessage[])
              : reconcileTimeline(current, page.messages as ChatSessionMessage[]),
          );
          setPageMeta({ total: page.total, hasMore: page.hasMore });
          cursorRef.current = page.nextCursor;
          setPendingNew(0);
        } else {
          setTimeline((current) =>
            reset
              ? (page.messages as ChatSessionMessage[])
              : reconcileTimeline(current, page.messages as ChatSessionMessage[]),
          );
          setPageMeta((current) => ({ total: page.total, hasMore: current?.hasMore ?? page.hasMore }));
          if (delta > 0) setPendingNew((count) => count + delta);
        }
        setError((current) => (current?.startsWith('Live refresh failed:') ? null : current));

        if (!refreshSubagents) return;
        const threads = await window.spaghetti.getSessionSubagents(projectSlug, session.sessionId, {
          sourceId,
          includeNested: true,
        });
        if (refreshSeq !== liveRefreshSeqRef.current) return;
        setSubagents(threads);
        for (const thread of threads) {
          if (expandedBranchToolIds.has(threadToolId(thread))) {
            void loadBranchPage(thread, false);
          }
        }
      } catch (e: unknown) {
        if (refreshSeq === liveRefreshSeqRef.current) {
          const message = e instanceof Error ? e.message : String(e);
          setError(`Live refresh failed: ${message}`);
        }
      }
    },
    [debugMessages, expandedBranchToolIds, loadBranchPage, projectSlug, session.sessionId, sourceId, timelineRequest],
  );

  requestLiveRefreshRef.current = (refreshSubagents, reset) => {
    if (refreshSubagents) liveNeedsSubagentRefreshRef.current = true;
    if (reset) liveResetPendingRef.current = true;
    if (liveTimerRef.current) clearTimeout(liveTimerRef.current);
    liveTimerRef.current = setTimeout(() => {
      const shouldRefreshSubagents = liveNeedsSubagentRefreshRef.current;
      const shouldReset = liveResetPendingRef.current;
      liveNeedsSubagentRefreshRef.current = false;
      liveResetPendingRef.current = false;
      void appendLiveTail(shouldRefreshSubagents, shouldReset);
    }, LIVE_DEBOUNCE_MS);
  };

  useEffect(() => {
    if (debugMessages) return;
    const unsub = window.spaghetti.onActiveSessionChange((change: ActiveSessionChange) => {
      if (change.streamId !== activeStreamIdRef.current) {
        if (
          !activeStreamIdRef.current &&
          change.sourceId === sourceId &&
          change.projectSlug === projectSlug &&
          change.sessionId === session.sessionId &&
          change.revision > (preOpenActiveChangeRef.current?.revision ?? 0)
        ) {
          preOpenActiveChangeRef.current = change;
        }
        return;
      }
      if (change.revision <= lastActiveRevisionRef.current) return;
      lastActiveRevisionRef.current = change.revision;
      requestLiveRefreshRef.current(
        change.reason === 'subagent' || change.reason === 'reset',
        change.reason === 'reset',
      );
    });

    return () => {
      unsub();
      liveRefreshSeqRef.current += 1;
      liveNeedsSubagentRefreshRef.current = false;
      liveResetPendingRef.current = false;
      preOpenActiveChangeRef.current = null;
      if (liveTimerRef.current) clearTimeout(liveTimerRef.current);
    };
  }, [debugMessages, projectSlug, session.sessionId, sourceId]);

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
      const behavior =
        scrollToBottomBehaviorRef.current === 'smooth' && !window.matchMedia('(prefers-reduced-motion: reduce)').matches
          ? 'smooth'
          : 'auto';
      container.scrollTo({ top: container.scrollHeight, behavior });
      tailScrollInProgressRef.current = behavior === 'smooth';
      if (tailScrollEndTimerRef.current) clearTimeout(tailScrollEndTimerRef.current);
      tailScrollEndTimerRef.current =
        behavior === 'smooth'
          ? setTimeout(() => {
              tailScrollInProgressRef.current = false;
              tailScrollEndTimerRef.current = null;
              const distance = container.scrollHeight - container.scrollTop - container.clientHeight;
              nearBottomRef.current = distance < NEAR_BOTTOM_PX;
            }, 600)
          : null;
      shouldScrollToBottomRef.current = false;
      scrollToBottomBehaviorRef.current = 'auto';
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
      nearBottomRef.current =
        tailScrollInProgressRef.current || scrollHeight - scrollTop - clientHeight < NEAR_BOTTOM_PX;

      if (nearBottomRef.current && pendingNew > 0) {
        setPendingNew(0);
      }

      if (scrollTop < NEAR_TOP_PX && !loadingMore && pageMeta?.hasMore && timeline.length > 0) {
        void loadMoreMessages();
      }
    },
    [loadingMore, pageMeta?.hasMore, timeline.length, loadMoreMessages, pendingNew],
  );

  const cancelTailScroll = useCallback(() => {
    if (!tailScrollInProgressRef.current) return;
    const container = scrollContainerRef.current;
    tailScrollInProgressRef.current = false;
    if (tailScrollEndTimerRef.current) clearTimeout(tailScrollEndTimerRef.current);
    tailScrollEndTimerRef.current = null;
    if (!container) return;
    container.scrollTo({ top: container.scrollTop, behavior: 'auto' });
    nearBottomRef.current = container.scrollHeight - container.scrollTop - container.clientHeight < NEAR_BOTTOM_PX;
  }, []);

  useEffect(
    () => () => {
      if (tailScrollEndTimerRef.current) clearTimeout(tailScrollEndTimerRef.current);
    },
    [],
  );

  const jumpToLatest = useCallback(() => {
    nearBottomRef.current = true;
    setPendingNew(0);
    void appendLiveTail(true);
  }, [appendLiveTail]);

  const consumeBranchScrollTarget = useCallback(() => {
    setPendingBranchScroll(null);
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
          <LiveDot active={isLive} />
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
        <div
          ref={scrollContainerRef}
          onScroll={handleScroll}
          onWheel={cancelTailScroll}
          onPointerDown={cancelTailScroll}
          onTouchStart={cancelTailScroll}
          className="h-full overflow-y-auto scrollbar-hide"
        >
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
          ) : embeddedMessages.length === 0 ? (
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

                <ArchiveTranscript
                  messages={embeddedMessages}
                  isDark={isDark}
                  expandedBranchToolIds={debugMessages ? undefined : expandedBranchToolIds}
                  branchCountsByToolId={debugMessages ? undefined : branchCountsByToolId}
                  onToggleBranch={debugMessages ? undefined : toggleBranch}
                  onLoadMoreBranch={debugMessages ? undefined : loadMoreBranch}
                  scrollElementRef={scrollContainerRef}
                  scrollToBranchToolId={pendingBranchScroll}
                  onScrollTargetConsumed={consumeBranchScrollTarget}
                />
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

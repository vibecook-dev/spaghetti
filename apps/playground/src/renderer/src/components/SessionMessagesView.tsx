/**
 * Session message browser — timeline chat UI with ProjectPage-style scroll UX:
 *
 * - Initial load: last PAGE_SIZE messages, then scroll to bottom (useLayoutEffect)
 * - Auto-load older history when user scrolls near the top (scrollTop < 50)
 * - Prepend + scroll-height diff so the viewport does not jump
 * - Top spinner + "scroll up" / "beginning" affordances
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
import type { MessagePage, SessionListItem } from '@vibecook/spaghetti-sdk';
import { SourceBadge } from './SourceBadge.js';
import { flattenPrompt, formatDuration, formatNumber, formatRelativeTime, formatTokenUsage } from '../lib/format.js';

type AnyMsg = Record<string, unknown>;

/** Messages per page (same ballpark as ProjectPage PAGE_SIZE). */
const PAGE_SIZE = 30;

/** Brief delay so the top spinner is perceptible on fast IPC. */
const LOADING_DELAY_MS = 280;

/** Distance from top (px) that triggers auto-load of older messages. */
const NEAR_TOP_PX = 80;

export interface SessionMessagesViewProps {
  projectSlug: string;
  sourceId: string;
  session: SessionListItem;
  sessionIndex: number;
  onBack: () => void;
}

export function SessionMessagesView({
  projectSlug,
  sourceId,
  session,
  sessionIndex,
  onBack,
}: SessionMessagesViewProps) {
  const [rawMessages, setRawMessages] = useState<AnyMsg[]>([]);
  const [pageMeta, setPageMeta] = useState<Pick<MessagePage, 'total' | 'hasMore'> | null>(null);
  const [loading, setLoading] = useState(true);
  const [loadingMore, setLoadingMore] = useState(false);
  const [error, setError] = useState<string | null>(null);

  /** Offset into the full message list (0 = start of transcript). */
  const offsetRef = useRef(0);
  const scrollContainerRef = useRef<HTMLDivElement>(null);
  /** Scroll height before a prepend; used to restore position. */
  const prevScrollHeightRef = useRef(0);
  const isPrependingRef = useRef(false);
  const shouldScrollToBottomRef = useRef(false);
  /** Guards concurrent auto-load triggers from scroll spam. */
  const loadMoreInFlightRef = useRef(false);

  // Reset scroll flags when session changes
  useEffect(() => {
    isPrependingRef.current = false;
    shouldScrollToBottomRef.current = false;
    prevScrollHeightRef.current = 0;
    loadMoreInFlightRef.current = false;
    offsetRef.current = 0;
  }, [session.sessionId]);

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

  // ── Scroll restoration (before paint — ProjectPage pattern) ───────────
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
      const { scrollTop } = e.currentTarget;
      if (scrollTop < NEAR_TOP_PX && !loadingMore && pageMeta?.hasMore && rawMessages.length > 0) {
        void loadMoreMessages();
      }
    },
    [loadingMore, pageMeta?.hasMore, rawMessages.length, loadMoreMessages],
  );

  const timeline: ChatSessionMessage[] = useMemo(() => transformRawMessagesToTimeline(rawMessages), [rawMessages]);

  const prompt = flattenPrompt(session.firstPrompt || session.summary, 80);
  const tok = formatTokenUsage(session.tokenUsage, session.sourceId, session.tokensEstimated);
  const hasMore = pageMeta?.hasMore ?? false;

  return (
    <div className="flex flex-col h-full min-h-0 bg-[#0a0a0a] text-[#f2f2f2]">
      {/* Header */}
      <div className="shrink-0 px-4 py-2.5 border-b border-white/10 bg-white/[0.02] flex items-start gap-3">
        <button
          type="button"
          onClick={onBack}
          className="mt-0.5 text-[11px] text-white/45 hover:text-white/80 border border-white/12 rounded px-2 py-1 cursor-pointer bg-transparent"
        >
          ← Sessions
        </button>
        <div className="min-w-0 flex-1">
          <div className="flex items-center gap-2 flex-wrap">
            <span className="text-xs font-medium text-white/85">#{sessionIndex + 1}</span>
            {session.gitBranch ? (
              <span className="text-[11px] font-mono text-amber-200/70">{session.gitBranch}</span>
            ) : null}
            <span className="text-[10px] font-mono text-white/30">{session.sessionId.slice(0, 8)}</span>
            <SourceBadge sourceId={session.sourceId} />
            {pageMeta && (
              <span className="text-[10px] text-white/35 font-mono ml-auto">
                {rawMessages.length}/{pageMeta.total} msgs
              </span>
            )}
          </div>
          <div className="text-[11px] italic text-white/40 truncate mt-0.5">
            {prompt ? `"${prompt}"` : '(no prompt)'}
          </div>
          <div className="text-[10px] text-white/35 font-mono mt-1">
            {formatNumber(session.messageCount)} msgs · {tok} tokens · {formatDuration(session.lifespanMs)} ·{' '}
            {formatRelativeTime(session.lastUpdate)}
          </div>
        </div>
      </div>

      {error && (
        <div className="shrink-0 px-4 py-2 text-[11px] text-red-300/90 border-b border-red-500/20 bg-red-500/[0.04]">
          {error}
        </div>
      )}

      {/* Timeline scroll container */}
      <div ref={scrollContainerRef} onScroll={handleScroll} className="flex-1 min-h-0 overflow-y-auto">
        {loading && rawMessages.length === 0 ? (
          <div className="flex items-center justify-center h-full gap-3 py-12">
            <Spinner />
            <span className="text-xs text-white/40">Loading messages…</span>
          </div>
        ) : timeline.length === 0 ? (
          <div className="flex flex-col items-center justify-center h-full opacity-50 py-12">
            <p className="text-sm">No messages in this session</p>
          </div>
        ) : (
          <div className="py-3 max-w-3xl mx-auto w-full">
            {/* Top: loading older / beginning / scroll hint */}
            {loadingMore && (
              <div className="flex items-center justify-center py-6 gap-3">
                <Spinner />
                <span className="text-sm text-white/50">Loading older messages…</span>
              </div>
            )}

            {!hasMore && timeline.length > 0 && (
              <div className="flex items-center justify-center py-4 text-xs text-white/25">
                — Beginning of conversation —
              </div>
            )}

            {hasMore && !loadingMore && (
              <div className="flex items-center justify-center py-3 text-xs text-white/35">
                ↑ Scroll up for older messages
              </div>
            )}

            {timeline.map((msg, i) => {
              const prev = i > 0 ? timeline[i - 1] : undefined;
              const next = i < timeline.length - 1 ? timeline[i + 1] : undefined;
              const isLast = i === timeline.length - 1;
              const connectToNext = !!(next && isTimelineType(next.type));
              const showSep = shouldShowTimestamp(msg.timestamp, prev?.timestamp ?? null);

              // Task tool → next sidechain (same heuristic as ProjectPage)
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
    </div>
  );
}

function Spinner() {
  return <div className="animate-spin rounded-full h-5 w-5 border-2 border-white/20 border-t-orange-400" aria-hidden />;
}

function delay(ms: number): Promise<void> {
  return new Promise((r) => setTimeout(r, ms));
}

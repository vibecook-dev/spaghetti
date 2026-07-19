/**
 * MessagesView — Line-based scrollable transcript with filter chips
 *
 * Items (messages, tool calls, thinking, system) are flattened to a single
 * line stream so scrolling advances one terminal row at a time. Items can
 * straddle the viewport boundary, which keeps the viewport densely filled
 * even when individual items are taller than it.
 *
 * Selection is still item-based (↑/↓); scrollOffset is computed from the
 * selected item's line range to keep it visible.
 */

import React, { useState, useMemo, useCallback, useEffect, useRef } from 'react';
import { Box, Text, useInput, useStdout } from 'ink';
import type { ProjectListItem, SessionListItem, SessionMessage } from '@vibecook/spaghetti-sdk';
import { useViewNav } from './context.js';
import { HRule } from './chrome.js';
import { useApi } from './shell.js';
import { formatTokens, formatRelativeTime, formatDuration } from '../lib/format.js';
import { theme } from '../lib/color.js';
import { renderMarkdownText } from '../lib/message-render.js';
import { adaptMessagesForDisplay } from '../lib/source-messages.js';
import {
  buildDisplayItems,
  applyDisplayFilters,
  createDefaultFilters,
  FILTER_CATEGORIES,
  assistantFilterLabel,
  getToolCategory,
  TOOL_CATEGORY_COLORS,
  toolInputSummary,
  toolResultSummary,
} from './display-items.js';
import type { DisplayItem, FilterState } from './display-items.js';
import type { ViewEntry } from './types.js';
import { DetailView } from './detail-view.js';
import pc from 'picocolors';

// ─── Constants ─────────────────────────────────────────────────────────

const PAGE_SIZE = 50;
const LOAD_MORE_THRESHOLD = 5;
const LINE_CAP = 20;

// ─── 256-Color Helpers ─────────────────────────────────────────────────

const RESET = '\x1b[0m';
const fg256 = (n: number) => `\x1b[38;5;${n}m`;
const BOLD = '\x1b[1m';

const COLORS = {
  userLabel: 79,
  userLabelDim: 36,
  userText: 36,
  userTextSelected: 79,
  /** Default assistant accent (Claude peach); Codex magenta; Grok amber. */
  claudeLabel: 216,
  claudeLabelDim: 173,
  codexLabel: 213,
  codexLabelDim: 170,
  grokLabel: 220,
  grokLabelDim: 178,
  assistantLabel: 116,
  assistantLabelDim: 73,
  timestamp: 248,
};

/** Accent colors for the assistant header bar/label by agent source. */
function assistantColors(sourceId: string): { bright: number; dim: number } {
  switch (sourceId) {
    case 'codex':
      return { bright: COLORS.codexLabel, dim: COLORS.codexLabelDim };
    case 'grok':
      return { bright: COLORS.grokLabel, dim: COLORS.grokLabelDim };
    case 'claude-code':
      return { bright: COLORS.claudeLabel, dim: COLORS.claudeLabelDim };
    default:
      return { bright: COLORS.assistantLabel, dim: COLORS.assistantLabelDim };
  }
}

function stripAnsi(s: string): string {
  // eslint-disable-next-line no-control-regex
  return s.replace(/\x1b\[[0-9;]*m/g, '');
}

// ─── Body-line builders ────────────────────────────────────────────────

function extractUserText(msg: SessionMessage): string {
  const content = (msg as any).message.content;
  if (typeof content === 'string') return content;
  if (Array.isArray(content)) {
    const t = content.find((b: any) => b.type === 'text');
    if (t && 'text' in t) return t.text;
  }
  return '';
}

function capLines(lines: string[], cap: number): string[] {
  if (lines.length <= cap) return lines;
  const extra = lines.length - cap + 1;
  const kept = lines.slice(0, cap - 1);
  kept.push(pc.dim(`\u2026 ${extra} more lines`));
  return kept;
}

function buildUserBodyLines(msg: SessionMessage, cols: number, cap: number): string[] {
  const text = extractUserText(msg);
  const innerWidth = Math.max(cols - 4, 10);
  const wrapped: string[] = [];
  for (const line of text.split('\n')) {
    if (line.length === 0) {
      wrapped.push('');
      continue;
    }
    let rest = line;
    while (rest.length > innerWidth) {
      wrapped.push(rest.slice(0, innerWidth));
      rest = rest.slice(innerWidth);
    }
    wrapped.push(rest);
  }
  return capLines(wrapped, cap);
}

function buildAssistantBodyLines(msg: SessionMessage, cols: number, cap: number): string[] {
  const blocks = (msg as any).message.content || [];
  const textBlocks = blocks.filter((b: any) => b.type === 'text');
  const text = textBlocks.map((b: any) => b.text).join('\n\n');
  if (!text.trim()) return [];
  const mdWidth = Math.max(cols - 4, 10);
  return capLines(renderMarkdownText(text, mdWidth), cap);
}

// ─── Filter Chips ──────────────────────────────────────────────────────

function buildFilterChips(filters: FilterState, sourceId: string): string {
  return FILTER_CATEGORIES.map((cat) => {
    const label = cat.key === '2' ? assistantFilterLabel(sourceId) : cat.label;
    const on = filters[cat.key];
    if (on) {
      return `${pc.white(cat.key)}${pc.dim(':')}${cat.color(label)}`;
    } else {
      return `${pc.dim(cat.key)}${pc.dim(':')}${pc.strikethrough(pc.dim(label))}`;
    }
  }).join(pc.dim('  '));
}

// ─── Line builders (item → terminal rows as strings) ──────────────────

function buildAccentBar(cols: number, color: number): string {
  // Zero-width panes (headless ptys, extreme resizes) report cols <= 0;
  // String.repeat throws on negative counts.
  return `${fg256(color)}${'\u2500'.repeat(Math.max(0, cols))}${RESET}`;
}

function buildUserLines(msg: SessionMessage, bodyLines: string[], cols: number, selected: boolean): string[] {
  const barColor = selected ? COLORS.userLabel : COLORS.userLabelDim;
  const labelColor = selected ? COLORS.userLabel : COLORS.userLabelDim;
  const textColor = selected ? COLORS.userTextSelected : COLORS.userText;
  const timestamp = 'timestamp' in msg && (msg as any).timestamp ? formatRelativeTime((msg as any).timestamp) : '';

  const labelVis = 'USER';
  const rightVis = `${timestamp}  ${labelVis}`;
  const headerPad = Math.max(1, cols - rightVis.length - 1);
  const header = `${' '.repeat(headerPad)}${fg256(COLORS.timestamp)}${timestamp}  ${selected ? BOLD : ''}${fg256(labelColor)}${labelVis}${RESET}`;

  const out: string[] = [buildAccentBar(cols, barColor), header];
  for (const line of bodyLines) {
    const visLen = stripAnsi(line).length;
    const pad = Math.max(1, cols - visLen - 1);
    out.push(`${' '.repeat(pad)}${fg256(textColor)}${line}${RESET}`);
  }
  out.push('');
  return out;
}

function buildAssistantLines(
  msg: SessionMessage,
  bodyLines: string[],
  cols: number,
  selected: boolean,
  sourceId: string,
): string[] {
  const colors = assistantColors(sourceId);
  const barColor = selected ? colors.bright : colors.dim;
  const labelColor = selected ? colors.bright : colors.dim;
  const timestamp = 'timestamp' in msg && (msg as any).timestamp ? formatRelativeTime((msg as any).timestamp) : '';
  // Source-aware label (was hardcoded CLAUDE — wrong for Codex / multi-agent).
  const label = theme.assistantName(sourceId);
  const header = `${selected ? BOLD : ''}${fg256(labelColor)}${label}${RESET}  ${fg256(COLORS.timestamp)}${timestamp}${RESET}`;

  const out: string[] = [buildAccentBar(cols, barColor), header];
  for (const line of bodyLines) {
    out.push(`  ${line}`);
  }
  out.push('');
  return out;
}

function buildToolCallLine(item: DisplayItem & { kind: 'tool-call' }, cols: number, selected: boolean): string {
  const cat = getToolCategory(item.toolName);
  const colorFn = TOOL_CATEGORY_COLORS[cat];
  const prefix = selected ? colorFn('\u258E') : ' ';

  const catLabel = pc.dim(cat);
  const badge = selected ? pc.bold(colorFn(item.toolName)) : colorFn(item.toolName);
  const input = toolInputSummary(item.toolName, item.toolInput);

  let status: string;
  let resultHint: string;
  if (item.result) {
    if (item.result.isError) {
      status = pc.red('\u2717');
      const errText = item.result.content.replace(/\n/g, ' ').slice(0, 40);
      resultHint = pc.red(pc.dim(errText));
    } else {
      status = pc.green('\u2713');
      const summary = toolResultSummary(item.result.content);
      resultHint = pc.dim(summary);
    }
  } else {
    status = pc.dim('\u00B7');
    resultHint = '';
  }

  const fixedWidth = cat.length + item.toolName.length + 10;
  const maxInputLen = Math.max(cols - fixedWidth - 20, 10);
  const inputText = input
    ? selected
      ? pc.white(input.length > maxInputLen ? input.slice(0, maxInputLen - 1) + '\u2026' : input)
      : pc.dim(input.length > maxInputLen ? input.slice(0, maxInputLen - 1) + '\u2026' : input)
    : '';

  return `${prefix} ${catLabel} ${badge}  ${inputText}  ${status} ${resultHint}`;
}

function buildThinkingLine(item: DisplayItem & { kind: 'thinking' }, cols: number, selected: boolean): string {
  const prefix = selected ? pc.magenta('\u258E') : ' ';
  const dots = selected ? pc.magenta('\u00B7\u00B7\u00B7') : pc.dim('\u00B7\u00B7\u00B7');
  const label = selected ? pc.white('thinking') : pc.dim('thinking');

  if (item.redacted) {
    return `${prefix} ${dots} ${label}  ${pc.dim('(redacted)')}`;
  }

  const tokensLabel = item.tokenEstimate > 0 ? pc.dim(`~${formatTokens(item.tokenEstimate)}`) : '';
  const firstLine = item.content.split('\n').find((l) => l.trim().length > 0) || '';
  const usedWidth = 22 + (item.tokenEstimate > 0 ? 8 : 0);
  const maxPreview = Math.max(cols - usedWidth, 20);
  const previewText =
    firstLine.trim().length > maxPreview ? firstLine.trim().slice(0, maxPreview - 1) + '\u2026' : firstLine.trim();
  const preview = selected ? pc.italic(pc.dim(previewText)) : pc.dim(pc.italic(previewText));

  return `${prefix} ${dots} ${label}  ${tokensLabel}  ${preview}`;
}

function buildSystemLine(msg: SessionMessage, cols: number, selected: boolean): string {
  const prefix = selected ? pc.dim('\u258E') : ' ';
  let symbol: string;
  let text: string;

  switch (msg.type) {
    case 'system': {
      const subtype = 'subtype' in msg ? (msg as any).subtype : '';
      if (subtype === 'turn_duration') {
        symbol = selected ? pc.white('\u23F1') : pc.dim('\u23F1');
        text = formatDuration((msg as any).durationMs || 0);
      } else if (subtype === 'api_error') {
        symbol = selected ? pc.red('\u26A0') : pc.dim('\u26A0');
        text = `api error (retry ${(msg as any).retryAttempt}/${(msg as any).maxRetries})`;
      } else if (subtype === 'compact_boundary') {
        symbol = selected ? pc.white('\u25C7') : pc.dim('\u25C7');
        const content = (msg as any).content || '';
        const maxLen = Math.max(cols - 16, 20);
        const contentText = content.replace(/\n/g, ' ');
        text = `compacted ${contentText.length > maxLen ? contentText.slice(0, maxLen - 1) + '\u2026' : contentText}`;
      } else if (subtype === 'stop_hook_summary') {
        symbol = selected ? pc.white('\u25A0') : pc.dim('\u25A0');
        text = `stop hook (${(msg as any).hookCount || 0} hooks)`;
      } else {
        symbol = selected ? pc.white('\u25C6') : pc.dim('\u25C6');
        const content =
          'content' in msg && typeof (msg as any).content === 'string'
            ? (msg as any).content.replace(/\n/g, ' ')
            : subtype || 'system';
        const maxLen = Math.max(cols - 8, 20);
        text = content.length > maxLen ? content.slice(0, maxLen - 1) + '\u2026' : content;
      }
      break;
    }
    case 'summary': {
      symbol = selected ? pc.white('\u00A7') : pc.dim('\u00A7');
      const summary = ((msg as any).summary || '').replace(/\n/g, ' ');
      const maxLen = Math.max(cols - 8, 20);
      text = pc.italic(summary.length > maxLen ? summary.slice(0, maxLen - 1) + '\u2026' : summary);
      break;
    }
    case 'progress': {
      symbol = selected ? pc.white('\u27F3') : pc.dim('\u27F3');
      const data = (msg as any).data;
      if (data?.type === 'bash_progress') {
        const maxLen = Math.max(cols - 14, 20);
        const output = (data.output || '').replace(/\n/g, ' ');
        text = `bash ${output.length > maxLen ? output.slice(0, maxLen - 1) + '\u2026' : output}`;
      } else if (data?.type === 'agent_progress') {
        text = `agent ${data.agentId || ''}`;
      } else if (data?.type === 'hook_progress') {
        text = `hook ${data.hookName || ''}`;
      } else if (data?.type === 'mcp_progress') {
        text = `mcp ${data.serverName || ''}/${data.toolName || ''}`;
      } else {
        text = data?.type || 'progress';
      }
      break;
    }
    default: {
      symbol = selected ? pc.white('\u00B7') : pc.dim('\u00B7');
      if (msg.type === 'saved_hook_context') {
        text = `${msg.type} ${(msg as any).hookName || ''}`;
      } else if (msg.type === 'queue-operation') {
        text = `queue ${(msg as any).operation || ''}`;
      } else if (msg.type === 'last-prompt') {
        text = `last-prompt`;
      } else if (msg.type === ('custom-title' as any)) {
        text = `title: ${(msg as any).customTitle || ''}`;
      } else if (msg.type === ('agent-name' as any)) {
        text = `agent: ${(msg as any).agentName || ''}`;
      } else {
        text = msg.type;
      }
      break;
    }
  }

  const styledText = selected ? pc.white(text) : pc.dim(text);
  return `${prefix} ${symbol} ${styledText}`;
}

function buildItemLines(
  item: DisplayItem,
  bodyLines: string[] | null,
  cols: number,
  selected: boolean,
  sourceId: string,
): string[] {
  if (item.kind === 'tool-call') return [buildToolCallLine(item, cols, selected)];
  if (item.kind === 'thinking') return [buildThinkingLine(item, cols, selected)];
  const msg = item.msg;
  if (msg.type === 'user') return buildUserLines(msg, bodyLines ?? [], cols, selected);
  if (msg.type === 'assistant') return buildAssistantLines(msg, bodyLines ?? [], cols, selected, sourceId);
  return [buildSystemLine(msg, cols, selected)];
}

function computeItemHeight(item: DisplayItem, bodyLines: string[] | null): number {
  if (item.kind === 'message' && bodyLines !== null) {
    return 3 + bodyLines.length;
  }
  return 1;
}

// ─── ScrollBar ────────────────────────────────────────────────────────

interface ScrollBarProps {
  viewportHeight: number;
  scrollLines: number;
  loadedLines: number;
  estimatedTotalLines: number;
  unloadedLinesBefore: number;
}

function ScrollBar({
  viewportHeight,
  scrollLines,
  loadedLines,
  estimatedTotalLines,
  unloadedLinesBefore,
}: ScrollBarProps): React.ReactElement {
  if (loadedLines === 0) {
    return (
      <Box flexDirection="column" width={1}>
        {Array.from({ length: viewportHeight }, (_, i) => (
          <Text key={i} dimColor>
            {' '}
          </Text>
        ))}
      </Box>
    );
  }

  const scrolledLines = scrollLines + unloadedLinesBefore;
  const totalForRatio = Math.max(estimatedTotalLines, viewportHeight);
  const ratio = totalForRatio > viewportHeight ? scrolledLines / (totalForRatio - viewportHeight) : 0;
  const thumbHeight = Math.max(1, Math.round((viewportHeight / totalForRatio) * viewportHeight));
  const thumbStart = Math.min(
    Math.max(0, Math.round(ratio * (viewportHeight - thumbHeight))),
    viewportHeight - thumbHeight,
  );

  const trackChars: string[] = [];
  for (let i = 0; i < viewportHeight; i++) {
    if (i >= thumbStart && i < thumbStart + thumbHeight) {
      trackChars.push('\x1b[38;5;245m\u2588\x1b[0m'); // █ gray
    } else {
      trackChars.push('\x1b[38;5;238m\u2502\x1b[0m'); // │ dim
    }
  }

  return (
    <Box flexDirection="column" width={1}>
      {trackChars.map((ch, i) => (
        <Text key={i}>{ch}</Text>
      ))}
    </Box>
  );
}

// ─── MessagesView ──────────────────────────────────────────────────────

export interface MessagesViewProps {
  project: ProjectListItem;
  session: SessionListItem;
  sessionIndex: number;
  /** Optional initial display-item index to scroll to (e.g. from search navigation) */
  initialIndex?: number;
}

export function MessagesView({
  project: _project,
  session,
  sessionIndex: _sessionIndex,
  initialIndex,
}: MessagesViewProps): React.ReactElement {
  const nav = useViewNav();
  const api = useApi();
  const { stdout } = useStdout();
  const cols = stdout?.columns ?? 80;
  const termRows = stdout?.rows ?? 24;

  const [filters, setFilters] = useState<FilterState>(createDefaultFilters);
  const [allMessages, setAllMessages] = useState<SessionMessage[]>([]);
  const [loadedOffsetLow, setLoadedOffsetLow] = useState(0);
  const [totalCount, setTotalCount] = useState(0);

  const sourceScope = useMemo(() => ({ sourceId: session.sourceId }), [session.sourceId]);

  // Initial load
  useEffect(() => {
    const probe = api.getSessionMessages(session.projectSlug, session.sessionId, 1, 0, sourceScope);
    const total = probe.total;
    setTotalCount(total);

    const startOffset = Math.max(0, total - PAGE_SIZE);
    const page = api.getSessionMessages(session.projectSlug, session.sessionId, PAGE_SIZE, startOffset, sourceScope);
    setAllMessages(adaptMessagesForDisplay(page.messages, session.sourceId));
    setLoadedOffsetLow(startOffset);
  }, [api, session.projectSlug, session.sessionId, session.sourceId, sourceScope]);

  const allDisplayItems = useMemo(() => buildDisplayItems(allMessages), [allMessages]);
  const displayItems = useMemo(() => applyDisplayFilters(allDisplayItems, filters), [allDisplayItems, filters]);

  const innerCols = cols - 1; // reserve 1 col for scrollbar

  // Body lines per message item (selection-independent; wrapping + markdown).
  const itemBodyLines = useMemo<(string[] | null)[]>(
    () =>
      displayItems.map((item) => {
        if (item.kind !== 'message') return null;
        if (item.msg.type === 'user') return buildUserBodyLines(item.msg, innerCols, LINE_CAP);
        if (item.msg.type === 'assistant') return buildAssistantBodyLines(item.msg, innerCols, LINE_CAP);
        return null;
      }),
    [displayItems, innerCols],
  );

  const itemHeights = useMemo<number[]>(
    () => displayItems.map((item, i) => computeItemHeight(item, itemBodyLines[i])),
    [displayItems, itemBodyLines],
  );

  // Cumulative line-start offset per item (e.g. item i starts at line itemLineStarts[i]).
  const itemLineStarts = useMemo<number[]>(() => {
    const starts = new Array<number>(itemHeights.length);
    let pos = 0;
    for (let i = 0; i < itemHeights.length; i++) {
      starts[i] = pos;
      pos += itemHeights[i];
    }
    return starts;
  }, [itemHeights]);

  const totalLoadedLines = useMemo<number>(() => itemHeights.reduce((a, b) => a + b, 0), [itemHeights]);

  // Flattened line stream (selection-independent). Selected item's lines are
  // patched in at render time so selection changes are cheap.
  const sourceId = session.sourceId;

  const allLinesUnselected = useMemo<string[]>(() => {
    const out: string[] = [];
    displayItems.forEach((item, idx) => {
      const lines = buildItemLines(item, itemBodyLines[idx], innerCols, false, sourceId);
      for (const text of lines) out.push(text);
    });
    return out;
  }, [displayItems, itemBodyLines, innerCols, sourceId]);

  const [selectedIndex, setSelectedIndex] = useState(initialIndex ?? 0);
  const [scrollLines, setScrollLines] = useState(0);

  const selectedItemLines = useMemo<string[]>(() => {
    const item = displayItems[selectedIndex];
    if (!item) return [];
    return buildItemLines(item, itemBodyLines[selectedIndex] ?? null, innerCols, true, sourceId);
  }, [displayItems, itemBodyLines, innerCols, selectedIndex, sourceId]);

  // Viewport budget in lines (filter chips + hrule + footer reserved).
  const viewportLines = Math.max(termRows - 8, 5);

  // Adjust scrollLines so the target item is visible. Scrolling policy:
  //   - Item taller than viewport: align its top to viewport top.
  //   - Item fits: if above viewport, align top; if below, align bottom.
  //   - Item already visible: no change.
  const adjustScroll = useCallback(
    (targetIndex: number) => {
      if (targetIndex < 0 || targetIndex >= itemHeights.length) return;
      const start = itemLineStarts[targetIndex];
      const end = start + itemHeights[targetIndex];
      setScrollLines((prev) => {
        const itemTaller = end - start > viewportLines;
        const fullyVisible = start >= prev && end <= prev + viewportLines;
        if (fullyVisible) return prev;
        if (start < prev) return start; // above viewport
        if (itemTaller) return start; // too tall: align top
        return Math.max(0, end - viewportLines); // below: align bottom
      });
    },
    [itemHeights, itemLineStarts, viewportLines],
  );

  const moveSelection = useCallback(
    (next: number) => {
      const clamped = Math.max(0, Math.min(next, displayItems.length - 1));
      setSelectedIndex(clamped);
      adjustScroll(clamped);
    },
    [displayItems.length, adjustScroll],
  );

  const moveUp = useCallback(() => moveSelection(selectedIndex - 1), [moveSelection, selectedIndex]);
  const moveDown = useCallback(() => moveSelection(selectedIndex + 1), [moveSelection, selectedIndex]);
  const jumpTo = useCallback((i: number) => moveSelection(i), [moveSelection]);

  // Clamp selection + reposition when displayItems shrinks (e.g. filter toggle).
  useEffect(() => {
    if (displayItems.length === 0) return;
    if (selectedIndex >= displayItems.length) {
      moveSelection(displayItems.length - 1);
    } else {
      adjustScroll(selectedIndex);
    }
  }, [displayItems.length, viewportLines, selectedIndex, moveSelection, adjustScroll]);

  // Clamp scrollLines to valid range whenever content shrinks.
  useEffect(() => {
    const maxScroll = Math.max(0, totalLoadedLines - viewportLines);
    setScrollLines((prev) => Math.min(prev, maxScroll));
  }, [totalLoadedLines, viewportLines]);

  // Jump to initialIndex once display items are loaded
  const initialJumpDone = useRef(false);
  useEffect(() => {
    if (initialIndex != null && !initialJumpDone.current && displayItems.length > 0) {
      initialJumpDone.current = true;
      const target = Math.min(initialIndex, displayItems.length - 1);
      jumpTo(target);
    }
  }, [initialIndex, displayItems.length, jumpTo]);

  // Load more messages when scrolling near bottom
  const loadMoreMessages = useCallback(() => {
    if (loadedOffsetLow <= 0) return;
    const nextOffset = Math.max(0, loadedOffsetLow - PAGE_SIZE);
    const fetchSize = loadedOffsetLow - nextOffset;
    if (fetchSize <= 0) return;

    const olderPage = api.getSessionMessages(
      session.projectSlug,
      session.sessionId,
      fetchSize,
      nextOffset,
      sourceScope,
    );
    setLoadedOffsetLow(nextOffset);
    setAllMessages((prev) => [...adaptMessagesForDisplay(olderPage.messages, session.sourceId), ...prev]);
  }, [api, session.projectSlug, session.sessionId, session.sourceId, loadedOffsetLow, sourceScope]);

  // Build filter chips + count label for display within this view
  const total = allDisplayItems.length;
  const shown = displayItems.length;
  const countLabel = total === shown ? `(${total})` : `(${shown}/${total})`;
  const filterChipsLine = `${buildFilterChips(filters, session.sourceId)}  ${countLabel}`;

  // Key handling
  useInput(
    (input, key) => {
      // Filter toggles
      if (input >= '1' && input <= '6') {
        setFilters((prev) => ({ ...prev, [input]: !prev[input] }));
        return;
      }

      if (key.upArrow) {
        moveUp();
      } else if (key.downArrow) {
        moveDown();
        // Load more when near bottom
        if (loadedOffsetLow > 0 && selectedIndex >= displayItems.length - LOAD_MORE_THRESHOLD) {
          loadMoreMessages();
        }
      } else if (key.return) {
        if (displayItems.length === 0) return;
        const selected = displayItems[selectedIndex];
        if (!selected) return;

        let detailBreadcrumb: string;
        const detailItem = selected;

        if (selected.kind === 'tool-call') {
          detailBreadcrumb = `${selected.toolName} ${toolInputSummary(selected.toolName, selected.toolInput).slice(0, 40)}`;
        } else if (selected.kind === 'thinking') {
          const label = selected.redacted ? 'Redacted Thinking' : 'Thinking';
          const tokLabel =
            !selected.redacted && selected.tokenEstimate > 0 ? ` ~${formatTokens(selected.tokenEstimate)} tokens` : '';
          detailBreadcrumb = `${label}${tokLabel}`;
        } else {
          const role = selected.msg.type || '';
          const ts =
            'timestamp' in selected.msg && (selected.msg as any).timestamp
              ? formatRelativeTime((selected.msg as any).timestamp)
              : '';
          detailBreadcrumb = `Message ${selectedIndex + 1} (${role} \u00B7 ${ts})`;
        }

        const entry: ViewEntry = {
          type: 'detail',
          component: () => <DetailView item={detailItem} />,
          breadcrumb: detailBreadcrumb,
          hints: '\u2191\u2193 scroll  Esc back  q quit',
        };
        nav.push(entry);
      } else if (key.escape) {
        nav.pop();
      }
    },
    { isActive: !nav.searchMode },
  );

  // Collect visible lines: slice from allLinesUnselected, patch selected item's lines.
  const selectedStart = displayItems[selectedIndex] ? itemLineStarts[selectedIndex] : -1;
  const selectedEnd = selectedStart >= 0 ? selectedStart + itemHeights[selectedIndex] : -1;

  const visibleLines: string[] = [];
  for (let i = 0; i < viewportLines; i++) {
    const absLine = scrollLines + i;
    if (absLine >= allLinesUnselected.length) break;
    if (absLine >= selectedStart && absLine < selectedEnd) {
      const lineInItem = absLine - selectedStart;
      visibleLines.push(selectedItemLines[lineInItem] ?? allLinesUnselected[absLine]);
    } else {
      visibleLines.push(allLinesUnselected[absLine]);
    }
  }
  const padLines = Math.max(0, viewportLines - visibleLines.length);

  // Scrollbar estimation: project total height from avg line/item from loaded set.
  const avgLinesPerItem = displayItems.length > 0 ? totalLoadedLines / displayItems.length : 1;
  const unloadedItems = Math.max(0, totalCount - allMessages.length);
  const unloadedLinesBefore = Math.round(unloadedItems * avgLinesPerItem);
  const estimatedTotalLines = totalLoadedLines + unloadedLinesBefore;

  return (
    <Box flexDirection="column">
      <Text> {filterChipsLine}</Text>
      <HRule />
      {displayItems.length === 0 ? (
        <Box flexDirection="column" height={viewportLines}>
          <Box paddingLeft={2} marginTop={1}>
            <Text dimColor>No messages match current filters.</Text>
          </Box>
        </Box>
      ) : (
        <Box height={viewportLines}>
          {/* Message content — leave 1 col for scrollbar */}
          <Box flexDirection="column" flexGrow={1}>
            {visibleLines.map((line, i) => (
              <Text key={i}>{line}</Text>
            ))}
            {padLines > 0 && <Box height={padLines} />}
          </Box>
          {/* Scrollbar track */}
          <ScrollBar
            viewportHeight={viewportLines}
            scrollLines={scrollLines}
            loadedLines={totalLoadedLines}
            estimatedTotalLines={estimatedTotalLines}
            unloadedLinesBefore={unloadedLinesBefore}
          />
        </Box>
      )}
    </Box>
  );
}

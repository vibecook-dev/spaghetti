/**
 * Message renderer — render SessionMessage objects for terminal display
 */

import type { SessionMessage, SystemMessage } from '@vibecook/spaghetti-sdk';
import { theme } from './color.js';
import { formatRelativeTime, formatTokens } from './format.js';
import { getTerminalWidth } from './terminal.js';
import cliTruncate from 'cli-truncate';
import { Marked } from 'marked';
import { markedTerminal } from 'marked-terminal';
import pc from 'picocolors';

// ─── Markdown rendering ────────────────────────────────────────────────

const MIN_MD_WIDTH = 20;
const markdownColors = pc.createColors(
  process.env.FORCE_COLOR === undefined ? pc.isColorSupported : process.env.FORCE_COLOR !== '0',
);
let mdCache: { width: number; instance: Marked } | null = null;

function getMarkdownRenderer(width: number): Marked {
  const w = Math.max(width, MIN_MD_WIDTH);
  if (mdCache && mdCache.width === w) return mdCache.instance;
  const instance = new Marked();
  instance.use(
    markedTerminal({
      width: w,
      reflowText: true,
      tab: 2,
      heading: (s: string) => markdownColors.bold(markdownColors.cyan(s)),
      firstHeading: (s: string) => markdownColors.bold(markdownColors.cyan(s)),
      strong: markdownColors.bold,
      em: markdownColors.italic,
      codespan: (s: string) => markdownColors.cyan(s),
      code: (s: string) => markdownColors.dim(s),
      blockquote: (s: string) => markdownColors.dim(markdownColors.italic(s)),
      hr: () => markdownColors.dim('─'.repeat(w)),
      listitem: (s: string) => s,
      link: (s: string) => markdownColors.cyan(markdownColors.underline(s)),
      href: (s: string) => markdownColors.dim(s),
    }) as Parameters<Marked['use']>[0],
  );
  mdCache = { width: w, instance };
  return instance;
}

/**
 * Render markdown text to an array of pre-wrapped ANSI-styled terminal lines.
 * Safe to call with plain text — marked passes it through with minimal changes.
 */
export function renderMarkdownText(text: string, width: number): string[] {
  const md = getMarkdownRenderer(width);
  const rendered = md.parse(text, { async: false }) as string;
  return rendered.replace(/\n+$/, '').split('\n');
}

/**
 * Strip markdown syntax for single-line previews in list views.
 * Fast regex-based pass — not a full parser. Keeps content readable without
 * literal `**`, `#`, `-`, etc. cluttering the preview.
 */
export function stripMarkdownInline(text: string): string {
  return text
    .replace(/```[\s\S]*?```/g, ' [code] ')
    .replace(/`([^`]+)`/g, '$1')
    .replace(/!\[([^\]]*)\]\([^)]*\)/g, '$1')
    .replace(/\[([^\]]+)\]\([^)]*\)/g, '$1')
    .replace(/^#{1,6}\s+/gm, '')
    .replace(/\*\*([^*]+)\*\*/g, '$1')
    .replace(/__([^_]+)__/g, '$1')
    .replace(/(^|[\s(])\*([^*\n]+)\*(?=[\s).,!?]|$)/g, '$1$2')
    .replace(/(^|[\s(])_([^_\n]+)_(?=[\s).,!?]|$)/g, '$1$2')
    .replace(/^\s*[-*+]\s+/gm, '')
    .replace(/^\s*\d+\.\s+/gm, '')
    .replace(/^\s*>\s?/gm, '');
}

export interface RenderOptions {
  compact?: boolean;
  noTools?: boolean;
  noThinking?: boolean;
  width?: number;
  /** Agent source id — drives the assistant header label (Claude / Codex / …). */
  sourceId?: string;
}

/**
 * Render a single SessionMessage for terminal display.
 */
export function renderMessage(msg: SessionMessage, opts?: RenderOptions): string {
  const width = opts?.width ?? getTerminalWidth();

  if (opts?.compact) {
    return renderCompact(msg, width, opts);
  }

  switch (msg.type) {
    case 'user':
      // When noTools is set, skip user messages that are purely tool_result blocks
      if (opts?.noTools && isToolResultOnly(msg)) {
        return '';
      }
      return renderUserMessage(msg, width);
    case 'assistant':
      return renderAssistantMessage(msg, width, opts);
    case 'system':
      return renderSystemMessage(msg, width);
    default:
      return renderOtherMessage(msg);
  }
}

/**
 * Render messages in compact (one-line-per-message) view.
 */
function renderCompact(msg: SessionMessage, width: number, opts?: RenderOptions): string {
  switch (msg.type) {
    case 'user': {
      // When noTools is set, skip user messages that are purely tool_result blocks
      if (opts?.noTools && isToolResultOnly(msg)) {
        return '';
      }
      const content = extractUserContent(msg);
      const preview = cliTruncate(content.replace(/\n/g, ' '), Math.max(width - 30, 20));
      return `  ${theme.accent('You')}     ${preview}`;
    }
    case 'assistant': {
      const payload = msg.message;
      const blocks = payload.content || [];
      const textBlocks = blocks.filter((b: any): b is { type: 'text'; text: string } => b.type === 'text');
      const toolBlocks = blocks.filter((b: any) => b.type === 'tool_use');

      // When noTools is set, skip assistant messages that contain ONLY tool_use blocks
      if (opts?.noTools) {
        const hasNonToolBlock = blocks.some((b: any) => b.type !== 'tool_use');
        if (!hasNonToolBlock) {
          return '';
        }
      }

      const text = textBlocks
        .map((b: any) => b.text)
        .join(' ')
        .replace(/\n/g, ' ');
      const tokens = payload.usage ? formatTokens(payload.usage.input_tokens + payload.usage.output_tokens) : '';
      const toolCount = toolBlocks.length;
      const toolInfo = toolCount > 0 && !opts?.noTools ? theme.muted(` [${toolCount} tools]`) : '';
      const tokInfo = tokens ? theme.muted(` (${tokens})`) : '';
      const preview = cliTruncate(text, Math.max(width - 50, 20));
      const agentLabel = theme.assistantName(opts?.sourceId);
      // Compact list: title-case from ASSISTANT_NAME (CLAUDE → Claude, CODEX → Codex)
      const display = agentLabel.charAt(0) + agentLabel.slice(1).toLowerCase().replace(/_/g, ' ');
      return `  ${theme.success(display)}  ${preview}${tokInfo}${toolInfo}`;
    }
    case 'system': {
      const subtype = getSystemSubtype(msg);
      const content = getSystemContent(msg);
      const preview = cliTruncate(content.replace(/\n/g, ' '), Math.max(width - 30, 20));
      return `  ${theme.muted(`[${subtype}]`)} ${preview}`;
    }
    default:
      return `  ${theme.muted(`[${msg.type}]`)}`;
  }
}

/** Check if a user message contains only tool_result content blocks. */
function isToolResultOnly(msg: SessionMessage & { type: 'user' }): boolean {
  const content = msg.message.content;
  if (typeof content === 'string') return false;
  if (!Array.isArray(content) || content.length === 0) return false;
  return content.every((block: any) => block.type === 'tool_result');
}

function extractUserContent(msg: SessionMessage & { type: 'user' }): string {
  const payload = msg.message;
  if (typeof payload.content === 'string') {
    return payload.content;
  }
  if (Array.isArray(payload.content)) {
    return payload.content
      .map((block: any) => {
        if (block.type === 'text') return block.text;
        if (block.type === 'tool_result') {
          const c = block.content;
          if (typeof c === 'string') return `[tool_result] ${c.slice(0, 100)}`;
          return '[tool_result]';
        }
        return `[${block.type}]`;
      })
      .join(' ');
  }
  return '';
}

function renderUserMessage(msg: SessionMessage & { type: 'user' }, width: number): string {
  const lines: string[] = [];
  const timestamp = 'timestamp' in msg && msg.timestamp ? formatRelativeTime(msg.timestamp) : '';
  const headerLeft = theme.accent('You:');
  const headerRight = timestamp ? theme.muted(timestamp) : '';
  const gap = Math.max(width - 6 - timestamp.length, 0);
  lines.push(`  ${headerLeft}${' '.repeat(gap)}${headerRight}`);

  const content = extractUserContent(msg);
  if (content) {
    for (const line of content.split('\n')) {
      lines.push(`  ${line}`);
    }
  }

  lines.push('');
  return lines.join('\n');
}

function renderAssistantMessage(
  msg: SessionMessage & { type: 'assistant' },
  width: number,
  opts?: RenderOptions,
): string {
  const payload = msg.message;
  const blocks = payload.content || [];

  // When noTools is set, skip assistant messages that contain ONLY tool_use blocks
  if (opts?.noTools) {
    const hasNonToolBlock = blocks.some((b: any) => b.type !== 'tool_use');
    if (!hasNonToolBlock) {
      return '';
    }
  }

  const lines: string[] = [];
  const timestamp = 'timestamp' in msg && msg.timestamp ? formatRelativeTime(msg.timestamp) : '';
  const tokenInfo = payload.usage
    ? theme.muted(` (${formatTokens(payload.usage.input_tokens + payload.usage.output_tokens)} tokens)`)
    : '';

  const agentLabel = theme.assistantName(opts?.sourceId);
  const display = agentLabel.charAt(0) + agentLabel.slice(1).toLowerCase().replace(/_/g, ' ');
  const headerLeft = theme.success(`${display}:`);
  const headerRight = timestamp ? theme.muted(timestamp) : '';
  lines.push(`  ${headerLeft}${tokenInfo}${headerRight ? '  ' + headerRight : ''}`);

  const mdWidth = Math.max(width - 2, MIN_MD_WIDTH);

  for (const block of blocks) {
    switch (block.type) {
      case 'text':
        for (const line of renderMarkdownText(block.text, mdWidth)) {
          lines.push(`  ${line}`);
        }
        break;

      case 'tool_use':
        if (!opts?.noTools) {
          const inputSummary = summarizeToolInput(block.input);
          lines.push(`  ${theme.warning(`[Tool: ${block.name}]`)} ${theme.muted(inputSummary)}`);
        }
        break;

      case 'thinking':
        if (!opts?.noThinking) {
          const thinkLen = block.thinking?.length ?? 0;
          const tokenEstimate = Math.round(thinkLen / 4);
          lines.push(`  ${theme.muted(`(thinking... ~${formatTokens(tokenEstimate)} tokens)`)}`);
        }
        break;

      case 'redacted_thinking':
        if (!opts?.noThinking) {
          lines.push(`  ${theme.muted('(redacted thinking)')}`);
        }
        break;
    }
  }

  lines.push('');
  return lines.join('\n');
}

function renderSystemMessage(msg: SessionMessage & { type: 'system' }, _width: number): string {
  const subtype = getSystemSubtype(msg);
  const content = getSystemContent(msg);
  const preview = content.replace(/\n/g, ' ').slice(0, 80);
  return `  ${theme.muted(`[system: ${subtype}]`)} ${theme.muted(preview)}`;
}

function renderOtherMessage(msg: SessionMessage): string {
  // Skip progress, file-history-snapshot, etc. — show one-line summary
  if (msg.type === 'summary') {
    const summary = msg.summary || '';
    return `  ${theme.muted('[summary]')} ${theme.muted(summary.slice(0, 80))}`;
  }
  return '';
}

/** Extract the subtype from a SystemMessage union. */
function getSystemSubtype(msg: SystemMessage): string {
  if ('subtype' in msg) return msg.subtype;
  return 'system';
}

/** Extract displayable content from a SystemMessage union. */
function getSystemContent(msg: SystemMessage): string {
  if ('content' in msg && typeof msg.content === 'string') return msg.content;
  if ('subtype' in msg && msg.subtype === 'turn_duration' && 'durationMs' in msg) {
    return `${msg.durationMs}ms`;
  }
  if ('subtype' in msg && msg.subtype === 'api_error' && 'cause' in msg) {
    return 'API error';
  }
  return '';
}

function summarizeToolInput(input: Record<string, unknown>): string {
  // Show the most relevant field from the tool input
  if ('command' in input && typeof input.command === 'string') {
    return input.command.slice(0, 60);
  }
  if ('file_path' in input && typeof input.file_path === 'string') {
    return input.file_path;
  }
  if ('pattern' in input && typeof input.pattern === 'string') {
    return input.pattern;
  }
  if ('content' in input && typeof input.content === 'string') {
    return input.content.slice(0, 60);
  }
  if ('query' in input && typeof input.query === 'string') {
    return input.query.slice(0, 60);
  }
  if ('skill' in input && typeof input.skill === 'string') {
    return input.skill;
  }

  // Fallback: show keys
  const keys = Object.keys(input);
  if (keys.length === 0) return '';
  return `{${keys.join(', ')}}`;
}

/** Internal message types that are filtered out during rendering. */
export const INTERNAL_MESSAGE_TYPES: ReadonlySet<string> = new Set([
  'progress',
  'file-history-snapshot',
  'saved_hook_context',
  'queue-operation',
  'last-prompt',
]);

/** Filter out internal message types that produce no visible output. */
export function filterDisplayableMessages(messages: SessionMessage[]): SessionMessage[] {
  return messages.filter((msg: SessionMessage) => !INTERNAL_MESSAGE_TYPES.has(msg.type));
}

/**
 * Render a batch of messages for display (session transcript view).
 */
export function renderMessages(messages: SessionMessage[], opts?: RenderOptions): string {
  const lines: string[] = [];

  for (const msg of filterDisplayableMessages(messages)) {
    const rendered = renderMessage(msg, opts);
    if (rendered) {
      lines.push(rendered);
    }
  }

  return lines.join('\n');
}

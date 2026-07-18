/**
 * Archive transcript — rail + icon language from spaghetti-ui-design.
 *
 * Design language:
 * - 1px solid hairline on the main rail (ink @ ~17% alpha)
 * - 1px dashed indigo on sub-thread rails
 * - Square nodes: paper fill, left accent rule only, monoline lucide icons
 * - Cubic SVG forks/returns with strokeDasharray "2 2" (no glow/blur)
 * - Type label mono uppercase; body serif for prose
 */

import { useState, useCallback, useMemo, type ComponentType } from 'react';
import {
  User,
  Feather,
  Brain,
  SquareTerminal,
  CheckCircle2,
  GitBranch,
  ChevronDown,
  ChevronRight,
  Circle,
  Layers,
  ListTodo,
} from 'lucide-react';
import { MarkdownContent, ToolResultRenderer, type ChatSessionMessage } from '@vibecook/spaghetti-sdk/react';
import { accentHex, inkHex, paperFill } from '../lib/archive-theme.js';

const RAIL_W = 28;
const NODE_SIZE = 24;
const INDENT = 28;

type RailKind = 'user' | 'assistant' | 'thought' | 'tool_use' | 'tool_result' | 'branch_start' | 'system' | 'summary';

const TYPE_META: Record<RailKind, { label: string; Icon: ComponentType<{ size?: number; strokeWidth?: number }> }> = {
  user: { label: 'Participant', Icon: User },
  assistant: { label: 'Scribe', Icon: Feather },
  thought: { label: 'Thought', Icon: Brain },
  tool_use: { label: 'Tool Use', Icon: SquareTerminal },
  tool_result: { label: 'Tool Result', Icon: CheckCircle2 },
  branch_start: { label: 'Branch', Icon: GitBranch },
  system: { label: 'System', Icon: Layers },
  summary: { label: 'Summary', Icon: ListTodo },
};

function mapKind(msg: ChatSessionMessage): RailKind {
  switch (msg.type) {
    case 'user':
      return 'user';
    case 'assistant':
      return 'assistant';
    case 'thinking':
      return 'thought';
    case 'tool_use':
      return msg.toolUse?.toolName === 'Task' ? 'branch_start' : 'tool_use';
    case 'tool_result':
      return 'tool_result';
    case 'compact_summary':
    case 'summary':
      return 'summary';
    case 'system':
    case 'checkpoint':
    case 'queue-operation':
      return 'system';
    default:
      return 'system';
  }
}

function depthOf(msg: ChatSessionMessage): 0 | 1 {
  return msg.isSidechain ? 1 : 0;
}

function formatTs(iso: string): string {
  if (!iso) return '';
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return '';
  return d.toLocaleTimeString('en-US', {
    hour12: false,
    hour: '2-digit',
    minute: '2-digit',
    second: '2-digit',
  });
}

function toolCommand(msg: ChatSessionMessage): string {
  const tu = msg.toolUse;
  if (!tu) return '';
  const input = tu.input ?? {};
  if (typeof input.command === 'string') return input.command;
  if (typeof input.file_path === 'string') return `file_path: ${input.file_path}`;
  if (typeof input.path === 'string') return `path: ${input.path}`;
  if (typeof input.pattern === 'string') return `pattern: ${input.pattern}`;
  if (typeof input.query === 'string') return `query: ${input.query}`;
  if (typeof input.prompt === 'string') return input.prompt;
  if (typeof input.description === 'string') return input.description;
  try {
    const s = JSON.stringify(input);
    return s.length > 280 ? s.slice(0, 280) + '…' : s;
  } catch {
    return tu.toolName;
  }
}

function branchLabel(msg: ChatSessionMessage): string | undefined {
  if (msg.type !== 'tool_use' || msg.toolUse?.toolName !== 'Task') return undefined;
  const input = msg.toolUse.input ?? {};
  const raw =
    (typeof input.description === 'string' && input.description) ||
    (typeof input.subagent_type === 'string' && input.subagent_type) ||
    (typeof input.prompt === 'string' && input.prompt.slice(0, 48)) ||
    'Task';
  return raw.length > 40 ? raw.slice(0, 40) + '…' : raw;
}

export interface ArchiveTranscriptProps {
  messages: ChatSessionMessage[];
  isDark: boolean;
}

export function ArchiveTranscript({ messages, isDark }: ArchiveTranscriptProps) {
  const [collapsed, setCollapsed] = useState<Set<string>>(() => new Set());

  const toggle = useCallback((id: string) => {
    setCollapsed((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  }, []);

  /**
   * Collapse a branch_start (Task) hides following sidechain depth until
   * the transcript returns to main — same rule as the design mock.
   */
  const visible = useMemo(() => {
    const hideBelow: number[] = [];
    return messages.filter((msg) => {
      const depth = depthOf(msg);
      while (hideBelow.length > 0 && depth <= hideBelow[hideBelow.length - 1]!) {
        hideBelow.pop();
      }
      if (hideBelow.length > 0) return false;
      const kind = mapKind(msg);
      if (kind === 'branch_start' && collapsed.has(msg.uuid)) {
        hideBelow.push(depth);
      }
      return true;
    });
  }, [messages, collapsed]);

  const ink = inkHex(isDark);
  const line = ink + '2b'; // hairline ~17% (design rail)
  const paper = paperFill(isDark);
  const branchAccent = accentHex('branch_start', isDark);

  return (
    <div className="flex flex-col pt-2 pb-8">
      {visible.map((msg, i) => {
        const next = visible[i + 1];
        const depth = depthOf(msg);
        const nextDepth = next ? depthOf(next) : 0;
        const kind = mapKind(msg);
        const meta = TYPE_META[kind];
        const Icon = meta.Icon;
        const accent = accentHex(kind, isDark);
        const isCollapsed = collapsed.has(msg.uuid);
        const ToggleIcon = isCollapsed ? ChevronRight : ChevronDown;
        const isLast = i === visible.length - 1;
        const returnsToMain = depth > 0 && nextDepth < depth;
        /** Fork when Task sits on main, or main message whose next is sidechain. */
        const drawFork = !isCollapsed && depth === 0 && (kind === 'branch_start' || nextDepth === 1);
        const railExtent = isLast ? { height: 18 } : { bottom: 0 };
        const labelChip = branchLabel(msg);

        return (
          <div key={msg.uuid} className="relative flex" style={{ marginLeft: depth * INDENT }}>
            {/* Rail column */}
            <div className="relative shrink-0 flex justify-start" style={{ width: RAIL_W }}>
              {/* Continuous main rail behind branched entries */}
              {depth > 0 && (
                <div
                  className="absolute w-px"
                  style={{ left: -depth * INDENT, background: line, top: 0, ...railExtent }}
                />
              )}

              {/* Solid main / dashed branch rail */}
              <div
                className="absolute left-0"
                style={
                  depth > 0
                    ? returnsToMain
                      ? {
                          borderLeft: `1px dashed ${branchAccent}`,
                          top: 0,
                          height: 'calc(100% - 42px)',
                        }
                      : {
                          borderLeft: `1px dashed ${branchAccent}`,
                          top: 0,
                          ...railExtent,
                        }
                    : { width: 1, background: line, top: 0, ...railExtent }
                }
              />

              {/* Square node — left accent rule, paper fill cuts the line */}
              <button
                type="button"
                onClick={() => toggle(msg.uuid)}
                aria-expanded={!isCollapsed}
                aria-label={`${isCollapsed ? 'Expand' : 'Collapse'} ${meta.label} message`}
                className="relative z-10 mt-1 flex items-center justify-center rounded-none"
                style={{
                  width: NODE_SIZE,
                  height: NODE_SIZE,
                  borderLeft: `1px solid ${accent}`,
                  background: paper,
                  color: accent,
                }}
              >
                <Icon size={12} strokeWidth={1.5} />
              </button>

              {/* Dashed cubic fork into sub-thread */}
              {drawFork && (
                <svg
                  className="absolute pointer-events-none overflow-visible"
                  style={{ left: 0, bottom: -2, width: INDENT + 2, height: 44 }}
                  viewBox={`0 0 ${INDENT + 2} 44`}
                  aria-hidden
                >
                  <path
                    d={`M0 0 C 0 26, ${INDENT} 18, ${INDENT} 44`}
                    fill="none"
                    stroke={branchAccent}
                    strokeWidth="1"
                    strokeDasharray="2 2"
                  />
                </svg>
              )}

              {/* Mirrored dashed return to main rail */}
              {returnsToMain && (
                <svg
                  className="absolute pointer-events-none overflow-visible"
                  style={{ left: -INDENT, bottom: -2, width: INDENT + 2, height: 44 }}
                  viewBox={`0 0 ${INDENT + 2} 44`}
                  aria-hidden
                >
                  <path
                    d={`M${INDENT} 0 C ${INDENT} 26, 0 18, 0 44`}
                    fill="none"
                    stroke={branchAccent}
                    strokeWidth="1"
                    strokeDasharray="2 2"
                  />
                </svg>
              )}
            </div>

            {/* Content */}
            <div className="flex-1 min-w-0 pb-8 pl-3">
              <div className="mt-1 mb-1.5 flex min-h-6 items-center gap-3">
                <button
                  type="button"
                  onClick={() => toggle(msg.uuid)}
                  aria-expanded={!isCollapsed}
                  className="-ml-1 inline-flex h-6 items-center gap-1.5 px-1 font-mono text-[9px] font-bold uppercase tracking-[0.2em] opacity-85 transition-opacity hover:opacity-100 bg-transparent border-0 cursor-pointer"
                  style={{ color: accent }}
                >
                  <span>{meta.label}</span>
                  <ToggleIcon size={11} strokeWidth={1.6} />
                </button>
                {labelChip ? (
                  <span
                    className="font-mono text-[9px] tracking-widest px-2 py-0.5 border rounded-none max-w-[220px] truncate"
                    style={{ color: accent, borderColor: accent + '66' }}
                    title={labelChip}
                  >
                    {labelChip}
                  </span>
                ) : null}
                {msg.type === 'tool_use' && msg.toolUse && kind !== 'branch_start' ? (
                  <span className="font-mono text-[9px] tracking-widest opacity-50">{msg.toolUse.toolName}</span>
                ) : null}
                <span className="ml-auto font-mono text-[8px] uppercase tracking-widest opacity-40">
                  {formatTs(msg.timestamp)}
                </span>
              </div>

              <div className={isCollapsed ? 'hidden' : ''}>
                <MessageBody msg={msg} kind={kind} isDark={isDark} accent={accent} />
              </div>
            </div>
          </div>
        );
      })}
    </div>
  );
}

/** Fence tool input for Prism via MarkdownContent (language from tool name). */
function toolLang(toolName: string): string {
  switch (toolName) {
    case 'Bash':
    case 'Shell':
    case 'bash':
      return 'bash';
    case 'Read':
    case 'Write':
    case 'Edit':
    case 'NotebookEdit':
      return 'text';
    case 'Grep':
    case 'Glob':
      return 'text';
    default:
      return toolName.toLowerCase().includes('json') ? 'json' : 'text';
  }
}

function fenced(lang: string, code: string): string {
  // Avoid breaking out of the fence if content has ```
  const safe = code.replace(/```/g, '``\u200b`');
  return `\`\`\`${lang}\n${safe}\n\`\`\``;
}

function MessageBody({
  msg,
  kind,
  isDark,
  accent,
}: {
  msg: ChatSessionMessage;
  kind: RailKind;
  isDark: boolean;
  accent: string;
}) {
  if (kind === 'user' || msg.type === 'user') {
    return (
      <div className="archive-md archive-md--user text-ink">
        <MarkdownContent content={msg.content || ''} />
      </div>
    );
  }

  if (kind === 'assistant' || msg.type === 'assistant') {
    return (
      <div className="archive-md archive-md--assistant text-ink">
        <MarkdownContent content={msg.content || ''} />
      </div>
    );
  }

  if (kind === 'thought' || msg.type === 'thinking') {
    return (
      <div className="archive-md archive-md--thought text-ink">
        <MarkdownContent content={msg.content || msg.thinking || ''} />
      </div>
    );
  }

  if (kind === 'branch_start') {
    const text = msg.toolUse
      ? `Delegating to **${String(msg.toolUse.input?.subagent_type ?? 'agent')}**…`
      : msg.content || 'Delegating…';
    return (
      <div className="archive-md archive-md--branch" style={{ color: accent }}>
        <MarkdownContent content={text} />
      </div>
    );
  }

  if (msg.type === 'tool_use' && msg.toolUse) {
    const cmd = toolCommand(msg);
    const hasResult = Boolean(msg.toolUse.result);
    const isError = msg.toolUse.result?.isError;
    const resultAccent = isError ? accentHex('assistant', isDark) : accentHex('tool_result', isDark);
    const lang = toolLang(msg.toolUse.toolName);
    return (
      <div className="border border-[color:var(--archive-ink-line-mid)] bg-ink/[0.04] px-4 py-3 rounded-none">
        <div className="font-mono text-[10px] uppercase tracking-widest mb-1.5" style={{ color: accent }}>
          {msg.toolUse.toolName}
        </div>
        <div className="archive-md archive-md--code">
          <MarkdownContent content={fenced(lang, cmd)} />
        </div>
        {hasResult ? (
          <div className="mt-3 pt-2 border-t border-[color:var(--archive-ink-line-soft)]">
            <div
              className="flex items-center gap-2 font-mono text-[9px] uppercase tracking-widest mb-1.5 opacity-70"
              style={{ color: resultAccent }}
            >
              {isError ? (
                <>
                  <Circle size={10} strokeWidth={1.5} /> OP_ERROR
                </>
              ) : (
                <>
                  <CheckCircle2 size={10} strokeWidth={1.5} /> OP_SUCCESS
                </>
              )}
            </div>
            <div className={`archive-md archive-md--result max-h-64 overflow-y-auto ${isError ? 'text-sanguine' : ''}`}>
              <ToolResultRenderer
                toolName={msg.toolUse.toolName}
                content={msg.toolUse.result?.content ?? ''}
                isError={isError}
                rawJson={msg.toolUse.result?.rawJson}
              />
            </div>
          </div>
        ) : (
          <div className="font-mono text-[9px] uppercase tracking-widest mt-2 opacity-50">No result captured</div>
        )}
      </div>
    );
  }

  if (msg.type === 'tool_result' && msg.toolResult) {
    return (
      <div className="archive-md archive-md--result">
        <ToolResultRenderer
          toolName="result"
          content={msg.toolResult.content}
          isError={msg.toolResult.isError}
          rawJson={msg.toolResult.rawJson}
        />
      </div>
    );
  }

  return (
    <div className="archive-md archive-md--thought text-ink opacity-70">
      <MarkdownContent content={msg.content || msg.systemSubtype || msg.type} />
    </div>
  );
}

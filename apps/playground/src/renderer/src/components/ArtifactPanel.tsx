/**
 * Session artifact drawer — plan, todos, task, subagents, project memory.
 * Lazy-loads each tab over IPC when opened.
 */

import { useCallback, useEffect, useState } from 'react';
import type { SubagentListItem } from '@vibecook/spaghetti-sdk';
import {
  TimelineMessageRenderer,
  TimeGroupSeparator,
  shouldShowTimestamp,
  isTimelineType,
  transformRawMessagesToTimeline,
  type ChatSessionMessage,
} from '@vibecook/spaghetti-sdk/react';
import { Btn, EmptyState, Spinner } from './ui.js';

export type ArtifactTab = 'plan' | 'todos' | 'task' | 'subagents' | 'memory';

export interface ArtifactPanelProps {
  open: boolean;
  onClose: () => void;
  projectSlug: string;
  sourceId: string;
  sessionId: string;
  /** Session affordances from list item (cheap badges). */
  hints: {
    todoCount: number;
    planSlug: string | null;
    hasTask: boolean;
    hasMemory?: boolean;
  };
  initialTab?: ArtifactTab;
  /** When true, render body only (tabs owned by Structure panel). */
  embedded?: boolean;
}

interface PlanShape {
  slug?: string;
  title?: string;
  content?: string;
  size?: number;
}

interface TaskShape {
  taskId?: string;
  hasHighwatermark?: boolean;
  highwatermark?: string | null;
  lockExists?: boolean;
}

interface TodoItemShape {
  content?: string;
  status?: string;
  activeForm?: string;
}

const TABS: { id: ArtifactTab; label: string }[] = [
  { id: 'plan', label: 'Plan' },
  { id: 'todos', label: 'Todos' },
  { id: 'task', label: 'Task' },
  { id: 'subagents', label: 'Subagents' },
  { id: 'memory', label: 'Memory' },
];

export function ArtifactPanel({
  open,
  onClose,
  projectSlug,
  sourceId,
  sessionId,
  hints,
  initialTab = 'plan',
  embedded = false,
}: ArtifactPanelProps) {
  const [tab, setTab] = useState<ArtifactTab>(initialTab);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const [plan, setPlan] = useState<PlanShape | null>(null);
  const [todos, setTodos] = useState<TodoItemShape[]>([]);
  const [task, setTask] = useState<TaskShape | null>(null);
  const [subagents, setSubagents] = useState<SubagentListItem[]>([]);
  const [memory, setMemory] = useState<string | null>(null);

  const [expandedAgent, setExpandedAgent] = useState<string | null>(null);
  const [agentMsgs, setAgentMsgs] = useState<ChatSessionMessage[]>([]);
  const [agentLoading, setAgentLoading] = useState(false);

  // Reset tab when panel opens with a preferred tab
  useEffect(() => {
    if (open) setTab(initialTab);
  }, [open, initialTab, sessionId]);

  // Escape closes (standalone drawer only)
  useEffect(() => {
    if (!open || embedded) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') {
        e.preventDefault();
        onClose();
      }
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [open, onClose, embedded]);

  // Parent Structure panel drives tab via initialTab when embedded
  useEffect(() => {
    if (embedded) setTab(initialTab);
  }, [embedded, initialTab]);

  const loadTab = useCallback(
    async (t: ArtifactTab) => {
      setLoading(true);
      setError(null);
      try {
        if (t === 'plan') {
          const p = (await window.spaghetti.getSessionPlan(projectSlug, sessionId)) as PlanShape | null;
          setPlan(p);
        } else if (t === 'todos') {
          const raw = await window.spaghetti.getSessionTodos(projectSlug, sessionId);
          setTodos(flattenTodos(raw));
        } else if (t === 'task') {
          const tsk = (await window.spaghetti.getSessionTask(projectSlug, sessionId)) as TaskShape | null;
          setTask(tsk);
        } else if (t === 'subagents') {
          const list = await window.spaghetti.getSessionSubagents(projectSlug, sessionId);
          setSubagents(list);
        } else if (t === 'memory') {
          const mem = await window.spaghetti.getProjectMemory(projectSlug, { sourceId });
          setMemory(mem);
        }
      } catch (e: unknown) {
        setError(String(e));
      } finally {
        setLoading(false);
      }
    },
    [projectSlug, sessionId, sourceId],
  );

  useEffect(() => {
    if (!open) return;
    void loadTab(tab);
    setExpandedAgent(null);
    setAgentMsgs([]);
  }, [open, tab, loadTab]);

  const openSubagent = async (agentId: string) => {
    if (expandedAgent === agentId) {
      setExpandedAgent(null);
      setAgentMsgs([]);
      return;
    }
    setExpandedAgent(agentId);
    setAgentLoading(true);
    try {
      const page = await window.spaghetti.getSubagentMessages(projectSlug, sessionId, agentId, 40, 0);
      setAgentMsgs(transformRawMessagesToTimeline(page.messages as unknown as Record<string, unknown>[]));
    } catch (e: unknown) {
      setError(String(e));
      setAgentMsgs([]);
    } finally {
      setAgentLoading(false);
    }
  };

  if (!open) return null;

  const tabHint = (id: ArtifactTab): string | null => {
    if (id === 'plan' && hints.planSlug) return '·';
    if (id === 'todos' && hints.todoCount > 0) return String(hints.todoCount);
    if (id === 'task' && hints.hasTask) return '·';
    if (id === 'memory' && hints.hasMemory) return '·';
    return null;
  };

  const body = (
    <div className="flex-1 overflow-y-auto min-h-0 scrollbar-hide">
      {error ? (
        <div className="px-3 py-2 font-mono text-[10px] text-sanguine border-b border-sanguine/20">{error}</div>
      ) : null}

      {loading ? (
        <div className="flex items-center justify-center gap-2 py-12 font-mono text-[10px] tracking-widest uppercase opacity-50">
          <Spinner />
          Loading…
        </div>
      ) : tab === 'plan' ? (
        <PlanBody plan={plan} />
      ) : tab === 'todos' ? (
        <TodosBody todos={todos} />
      ) : tab === 'task' ? (
        <TaskBody task={task} />
      ) : tab === 'subagents' ? (
        <SubagentsBody
          subagents={subagents}
          expandedAgent={expandedAgent}
          agentMsgs={agentMsgs}
          agentLoading={agentLoading}
          onToggle={openSubagent}
        />
      ) : (
        <MemoryBody memory={memory} sourceId={sourceId} />
      )}
    </div>
  );

  if (embedded) {
    return (
      <div className="flex flex-col h-full min-h-0 text-ink" aria-label="Session artifacts">
        {body}
      </div>
    );
  }

  return (
    <aside
      className="w-[300px] shrink-0 border-l border-ink/20 bg-transparent flex flex-col min-h-0 text-ink"
      aria-label="Session artifacts"
    >
      <header className="flex items-center gap-2 px-3 py-2 border-b border-ink/10 shrink-0">
        <h2 className="font-serif text-[10px] tracking-[0.14em] uppercase opacity-70 flex-1">Artifacts</h2>
        <Btn onClick={onClose} className="!px-2" title="Close artifacts (Esc)">
          Close
        </Btn>
      </header>

      <nav className="flex gap-0.5 px-2 py-1.5 border-b border-ink/10 overflow-x-auto shrink-0" role="tablist">
        {TABS.map((t) => {
          const hint = tabHint(t.id);
          const active = tab === t.id;
          return (
            <button
              key={t.id}
              type="button"
              role="tab"
              aria-selected={active}
              onClick={() => setTab(t.id)}
              className={`font-mono text-[9px] px-2 py-1 border-b cursor-pointer transition-colors whitespace-nowrap bg-transparent ${
                active ? 'border-ink text-ink' : 'border-transparent text-ink/40 hover:text-ink/70'
              }`}
            >
              {t.label}
              {hint ? <span className={`ml-1 ${active ? 'text-sanguine' : 'text-ink/30'}`}>{hint}</span> : null}
            </button>
          );
        })}
      </nav>

      {body}
    </aside>
  );
}

function PlanBody({ plan }: { plan: PlanShape | null }) {
  if (!plan) {
    return <EmptyState title="No plan" detail="This session has no linked plan file." />;
  }
  return (
    <div className="p-3 space-y-2">
      {plan.title ? <h3 className="font-serif text-sm text-ink/90">{plan.title}</h3> : null}
      {plan.slug ? <p className="text-[10px] font-mono text-ink/30 truncate">{plan.slug}</p> : null}
      <pre className="text-[11px] text-ink/70 whitespace-pre-wrap font-mono leading-relaxed">
        {plan.content || '(empty plan)'}
      </pre>
    </div>
  );
}

function TodosBody({ todos }: { todos: TodoItemShape[] }) {
  if (todos.length === 0) {
    return <EmptyState title="No todos" detail="No todo list is attached to this session." />;
  }
  return (
    <ul className="py-1">
      {todos.map((t, i) => {
        const status = (t.status || 'pending').toLowerCase();
        const color =
          status === 'completed' ? 'text-verdigris' : status === 'in_progress' ? 'text-sanguine' : 'text-ink/40';
        return (
          <li key={i} className="px-3 py-2 border-b border-ink/5 flex gap-2.5 items-start">
            <span className={`text-[10px] font-mono uppercase tracking-wide mt-0.5 shrink-0 w-16 ${color}`}>
              {status.replace('_', ' ')}
            </span>
            <div className="min-w-0">
              <p className="text-[12px] font-serif text-ink/80 leading-snug">
                {t.content || t.activeForm || '(empty)'}
              </p>
              {t.activeForm && t.content ? (
                <p className="text-[10px] text-ink/35 mt-0.5 italic">{t.activeForm}</p>
              ) : null}
            </div>
          </li>
        );
      })}
    </ul>
  );
}

function TaskBody({ task }: { task: TaskShape | null }) {
  if (!task) {
    return <EmptyState title="No task metadata" detail="No high-watermark / lock recorded for this session." />;
  }
  return (
    <div className="p-3 space-y-2 text-[12px]">
      <Row label="Task id" value={task.taskId || '—'} mono />
      <Row label="Lock" value={task.lockExists ? 'held' : 'none'} />
      <Row label="Highwatermark" value={task.hasHighwatermark ? 'yes' : 'no'} />
      {task.highwatermark ? (
        <pre className="mt-2 text-[10px] font-mono text-white/50 whitespace-pre-wrap break-all bg-white/[0.03] rounded p-2 border border-white/6">
          {task.highwatermark}
        </pre>
      ) : null}
    </div>
  );
}

function SubagentsBody({
  subagents,
  expandedAgent,
  agentMsgs,
  agentLoading,
  onToggle,
}: {
  subagents: SubagentListItem[];
  expandedAgent: string | null;
  agentMsgs: ChatSessionMessage[];
  agentLoading: boolean;
  onToggle: (id: string) => void;
}) {
  if (subagents.length === 0) {
    return <EmptyState title="No subagents" detail="No top-level subagent transcripts for this session." />;
  }
  return (
    <div className="py-1">
      {subagents.map((s) => {
        const open = expandedAgent === s.agentId;
        return (
          <div key={s.agentId} className="border-b border-white/[0.04]">
            <button
              type="button"
              onClick={() => void onToggle(s.agentId)}
              className="w-full text-left px-3 py-2.5 bg-transparent border-none cursor-pointer hover:bg-white/[0.04] transition-colors"
            >
              <div className="flex items-center gap-2">
                <span className="text-[12px] text-white/80 font-medium truncate flex-1">{s.agentType || 'agent'}</span>
                <span className="text-[10px] font-mono text-white/30">{s.messageCount} msgs</span>
              </div>
              <div className="text-[10px] font-mono text-white/25 truncate mt-0.5">{s.agentId}</div>
            </button>
            {open ? (
              <div className="bg-black/30 border-t border-white/5 max-h-72 overflow-y-auto">
                {agentLoading ? (
                  <div className="flex items-center gap-2 justify-center py-6 text-xs text-white/40">
                    <Spinner /> Loading transcript…
                  </div>
                ) : agentMsgs.length === 0 ? (
                  <p className="text-[11px] text-white/35 px-3 py-4">No messages</p>
                ) : (
                  <div className="py-2">
                    {agentMsgs.map((msg, i) => {
                      const prev = i > 0 ? agentMsgs[i - 1] : undefined;
                      const next = i < agentMsgs.length - 1 ? agentMsgs[i + 1] : undefined;
                      const showSep = shouldShowTimestamp(msg.timestamp, prev?.timestamp ?? null);
                      return (
                        <div key={msg.uuid} className="px-2">
                          {showSep && <TimeGroupSeparator timestamp={msg.timestamp} />}
                          <TimelineMessageRenderer
                            message={msg}
                            isLast={i === agentMsgs.length - 1}
                            connectToNext={!!(next && isTimelineType(next.type))}
                            prevTimestamp={prev?.timestamp}
                            prevAgentId={prev?.agentId}
                            nextAgentId={next?.agentId}
                            nextIsSidechain={next?.isSidechain}
                          />
                        </div>
                      );
                    })}
                  </div>
                )}
              </div>
            ) : null}
          </div>
        );
      })}
    </div>
  );
}

function MemoryBody({ memory, sourceId }: { memory: string | null; sourceId: string }) {
  if (sourceId !== 'claude-code') {
    return (
      <EmptyState title="Memory is Claude-only" detail="Project MEMORY.md is only indexed for Claude Code sources." />
    );
  }
  if (!memory) {
    return <EmptyState title="No memory" detail="This project has no MEMORY.md on disk." />;
  }
  return <pre className="p-3 text-[11px] text-white/65 whitespace-pre-wrap font-mono leading-relaxed">{memory}</pre>;
}

function Row({ label, value, mono }: { label: string; value: string; mono?: boolean }) {
  return (
    <div className="flex items-baseline gap-3">
      <span className="text-[10px] uppercase tracking-wide text-white/35 w-28 shrink-0">{label}</span>
      <span className={`text-white/75 break-all ${mono ? 'font-mono text-[11px]' : ''}`}>{value}</span>
    </div>
  );
}

/** Todos may be stored as nested arrays / TodoFile shapes. */
function flattenTodos(raw: unknown[]): TodoItemShape[] {
  const out: TodoItemShape[] = [];
  for (const entry of raw) {
    if (Array.isArray(entry)) {
      for (const item of entry) {
        if (item && typeof item === 'object') out.push(item as TodoItemShape);
      }
    } else if (entry && typeof entry === 'object') {
      const obj = entry as { items?: unknown[]; content?: string; status?: string };
      if (Array.isArray(obj.items)) {
        for (const item of obj.items) {
          if (item && typeof item === 'object') out.push(item as TodoItemShape);
        }
      } else if ('content' in obj || 'status' in obj) {
        out.push(obj as TodoItemShape);
      }
    }
  }
  return out;
}

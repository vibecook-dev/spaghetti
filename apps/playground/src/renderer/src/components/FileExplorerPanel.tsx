/**
 * Right Structure panel — design mock layout:
 *   Project Files | Plans | Todos | Tasks | Memory
 * each a collapsible section with a file tree (no tool chrome under Project Files).
 */

import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { ChevronDown, ChevronRight, TerminalSquare, X } from 'lucide-react';
import type { FileExplorer } from '@vibecook/mille';
import { connectFileExplorer, type PortFileExplorer } from '@vibecook/mille/port';
import { FileTreeProvider, FileTree, useFileTreeRef } from '@vibecook/mille-ui';
import { minimalIconTheme } from '@vibecook/mille-ui/icons/minimal';
import { createCommandRegistry, defaultCommands } from '@vibecook/mille-ui/commands';
import type { SubagentListItem } from '@vibecook/spaghetti-sdk';
import '@vibecook/mille-ui/tokens.css';
import '@vibecook/mille-ui/theme/minimal.css';
import { onFxPort } from '../lib/fx-port.js';
import { EmptyState, Spinner } from './ui.js';
import { StructureFilePreview, StructureTree, type StructureNode } from './StructureTree.js';

export interface SessionArtifactsProps {
  projectSlug: string;
  sourceId: string;
  sessionId: string;
  hints: {
    todoCount: number;
    planSlug: string | null;
    hasTask: boolean;
    hasMemory?: boolean;
  };
}

export interface FileExplorerPanelProps {
  open: boolean;
  onClose: () => void;
  projectPath: string | null;
  projectLabel?: string | null;
  isDark?: boolean;
  sessionArtifacts?: SessionArtifactsProps | null;
}

type StructureSectionId = 'project-files' | 'plans' | 'todos' | 'tasks' | 'memory' | 'subagents';

type ArtifactSectionId = Exclude<StructureSectionId, 'project-files'>;

const ARTIFACT_SECTIONS: { id: ArtifactSectionId; label: string }[] = [
  { id: 'plans', label: 'Plans' },
  { id: 'todos', label: 'Todos' },
  { id: 'tasks', label: 'Tasks' },
  { id: 'memory', label: 'Memory' },
  { id: 'subagents', label: 'Subagents' },
];

interface PlanShape {
  slug?: string;
  title?: string;
  content?: string;
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

export function FileExplorerPanel({
  open,
  onClose,
  projectPath,
  isDark = true,
  sessionArtifacts = null,
}: FileExplorerPanelProps) {
  const [fx, setFx] = useState<PortFileExplorer | null>(null);
  const [root, setRoot] = useState<string | null>(null);
  const [status, setStatus] = useState<'idle' | 'opening' | 'ready' | 'error'>('idle');
  const [error, setError] = useState<string | null>(null);
  const [openSections, setOpenSections] = useState<Set<StructureSectionId>>(() => new Set(['project-files']));
  const currentFxRef = useRef<PortFileExplorer | null>(null);
  const openSeq = useRef(0);

  // Artifact data (lazy per session)
  const [plan, setPlan] = useState<PlanShape | null>(null);
  const [todos, setTodos] = useState<TodoItemShape[]>([]);
  const [task, setTask] = useState<TaskShape | null>(null);
  const [memory, setMemory] = useState<string | null>(null);
  const [subagents, setSubagents] = useState<SubagentListItem[]>([]);
  const [artifactLoading, setArtifactLoading] = useState(false);
  const [preview, setPreview] = useState<{ section: StructureSectionId; node: StructureNode } | null>(null);

  useEffect(() => {
    if (!open) return;
    const onKey = (e: KeyboardEvent) => {
      const t = e.target;
      if (t instanceof HTMLElement && (t.closest('[data-mille-filter]') || t.tagName === 'INPUT')) {
        return;
      }
      if (e.key === 'Escape') {
        e.preventDefault();
        if (preview) {
          setPreview(null);
          return;
        }
        onClose();
      }
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [open, onClose, preview]);

  // Load artifact trees when a session is selected
  useEffect(() => {
    if (!sessionArtifacts) {
      setPlan(null);
      setTodos([]);
      setTask(null);
      setMemory(null);
      setSubagents([]);
      setPreview(null);
      return;
    }
    let cancelled = false;
    setArtifactLoading(true);
    setPreview(null);
    const { projectSlug, sessionId, sourceId } = sessionArtifacts;
    void (async () => {
      try {
        const [p, t, tsk, mem, agents] = await Promise.all([
          window.spaghetti.getSessionPlan(projectSlug, sessionId),
          window.spaghetti.getSessionTodos(projectSlug, sessionId),
          window.spaghetti.getSessionTask(projectSlug, sessionId),
          window.spaghetti.getProjectMemory(projectSlug, { sourceId }),
          window.spaghetti.getSessionSubagents(projectSlug, sessionId),
        ]);
        if (cancelled) return;
        setPlan((p as PlanShape | null) ?? null);
        setTodos(flattenTodos(t));
        setTask((tsk as TaskShape | null) ?? null);
        setMemory(mem);
        setSubagents(agents);
      } catch {
        if (!cancelled) {
          setPlan(null);
          setTodos([]);
          setTask(null);
          setMemory(null);
          setSubagents([]);
        }
      } finally {
        if (!cancelled) setArtifactLoading(false);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [sessionArtifacts?.projectSlug, sessionArtifacts?.sessionId, sessionArtifacts?.sourceId]);

  useEffect(() => {
    if (!open) {
      void currentFxRef.current?.dispose();
      currentFxRef.current = null;
      setFx(null);
      setRoot(null);
      setStatus('idle');
      setError(null);
      void window.mille?.closeWorkspace().catch(() => {});
      return;
    }

    if (!projectPath) {
      void currentFxRef.current?.dispose();
      currentFxRef.current = null;
      setFx(null);
      setRoot(null);
      setStatus('idle');
      setError(null);
      void window.mille?.closeWorkspace().catch(() => {});
      return;
    }

    if (!window.mille) {
      setStatus('error');
      setError('mille preload bridge missing');
      return;
    }

    const seq = ++openSeq.current;
    setStatus('opening');
    setError(null);

    const attach = async (port: MessagePort, workspaceRoot: string) => {
      if (seq !== openSeq.current) {
        port.close();
        return;
      }
      try {
        const next = await connectFileExplorer(port, {
          mirrorCap: 20_000,
          prefetchRows: 200,
        });
        if (seq !== openSeq.current) {
          void next.dispose();
          return;
        }
        const prev = currentFxRef.current;
        currentFxRef.current = next;
        setFx(next);
        setRoot(workspaceRoot);
        setStatus('ready');
        setError(null);
        if (prev) void prev.dispose();
      } catch (e: unknown) {
        if (seq !== openSeq.current) return;
        setStatus('error');
        setError(e instanceof Error ? e.message : String(e));
      }
    };

    const off = onFxPort(({ port, workspaceRoot }) => {
      void attach(port, workspaceRoot);
    });

    void window.mille.openWorkspace(projectPath).catch((e: unknown) => {
      if (seq !== openSeq.current) return;
      setStatus('error');
      setError(e instanceof Error ? e.message : String(e));
    });

    return () => {
      off();
      void currentFxRef.current?.dispose();
      currentFxRef.current = null;
      setFx(null);
    };
  }, [open, projectPath]);

  useEffect(() => {
    return () => {
      void currentFxRef.current?.dispose();
      currentFxRef.current = null;
      void window.mille?.closeWorkspace().catch(() => {});
    };
  }, []);

  const toggleSection = useCallback((id: StructureSectionId) => {
    setOpenSections((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  }, []);

  const plansTree = useMemo((): StructureNode[] => {
    if (!plan) return [];
    const name = plan.title || plan.slug || 'plan.md';
    const fileName = name.endsWith('.md') ? name : `${slugify(name)}.md`;
    return [
      {
        name: 'plans',
        type: 'folder',
        isOpen: true,
        children: [{ name: fileName, type: 'file', content: plan.content || '' }],
      },
    ];
  }, [plan]);

  const todosTree = useMemo((): StructureNode[] => {
    if (todos.length === 0) return [];
    return [
      {
        name: 'todo',
        type: 'folder',
        isOpen: true,
        children: todos.map((t, i) => {
          const label = (t.content || t.activeForm || `item-${i + 1}`).slice(0, 48);
          const status = (t.status || 'pending').replace(/_/g, '-');
          return {
            name: `${status} · ${label}${label.length >= 48 ? '…' : ''}.md`,
            type: 'file' as const,
            content: [
              `# ${t.content || t.activeForm || 'Todo'}`,
              '',
              `Status: ${t.status || 'pending'}`,
              t.activeForm ? `Active form: ${t.activeForm}` : '',
            ]
              .filter(Boolean)
              .join('\n'),
          };
        }),
      },
    ];
  }, [todos]);

  const tasksTree = useMemo((): StructureNode[] => {
    if (!task) return [];
    const id = task.taskId || 'task';
    return [
      {
        name: 'task-queue',
        type: 'folder',
        isOpen: true,
        children: [
          {
            name: `${id.slice(0, 8)}.md`,
            type: 'file',
            content: [
              `# Task ${id}`,
              '',
              `Lock: ${task.lockExists ? 'held' : 'none'}`,
              `Highwatermark: ${task.hasHighwatermark ? 'yes' : 'no'}`,
              task.highwatermark ? `\n\`\`\`\n${task.highwatermark}\n\`\`\`` : '',
            ].join('\n'),
          },
        ],
      },
    ];
  }, [task]);

  const memoryTree = useMemo((): StructureNode[] => {
    if (!memory) return [];
    return [
      {
        name: 'memory',
        type: 'folder',
        isOpen: true,
        children: [{ name: 'MEMORY.md', type: 'file', content: memory }],
      },
    ];
  }, [memory]);

  const subagentsTree = useMemo((): StructureNode[] => {
    if (subagents.length === 0) return [];
    return [
      {
        name: 'subagents',
        type: 'folder',
        isOpen: true,
        children: subagents.map((s) => ({
          name: `${s.agentType || 'agent'} · ${s.agentId.slice(0, 8)}.md`,
          type: 'file' as const,
          content: [`# ${s.agentType || 'agent'}`, '', `Agent id: ${s.agentId}`, `Messages: ${s.messageCount}`].join(
            '\n',
          ),
        })),
      },
    ];
  }, [subagents]);

  const trees: Record<ArtifactSectionId, StructureNode[]> = {
    plans: plansTree,
    todos: todosTree,
    tasks: tasksTree,
    memory: memoryTree,
    subagents: subagentsTree,
  };

  if (!open) return null;

  return (
    <aside
      className="w-64 border-l border-[color:var(--archive-ink-line)] flex flex-col shrink-0 bg-transparent min-h-0 z-20"
      data-theme={isDark ? 'dark' : 'light'}
      data-mille-theme="minimal"
      aria-label="Structure"
    >
      <div className="h-10 border-b border-[color:var(--archive-ink-line-soft)] flex items-center justify-between px-4 shrink-0">
        <span className="font-serif text-[10px] uppercase tracking-[0.15em] opacity-80">Structure</span>
        <button
          type="button"
          onClick={onClose}
          className="opacity-50 hover:opacity-100 bg-transparent border-0 text-ink cursor-pointer p-0"
          title="Close structure (Esc)"
        >
          <X size={12} />
        </button>
      </div>

      <div className="flex-1 overflow-y-auto scrollbar-hide min-h-0">
        {/* Project Files — mille tree only, no toolbar */}
        <StructureSection
          id="project-files"
          label="Project Files"
          open={openSections.has('project-files')}
          onToggle={() => toggleSection('project-files')}
        >
          <div className="min-h-[180px] h-[min(36vh,280px)] flex flex-col">
            {!projectPath ? (
              <EmptyState title="Select a project" detail="Opens the project folder on disk." />
            ) : status === 'opening' || (status === 'ready' && !fx) ? (
              <div className="flex items-center justify-center gap-2 py-10 font-mono text-[10px] tracking-widest uppercase opacity-50">
                <Spinner />
                Opening…
              </div>
            ) : status === 'error' ? (
              <EmptyState title="Could not open folder" detail={error ?? 'Unknown error'} />
            ) : fx && root ? (
              <ExplorerTree fx={fx} />
            ) : (
              <EmptyState title="Waiting for host" detail="Connecting to the file explorer…" />
            )}
          </div>
        </StructureSection>

        {/* Plans / Todos / Tasks / Memory / Subagents — each its own tree */}
        {ARTIFACT_SECTIONS.map(({ id, label }) => {
          const nodes = trees[id];
          const hasSession = Boolean(sessionArtifacts);
          return (
            <StructureSection
              key={id}
              id={id}
              label={label}
              open={openSections.has(id)}
              onToggle={() => toggleSection(id)}
            >
              {!hasSession ? (
                <p className="px-3 py-2 font-mono text-[9px] tracking-wide opacity-40">Open a session</p>
              ) : artifactLoading ? (
                <div className="flex items-center gap-2 px-3 py-3 font-mono text-[9px] tracking-widest uppercase opacity-50">
                  <Spinner /> Loading…
                </div>
              ) : (
                <>
                  <StructureTree nodes={nodes} onOpenFile={(node) => setPreview({ section: id, node })} />
                  {preview?.section === id && preview.node.content != null ? (
                    <StructureFilePreview
                      title={preview.node.name}
                      content={preview.node.content}
                      onClose={() => setPreview(null)}
                    />
                  ) : null}
                </>
              )}
            </StructureSection>
          );
        })}
      </div>
    </aside>
  );
}

function StructureSection({
  id,
  label,
  open,
  onToggle,
  children,
}: {
  id: string;
  label: string;
  open: boolean;
  onToggle: () => void;
  children: React.ReactNode;
}) {
  return (
    <section className="border-b border-[color:var(--archive-ink-line-soft)]" data-section={id}>
      <button
        type="button"
        onClick={onToggle}
        aria-expanded={open}
        className="flex w-full items-center justify-between px-4 py-2 font-mono text-[9px] tracking-widest opacity-70 transition-opacity hover:opacity-100 bg-transparent border-0 text-ink cursor-pointer"
      >
        <span className="flex min-w-0 items-center gap-2 uppercase">
          <TerminalSquare size={10} />
          <span className="truncate">{label}</span>
        </span>
        {open ? <ChevronDown size={12} /> : <ChevronRight size={12} />}
      </button>
      {open ? <div className="pb-1">{children}</div> : null}
    </section>
  );
}

/** Mille tree only — no filter/collapse chrome or project name strip. */
function ExplorerTree({ fx }: { fx: PortFileExplorer }) {
  const commands = useMemo(() => createCommandRegistry(defaultCommands), []);
  const treeRef = useFileTreeRef();
  const expandedRootsRef = useRef(false);

  useEffect(() => {
    expandedRootsRef.current = false;
    const tryExpand = (): void => {
      if (expandedRootsRef.current) return;
      const roots = fx.getSnapshot().roots();
      if (roots.length === 0) return;
      expandedRootsRef.current = true;
      fx.setExpanded({ add: roots.map((r) => r.id) });
    };
    tryExpand();
    const sub = fx.on('change', tryExpand);
    return () => sub.dispose();
  }, [fx]);

  return (
    <FileTreeProvider fx={fx as unknown as FileExplorer} commands={commands}>
      <div className="flex flex-col h-full min-h-0 mille-panel" data-mille-theme="minimal">
        <div className="flex-1 min-h-0 overflow-hidden">
          <FileTree ref={treeRef} ariaLabel="Project files" iconTheme={minimalIconTheme} rowHeight={26} overscan={24} />
        </div>
      </div>
    </FileTreeProvider>
  );
}

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

function slugify(s: string): string {
  return (
    s
      .toLowerCase()
      .replace(/[^a-z0-9]+/g, '-')
      .replace(/^-|-$/g, '')
      .slice(0, 48) || 'plan'
  );
}

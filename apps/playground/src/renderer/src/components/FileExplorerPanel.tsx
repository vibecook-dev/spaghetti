/**
 * Right Structure panel — Project Files is permanently visible while the
 * session-artifact switches stay docked to the bottom edge.
 */

import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { ChevronDown, ChevronRight, TerminalSquare, X } from 'lucide-react';
import type { Entry, FileExplorer } from '@vibecook/mille';
import { connectFileExplorer, type PortFileExplorer } from '@vibecook/mille/port';
import { FileTreeProvider, FileTree, useFileTreeRef } from '@vibecook/mille-ui';
import { minimalIconTheme } from '@vibecook/mille-ui/icons/minimal';
import { createCommandRegistry, defaultCommands } from '@vibecook/mille-ui/commands';
import type { ProjectReference, SubagentListItem } from '@vibecook/spaghetti-sdk';
import type { WorktreeInfo } from '@shared/ipc.js';
import '@vibecook/mille-ui/tokens.css';
import '@vibecook/mille-ui/theme/minimal.css';
import { onFxPort } from '../lib/fx-port.js';
import { EmptyState, Spinner } from './ui.js';
import { FileViewerDialog } from './FileViewerDialog.js';
import { StructureTree, type StructureNode } from './StructureTree.js';

export interface SessionArtifactsProps {
  projectSlug: string;
  sourceId: string;
  /** Aggregated project locator for source-correct project-level memory. */
  memoryProject?: ProjectReference;
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

type ArtifactSectionId = 'plans' | 'todos' | 'tasks' | 'memory' | 'subagents' | 'worktrees';

/**
 * `scope` decides what an empty section says. Everything here used to be
 * session-derived, so the accordion could assume "no session" was the only
 * reason to be empty. Worktrees belong to the project and are readable with no
 * session selected at all, so the prompt has to follow the section.
 */
const ARTIFACT_SECTIONS: { id: ArtifactSectionId; label: string; scope: 'session' | 'project' }[] = [
  { id: 'plans', label: 'Plans', scope: 'session' },
  { id: 'todos', label: 'Todos', scope: 'session' },
  { id: 'tasks', label: 'Tasks', scope: 'session' },
  { id: 'memory', label: 'Memory', scope: 'session' },
  { id: 'subagents', label: 'Subagents', scope: 'session' },
  { id: 'worktrees', label: 'Worktrees', scope: 'project' },
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

interface ProjectFilePreview {
  title: string;
  absolutePath: string;
  content: string | null;
  loading: boolean;
  error: string | null;
}

const MAX_PREVIEW_BYTES = 5 * 1024 * 1024;

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
  const [openSections, setOpenSections] = useState<Set<ArtifactSectionId>>(() => new Set());
  const currentFxRef = useRef<PortFileExplorer | null>(null);
  const openSeq = useRef(0);

  // Artifact data (lazy per session)
  const [plan, setPlan] = useState<PlanShape | null>(null);
  const [todos, setTodos] = useState<TodoItemShape[]>([]);
  const [task, setTask] = useState<TaskShape | null>(null);
  const [memory, setMemory] = useState<string | null>(null);
  const [subagents, setSubagents] = useState<SubagentListItem[]>([]);
  const [worktrees, setWorktrees] = useState<WorktreeInfo[]>([]);
  const [worktreesLoading, setWorktreesLoading] = useState(false);
  const [artifactLoading, setArtifactLoading] = useState(false);
  const [preview, setPreview] = useState<{ section: ArtifactSectionId; node: StructureNode } | null>(null);
  const [projectPreview, setProjectPreview] = useState<ProjectFilePreview | null>(null);

  useEffect(() => {
    if (!open) return;
    const onKey = (e: KeyboardEvent) => {
      const t = e.target;
      if (t instanceof HTMLElement && (t.closest('[data-mille-filter]') || t.tagName === 'INPUT')) {
        return;
      }
      if (e.key === 'Escape') {
        e.preventDefault();
        if (projectPreview) {
          setProjectPreview(null);
          return;
        }
        if (preview) {
          setPreview(null);
          return;
        }
        onClose();
      }
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [open, onClose, preview, projectPreview]);

  // Worktrees follow the open project, not the selected session — a repo has
  // them whether or not a transcript is being read. Kept out of the artifact
  // effect below so switching sessions within one project doesn't re-shell to
  // git for an answer that cannot have changed.
  useEffect(() => {
    if (!open || projectPath === null) {
      setWorktrees([]);
      return;
    }
    let cancelled = false;
    setWorktreesLoading(true);
    void (async () => {
      try {
        const list = await window.spaghetti.getProjectWorktrees(projectPath);
        if (!cancelled) setWorktrees(list);
      } catch {
        // listWorktrees resolves rather than rejects for the expected cases
        // (no repo, no git), so reaching here means the bridge itself failed.
        // An empty list is the right answer either way.
        if (!cancelled) setWorktrees([]);
      } finally {
        if (!cancelled) setWorktreesLoading(false);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [open, projectPath]);

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
    const { projectSlug, sessionId, sourceId, memoryProject } = sessionArtifacts;
    void (async () => {
      try {
        const [p, t, tsk, mem, agents] = await Promise.all([
          window.spaghetti.getSessionPlan(projectSlug, sessionId),
          window.spaghetti.getSessionTodos(projectSlug, sessionId),
          window.spaghetti.getSessionTask(projectSlug, sessionId),
          window.spaghetti.getProjectMemory(memoryProject ?? projectSlug, memoryProject ? undefined : { sourceId }),
          window.spaghetti.getSessionSubagents(projectSlug, sessionId, { sourceId }),
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
  }, [
    sessionArtifacts?.projectSlug,
    sessionArtifacts?.sessionId,
    sessionArtifacts?.sourceId,
    sessionArtifacts?.memoryProject,
  ]);

  useEffect(() => {
    if (!open) {
      void currentFxRef.current?.dispose();
      currentFxRef.current = null;
      setFx(null);
      setRoot(null);
      setStatus('idle');
      setError(null);
      setProjectPreview(null);
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
      setProjectPreview(null);
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
    setProjectPreview(null);

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

  const toggleSection = useCallback((id: ArtifactSectionId) => {
    setOpenSections((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  }, []);

  const openProjectFile = useCallback(
    async (entry: Entry) => {
      if (!fx || !root || entry.kind === 1 || (entry.kind === 2 && entry.symlinkTargetIsDir)) return;

      const absolutePath = resolveEntryPath(fx, root, entry);
      setProjectPreview({ title: entry.name, absolutePath, content: null, loading: true, error: null });

      try {
        if (entry.size > MAX_PREVIEW_BYTES) {
          throw new Error(`This file is larger than the ${MAX_PREVIEW_BYTES / 1024 / 1024} MB preview limit.`);
        }
        const content = await fx.readText(entry.id, 'utf-8');
        if (typeof content !== 'string') throw new Error('The file explorer returned an unreadable response.');
        if (content.includes('\0')) throw new Error('Binary files cannot be shown in the text viewer.');
        setProjectPreview((current) =>
          current?.absolutePath === absolutePath ? { ...current, content, loading: false } : current,
        );
      } catch (e: unknown) {
        const message = e instanceof Error ? e.message : String(e);
        setProjectPreview((current) =>
          current?.absolutePath === absolutePath ? { ...current, loading: false, error: message } : current,
        );
      }
    },
    [fx, root],
  );

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

  const worktreesTree = useMemo((): StructureNode[] => {
    if (worktrees.length === 0) return [];
    return [
      {
        name: 'worktrees',
        type: 'folder',
        isOpen: true,
        children: worktrees.map((w) => ({
          name: worktreeLabel(w),
          type: 'file' as const,
          content: [
            `# ${worktreeLabel(w)}`,
            '',
            `Path: ${w.path}`,
            `Branch: ${w.branchRef ?? (w.detached ? '(detached HEAD)' : '—')}`,
            `HEAD: ${w.head ?? '—'}`,
            `Main worktree: ${w.isMain ? 'yes' : 'no'}`,
            ...(w.locked ? [`Locked: ${w.lockReason ?? '(no reason given)'}`] : []),
            ...(w.prunable ? [`Prunable: ${w.prunableReason ?? '(no reason given)'}`] : []),
          ].join('\n'),
        })),
      },
    ];
  }, [worktrees]);

  const trees: Record<ArtifactSectionId, StructureNode[]> = {
    plans: plansTree,
    todos: todosTree,
    tasks: tasksTree,
    memory: memoryTree,
    subagents: subagentsTree,
    worktrees: worktreesTree,
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

      <div className="flex min-h-0 flex-1 flex-col">
        {/* Project Files is the permanent primary region. */}
        <section className="flex min-h-[180px] flex-1 flex-col border-b border-[color:var(--archive-ink-line-soft)]">
          <div className="flex shrink-0 items-center gap-2 px-4 py-2 font-mono text-[9px] uppercase tracking-widest opacity-70">
            <TerminalSquare size={10} />
            <span>Project Files</span>
          </div>
          <div className="min-h-0 flex-1">
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
              <ExplorerTree fx={fx} onOpenFile={(entry) => void openProjectFile(entry)} />
            ) : (
              <EmptyState title="Waiting for host" detail="Connecting to the file explorer…" />
            )}
          </div>
        </section>

        {/* Bottom-anchored accordion: every tree opens directly below its own switch. */}
        <div className="max-h-[calc(100%-180px)] shrink-0 overflow-y-auto border-t border-[color:var(--archive-ink-line-soft)] bg-[color:var(--archive-paper)] scrollbar-hide">
          {ARTIFACT_SECTIONS.map(({ id, label, scope }) => {
            const nodes = trees[id];
            const isOpen = openSections.has(id);
            // Each section reports against the thing it is actually derived
            // from: a session for the artifact sections, the project itself
            // for worktrees.
            const ready = scope === 'session' ? Boolean(sessionArtifacts) : projectPath !== null;
            const notReadyPrompt = scope === 'session' ? 'Open a session' : 'Select a project';
            const loading = scope === 'session' ? artifactLoading : worktreesLoading;
            return (
              <section key={id} className="border-b border-[color:var(--archive-ink-line-soft)]" data-section={id}>
                <ArtifactToggle label={label} open={isOpen} onToggle={() => toggleSection(id)} />
                {isOpen ? (
                  <div className="border-t border-[color:var(--archive-ink-line-soft)] pb-1">
                    {!ready ? (
                      <p className="px-3 py-2 font-mono text-[9px] tracking-wide opacity-40">{notReadyPrompt}</p>
                    ) : loading ? (
                      <div className="flex items-center gap-2 px-3 py-3 font-mono text-[9px] uppercase tracking-widest opacity-50">
                        <Spinner /> Loading…
                      </div>
                    ) : id === 'worktrees' && nodes.length === 0 ? (
                      // Distinct from "no repo": a single-worktree repository is
                      // the overwhelmingly common case and shouldn't read as an
                      // error or a missing feature.
                      <p className="px-3 py-2 font-mono text-[9px] tracking-wide opacity-40">No linked worktrees</p>
                    ) : (
                      <StructureTree nodes={nodes} onOpenFile={(node) => setPreview({ section: id, node })} />
                    )}
                  </div>
                ) : null}
              </section>
            );
          })}
        </div>
      </div>

      {projectPreview ? (
        <FileViewerDialog
          title={projectPreview.title}
          absolutePath={projectPreview.absolutePath}
          content={projectPreview.content}
          loading={projectPreview.loading}
          error={projectPreview.error}
          onClose={() => setProjectPreview(null)}
        />
      ) : null}

      {preview?.node.content != null ? (
        <FileViewerDialog
          title={preview.node.name}
          content={preview.node.content}
          collection="Session Archive / Indexed"
          description="A preserved working artifact indexed for this local archive session."
          onClose={() => setPreview(null)}
        />
      ) : null}
    </aside>
  );
}

/**
 * Row label for one worktree.
 *
 * Branch name leads because that is how anyone refers to a worktree in
 * practice; the directory basename is frequently just the branch again, or an
 * opaque temp path for an agent-created one. Falls back to a short SHA when
 * detached, since "no branch" alone would make several rows indistinguishable.
 */
function worktreeLabel(w: WorktreeInfo): string {
  const base = w.bare ? '(bare)' : (w.branch ?? (w.head !== null ? `(detached ${w.head.slice(0, 7)})` : '(detached)'));
  const flags = [w.isMain ? 'main' : null, w.locked ? 'locked' : null, w.prunable ? 'prunable' : null].filter(
    (f): f is string => f !== null,
  );
  return flags.length > 0 ? `${base} · ${flags.join(' · ')}` : base;
}

function ArtifactToggle({ label, open, onToggle }: { label: string; open: boolean; onToggle: () => void }) {
  return (
    <button
      type="button"
      onClick={onToggle}
      aria-expanded={open}
      className="flex w-full cursor-pointer items-center justify-between border-0 bg-transparent px-4 py-2 font-mono text-[9px] tracking-widest text-ink opacity-70 transition-[opacity,background-color] hover:bg-ink/[0.04] hover:opacity-100"
    >
      <span className="flex min-w-0 items-center gap-2 uppercase">
        <TerminalSquare size={10} />
        <span className="truncate">{label}</span>
      </span>
      {open ? <ChevronDown size={12} /> : <ChevronRight size={12} />}
    </button>
  );
}

/** Mille tree only — no filter/collapse chrome or project name strip. */
function ExplorerTree({ fx, onOpenFile }: { fx: PortFileExplorer; onOpenFile: (entry: Entry) => void }) {
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
          <FileTree
            ref={treeRef}
            ariaLabel="Project files"
            iconTheme={minimalIconTheme}
            rowHeight={26}
            overscan={24}
            onOpen={onOpenFile}
          />
        </div>
      </div>
    </FileTreeProvider>
  );
}

function resolveEntryPath(fx: PortFileExplorer, root: string, entry: Entry): string {
  const snapshot = fx.getSnapshot();
  const segments: string[] = [];
  const seen = new Set<number>();
  let current: Entry | null = entry;

  // Mille's native bridge may represent a root's absent parent as
  // `undefined` at runtime even though the public type says `null`.
  while (current && current.parentId !== null && current.parentId !== undefined && !seen.has(current.id)) {
    seen.add(current.id);
    segments.unshift(...(current.pathSegments?.length ? current.pathSegments : [current.name]));
    current = snapshot.getById(current.parentId) ?? null;
  }

  return `${root.replace(/\/+$/, '')}/${segments.join('/')}`;
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

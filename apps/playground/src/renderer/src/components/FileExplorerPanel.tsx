/**
 * Right Structure panel (design mock).
 * - Project Files: mille tree for selected project's absolutePath
 * - Session artifacts (plan / todos / task / subagents / memory) when a session is open
 */

import { useEffect, useMemo, useRef, useState } from 'react';
import { ChevronDown, ChevronRight, TerminalSquare, X } from 'lucide-react';
import type { FileExplorer } from '@vibecook/mille';
import { connectFileExplorer, type PortFileExplorer } from '@vibecook/mille/port';
import { FileTreeProvider, FileTree, useFileTreeRef } from '@vibecook/mille-ui';
import { duotoneIconTheme } from '@vibecook/mille-ui/icons';
import { createCommandRegistry, defaultCommands } from '@vibecook/mille-ui/commands';
import '@vibecook/mille-ui/tokens.css';
import { onFxPort } from '../lib/fx-port.js';
import { EmptyState, Spinner } from './ui.js';
import { ArtifactPanel, type ArtifactTab } from './ArtifactPanel.js';

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

type StructureSection = 'project-files' | 'artifacts';

export function FileExplorerPanel({
  open,
  onClose,
  projectPath,
  projectLabel,
  isDark = true,
  sessionArtifacts = null,
}: FileExplorerPanelProps) {
  const [fx, setFx] = useState<PortFileExplorer | null>(null);
  const [root, setRoot] = useState<string | null>(null);
  const [status, setStatus] = useState<'idle' | 'opening' | 'ready' | 'error'>('idle');
  const [error, setError] = useState<string | null>(null);
  const [openSections, setOpenSections] = useState<Set<StructureSection>>(() => new Set(['project-files']));
  const [artifactTab, setArtifactTab] = useState<ArtifactTab>('plan');
  const currentFxRef = useRef<PortFileExplorer | null>(null);
  const openSeq = useRef(0);

  useEffect(() => {
    if (!open) return;
    const onKey = (e: KeyboardEvent) => {
      const t = e.target;
      if (t instanceof HTMLElement && (t.closest('[data-mille-filter]') || t.tagName === 'INPUT')) {
        return;
      }
      if (e.key === 'Escape') {
        e.preventDefault();
        onClose();
      }
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [open, onClose]);

  // Auto-open artifacts section when session selected
  useEffect(() => {
    if (sessionArtifacts) {
      setOpenSections((prev) => new Set([...prev, 'artifacts']));
    }
  }, [sessionArtifacts?.sessionId]);

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

  const toggleSection = (id: StructureSection) => {
    setOpenSections((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  };

  if (!open) return null;

  return (
    <aside
      className="w-64 border-l border-ink/20 flex flex-col shrink-0 bg-transparent min-h-0 z-20"
      data-theme={isDark ? 'dark' : 'light'}
      aria-label="Structure"
    >
      <div className="h-10 border-b border-ink/10 flex items-center justify-between px-4 shrink-0">
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
        {/* Project Files */}
        <section className="border-b border-ink/10">
          <button
            type="button"
            onClick={() => toggleSection('project-files')}
            aria-expanded={openSections.has('project-files')}
            className="flex w-full items-center justify-between px-4 py-2 font-mono text-[9px] tracking-widest opacity-70 transition-opacity hover:opacity-100 bg-transparent border-0 text-ink cursor-pointer"
          >
            <span className="flex min-w-0 items-center gap-2 uppercase">
              <TerminalSquare size={10} />
              <span className="truncate">Project Files</span>
            </span>
            {openSections.has('project-files') ? <ChevronDown size={12} /> : <ChevronRight size={12} />}
          </button>
          {openSections.has('project-files') && (
            <div className="min-h-[200px] h-[min(40vh,320px)] flex flex-col border-t border-ink/5">
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
                <ExplorerTree fx={fx} root={root} label={projectLabel} />
              ) : (
                <EmptyState title="Waiting for host" detail="Connecting to the file explorer…" />
              )}
            </div>
          )}
        </section>

        {/* Session artifacts */}
        {sessionArtifacts ? (
          <section className="border-b border-ink/10 flex flex-col min-h-0">
            <button
              type="button"
              onClick={() => toggleSection('artifacts')}
              aria-expanded={openSections.has('artifacts')}
              className="flex w-full items-center justify-between px-4 py-2 font-mono text-[9px] tracking-widest opacity-70 transition-opacity hover:opacity-100 bg-transparent border-0 text-ink cursor-pointer"
            >
              <span className="flex min-w-0 items-center gap-2 uppercase">
                <TerminalSquare size={10} />
                <span className="truncate">Session artifacts</span>
              </span>
              {openSections.has('artifacts') ? <ChevronDown size={12} /> : <ChevronRight size={12} />}
            </button>
            {openSections.has('artifacts') && (
              <div className="min-h-[240px] max-h-[45vh] flex flex-col border-t border-ink/5">
                <div className="flex gap-0.5 px-2 py-1.5 flex-wrap border-b border-ink/5">
                  {(
                    [
                      ['plan', 'Plan', !!sessionArtifacts.hints.planSlug],
                      ['todos', 'Todos', sessionArtifacts.hints.todoCount > 0],
                      ['task', 'Task', sessionArtifacts.hints.hasTask],
                      ['subagents', 'Agents', false],
                      ['memory', 'Memory', !!sessionArtifacts.hints.hasMemory],
                    ] as const
                  ).map(([id, label, hint]) => (
                    <button
                      key={id}
                      type="button"
                      onClick={() => setArtifactTab(id)}
                      className={`font-mono text-[9px] tracking-wide px-1.5 py-0.5 border-b cursor-pointer bg-transparent transition-colors ${
                        artifactTab === id ? 'border-ink text-ink' : 'border-transparent text-ink/40 hover:text-ink/70'
                      }`}
                    >
                      {label}
                      {hint ? <span className="ml-1 text-sanguine">·</span> : null}
                    </button>
                  ))}
                </div>
                <div className="flex-1 min-h-0 overflow-hidden">
                  <ArtifactPanel
                    open
                    embedded
                    onClose={() => {}}
                    projectSlug={sessionArtifacts.projectSlug}
                    sourceId={sessionArtifacts.sourceId}
                    sessionId={sessionArtifacts.sessionId}
                    hints={sessionArtifacts.hints}
                    initialTab={artifactTab}
                  />
                </div>
              </div>
            )}
          </section>
        ) : null}
      </div>
    </aside>
  );
}

function ExplorerTree({ fx, root, label }: { fx: PortFileExplorer; root: string; label?: string | null }) {
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
      <div className="flex flex-col h-full min-h-0 mille-panel">
        <div className="px-2 py-1 border-b border-ink/5 flex items-center gap-1 shrink-0">
          <button
            type="button"
            className="font-mono text-[9px] tracking-widest uppercase px-1 py-0.5 text-ink/45 hover:text-ink/80 cursor-pointer bg-transparent border-0"
            title="Filter tree"
            onClick={() => treeRef.current?.focusFilter()}
          >
            ⌕
          </button>
          <button
            type="button"
            className="font-mono text-[9px] tracking-widest uppercase px-1 py-0.5 text-ink/45 hover:text-ink/80 cursor-pointer bg-transparent border-0"
            title="Collapse all"
            onClick={() => treeRef.current?.reset()}
          >
            ⊟
          </button>
          <span className="font-mono text-[8px] text-ink/30 ml-auto truncate max-w-[140px]" title={root}>
            {label || basename(root)}
          </span>
        </div>
        <div className="flex-1 min-h-0 overflow-hidden">
          <FileTree ref={treeRef} ariaLabel="Project files" iconTheme={duotoneIconTheme} rowHeight={20} overscan={24} />
        </div>
      </div>
    </FileTreeProvider>
  );
}

function basename(path: string): string {
  const parts = path.replace(/\\/g, '/').split('/').filter(Boolean);
  return parts[parts.length - 1] ?? path;
}

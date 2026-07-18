/**
 * Rightmost mille file-tree panel.
 *
 * Opens the selected project's `absolutePath` via UtilityProcess + MessagePort
 * (`window.mille.openWorkspace`). Toggled from the shell header.
 */

import { useEffect, useMemo, useRef, useState } from 'react';
import type { FileExplorer } from '@vibecook/mille';
import { connectFileExplorer, type PortFileExplorer } from '@vibecook/mille/port';
import { FileTreeProvider, FileTree, useFileTreeRef } from '@vibecook/mille-ui';
import { duotoneIconTheme } from '@vibecook/mille-ui/icons';
import { createCommandRegistry, defaultCommands } from '@vibecook/mille-ui/commands';
import '@vibecook/mille-ui/tokens.css';
import { onFxPort } from '../lib/fx-port.js';
import { Btn, EmptyState, Spinner } from './ui.js';

export interface FileExplorerPanelProps {
  open: boolean;
  onClose: () => void;
  /** Absolute path of the selected project folder (from ProjectListItem). */
  projectPath: string | null;
  projectLabel?: string | null;
}

export function FileExplorerPanel({ open, onClose, projectPath, projectLabel }: FileExplorerPanelProps) {
  const [fx, setFx] = useState<PortFileExplorer | null>(null);
  const [root, setRoot] = useState<string | null>(null);
  const [status, setStatus] = useState<'idle' | 'opening' | 'ready' | 'error'>('idle');
  const [error, setError] = useState<string | null>(null);
  const currentFxRef = useRef<PortFileExplorer | null>(null);
  const openSeq = useRef(0);

  // Escape closes panel
  useEffect(() => {
    if (!open) return;
    const onKey = (e: KeyboardEvent) => {
      // Don't steal Escape from tree rename / filter
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

  // Listen for ports first, then open the workspace (order matters —
  // same-root re-attach posts the port immediately).
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
      // Drop the client when path/panel changes; the next attach creates a fresh one.
      void currentFxRef.current?.dispose();
      currentFxRef.current = null;
      setFx(null);
    };
  }, [open, projectPath]);

  // Dispose on unmount
  useEffect(() => {
    return () => {
      void currentFxRef.current?.dispose();
      currentFxRef.current = null;
      void window.mille?.closeWorkspace().catch(() => {});
    };
  }, []);

  if (!open) return null;

  return (
    <aside
      className="w-[300px] shrink-0 border-l border-white/10 bg-[#0b0b0b] flex flex-col min-h-0"
      data-theme="dark"
      aria-label="Project files"
    >
      <header className="flex items-center gap-2 px-3 py-2 border-b border-white/10 shrink-0">
        <div className="min-w-0 flex-1">
          <div className="text-[10px] tracking-[0.14em] uppercase text-white/40">Files</div>
          <div className="text-[11px] text-white/75 truncate font-mono" title={projectPath ?? undefined}>
            {projectLabel || (projectPath ? basename(projectPath) : 'No project')}
          </div>
        </div>
        <Btn onClick={onClose} className="!px-2" title="Close files (Esc)">
          Close
        </Btn>
      </header>

      <div className="flex-1 min-h-0 overflow-hidden flex flex-col">
        {!projectPath ? (
          <EmptyState title="Select a project" detail="The file tree opens the project folder on disk." />
        ) : status === 'opening' || (status === 'ready' && !fx) ? (
          <div className="flex items-center justify-center gap-2 py-12 text-xs text-white/40">
            <Spinner />
            Opening folder…
          </div>
        ) : status === 'error' ? (
          <EmptyState title="Could not open folder" detail={error ?? 'Unknown error'} />
        ) : fx && root ? (
          <ExplorerTree fx={fx} root={root} />
        ) : (
          <EmptyState title="Waiting for host" detail="Connecting to the file explorer process…" />
        )}
      </div>
    </aside>
  );
}

function ExplorerTree({ fx, root }: { fx: PortFileExplorer; root: string }) {
  const commands = useMemo(() => createCommandRegistry(defaultCommands), []);
  const treeRef = useFileTreeRef();
  const expandedRootsRef = useRef(false);

  // Expand workspace roots once they appear in the snapshot
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
        <div className="px-2 py-1 border-b border-white/6 flex items-center gap-1 shrink-0">
          <button
            type="button"
            className="text-[10px] px-1.5 py-0.5 rounded border border-white/10 text-white/45 hover:text-white/75 hover:bg-white/[0.04] cursor-pointer bg-transparent"
            title="Filter tree (⌘F)"
            onClick={() => treeRef.current?.focusFilter()}
          >
            ⌕ Filter
          </button>
          <button
            type="button"
            className="text-[10px] px-1.5 py-0.5 rounded border border-white/10 text-white/45 hover:text-white/75 hover:bg-white/[0.04] cursor-pointer bg-transparent"
            title="Collapse all"
            onClick={() => treeRef.current?.reset()}
          >
            ⊟
          </button>
          <span className="text-[9px] font-mono text-white/25 ml-auto truncate max-w-[120px]" title={root}>
            {basename(root)}
          </span>
        </div>
        <div className="flex-1 min-h-0 overflow-hidden text-[12px] text-white/85">
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

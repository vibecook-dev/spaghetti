/**
 * Lightweight archive file tree (design mock FileTreeNode).
 * [+]/[-] folders, mono type — no tool chrome.
 */

import { useState, type ReactNode } from 'react';
import * as Dialog from '@radix-ui/react-dialog';
import { X } from 'lucide-react';

export interface StructureNode {
  name: string;
  type: 'folder' | 'file';
  isOpen?: boolean;
  /** Selected / active file — inverted ink selection like design FileTreeNode. */
  active?: boolean;
  /** Optional body shown when file is opened. */
  content?: string;
  children?: StructureNode[];
}

export function StructureTree({
  nodes,
  onOpenFile,
}: {
  nodes: StructureNode[];
  onOpenFile?: (node: StructureNode) => void;
}) {
  if (nodes.length === 0) {
    return <p className="px-3 py-2 font-mono text-[9px] tracking-wide opacity-40">— empty —</p>;
  }
  return (
    <div className="px-2 py-1">
      {nodes.map((node, i) => (
        <StructureTreeNode key={`${node.name}-${i}`} node={node} depth={0} onOpenFile={onOpenFile} />
      ))}
    </div>
  );
}

function StructureTreeNode({
  node,
  depth,
  onOpenFile,
}: {
  node: StructureNode;
  depth: number;
  onOpenFile?: (node: StructureNode) => void;
}) {
  const [isOpen, setIsOpen] = useState(node.isOpen ?? true);
  const isFolder = node.type === 'folder';

  return (
    <div className="font-mono text-[10px] tracking-tight">
      <div
        className="flex items-center py-1 px-2 cursor-pointer hover:bg-ink/[0.05] transition-colors data-[active=true]:bg-ink data-[active=true]:text-paper-bright"
        style={{ paddingLeft: `${depth * 12 + 8}px` }}
        data-active={node.active ? 'true' : undefined}
        onClick={() => {
          if (isFolder) setIsOpen((v) => !v);
        }}
        onDoubleClick={() => {
          if (!isFolder) onOpenFile?.(node);
        }}
        title={!isFolder ? 'Double-click to view file' : undefined}
        role="treeitem"
        aria-expanded={isFolder ? isOpen : undefined}
      >
        <span className="w-4 flex justify-center opacity-60 mr-1 shrink-0">
          {isFolder ? (isOpen ? '[-]' : '[+]') : ''}
        </span>
        {/* Design FileTreeNode: folders uppercase tracking-widest 9px; files inherit 10px tracking-tight */}
        <span className={`truncate min-w-0 ${isFolder ? 'uppercase tracking-widest text-[9px]' : ''}`}>
          {node.name}
        </span>
      </div>
      {isFolder && isOpen && node.children && node.children.length > 0 ? (
        <div className="border-l border-dashed border-[color:var(--archive-ink-line)] ml-3">
          {node.children.map((child, idx) => (
            <StructureTreeNode key={`${child.name}-${idx}`} node={child} depth={depth + 1} onOpenFile={onOpenFile} />
          ))}
        </div>
      ) : null}
    </div>
  );
}

/**
 * Archive catalogue preview — mirrors the reference's modal file viewer.
 * Keeping this in a portal also prevents an opened artifact from compressing
 * the narrow Structure column.
 */
export function StructureFilePreview({
  title,
  content,
  onClose,
}: {
  title: string;
  content: string;
  onClose: () => void;
}): ReactNode {
  const lineCount = content ? content.split('\n').length : 0;
  const reference = `A-${
    title
      .replace(/[^a-z0-9]/gi, '')
      .slice(0, 6)
      .toUpperCase() || '000000'
  }`;

  return (
    <Dialog.Root open onOpenChange={(open) => !open && onClose()}>
      <Dialog.Portal>
        <Dialog.Overlay className="fixed inset-0 z-50 bg-[#11100f]/72 backdrop-blur-[3px]" />
        <Dialog.Content className="fixed left-1/2 top-1/2 z-50 flex h-[min(46rem,86vh)] w-[min(54rem,calc(100vw-2rem))] -translate-x-1/2 -translate-y-1/2 flex-col border border-[#2b2623]/60 bg-[#f8f6f0] text-[#2b2623] shadow-2xl outline-none dark:border-[#d4cbbd]/45 dark:bg-[#171615] dark:text-[#d4cbbd]">
          <div className="flex items-center justify-between border-b border-[#2b2623]/20 px-6 py-3 dark:border-[#d4cbbd]/20">
            <span className="font-mono text-[9px] tracking-[0.18em] opacity-55">SPAGHETTI ARCHIVE · FILE VIEWER</span>
            <Dialog.Close asChild>
              <button
                type="button"
                className="flex items-center gap-2 border-0 bg-transparent font-mono text-[9px] tracking-[0.14em] text-inherit opacity-60 transition-opacity hover:opacity-100"
                aria-label="Close file viewer"
              >
                <span>CLOSE</span>
                <X size={14} />
              </button>
            </Dialog.Close>
          </div>

          <div className="grid shrink-0 gap-6 px-6 py-6 md:grid-cols-[9.5rem_1fr]">
            <div className="border-r border-[#2b2623]/20 pr-5 font-mono text-[9px] tracking-[0.12em] dark:border-[#d4cbbd]/20">
              <p className="mb-5 opacity-50">CATALOGUE ENTRY</p>
              <dl className="space-y-4 leading-relaxed">
                <div>
                  <dt className="opacity-45">COLLECTION</dt>
                  <dd className="mt-0.5 text-[10px] tracking-[0.06em]">Session Archive / Indexed</dd>
                </div>
                <div>
                  <dt className="opacity-45">REFERENCE</dt>
                  <dd className="mt-0.5 text-[10px] tracking-[0.06em]">{reference}</dd>
                </div>
                <div>
                  <dt className="opacity-45">FORMAT</dt>
                  <dd className="mt-0.5 text-[10px] tracking-[0.06em]">UTF-8 text</dd>
                </div>
              </dl>
            </div>
            <div className="min-w-0">
              <Dialog.Title className="font-serif text-[clamp(2rem,5vw,3.35rem)] font-medium leading-[0.92] tracking-[-0.02em]">
                {title}
              </Dialog.Title>
              <Dialog.Description className="mt-4 max-w-xl font-serif text-[15px] leading-relaxed opacity-70">
                A preserved working artifact indexed for this local archive session.
              </Dialog.Description>
              <div className="mt-5 flex items-center gap-3 border-t border-[#2b2623]/20 pt-3 font-mono text-[9px] tracking-[0.12em] opacity-55 dark:border-[#d4cbbd]/20">
                <span>READ-ONLY</span>
                <span>·</span>
                <span>{lineCount} LINES</span>
                <span>·</span>
                <span>UTF-8 TEXT</span>
              </div>
            </div>
          </div>

          <div className="mx-6 min-h-0 flex-1 overflow-hidden border border-[#2b2623]/20 bg-[#eee9de] dark:border-[#d4cbbd]/20 dark:bg-[#0f0e0d]">
            <pre className="h-full overflow-auto whitespace-pre-wrap p-5 font-mono text-[12px] leading-6 scrollbar-hide">
              {content || '(empty)'}
            </pre>
          </div>

          <div className="mx-6 mt-4 flex items-center justify-between border-t border-[#2b2623]/20 py-3 font-mono text-[9px] tracking-[0.12em] opacity-50 dark:border-[#d4cbbd]/20">
            <span>LOCAL SESSION ARCHIVE</span>
            <span>ESC TO RETURN</span>
          </div>
        </Dialog.Content>
      </Dialog.Portal>
    </Dialog.Root>
  );
}

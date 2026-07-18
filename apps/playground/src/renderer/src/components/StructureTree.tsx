/**
 * Lightweight archive file tree (design mock FileTreeNode).
 * [+]/[-] folders, mono type — no tool chrome.
 */

import { useState } from 'react';

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

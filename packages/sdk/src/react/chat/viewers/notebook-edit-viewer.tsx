/**
 * NotebookEditViewer Component
 *
 * Displays Jupyter notebook edit operations.
 */

import React, { memo } from 'react';
import { FileCode2 } from 'lucide-react';
import { cn } from '../../lib/utils.js';
import { chatCardVariants, chatCardHeaderVariants, badgeVariants } from '../variants';
import { toolColors } from '../theme';

interface NotebookEditViewerProps {
  input: Record<string, unknown>;
}

const notebookColor = toolColors.notebook;

export const NotebookEditViewer = memo(function NotebookEditViewer({ input }: NotebookEditViewerProps) {
  const notebookPath = String(input.notebook_path || '');
  const cellId = input.cell_id as string | undefined;
  const cellType = (input.cell_type as string) || 'code';
  const editMode = (input.edit_mode as string) || 'replace';
  const newSource = String(input.new_source || '');

  const fileName = notebookPath.split('/').pop() || notebookPath;
  const directory = notebookPath.split('/').slice(0, -1).join('/') || '.';

  const getModeLabel = () => {
    switch (editMode) {
      case 'insert':
        return 'Insert Cell';
      case 'delete':
        return 'Delete Cell';
      default:
        return 'Replace Cell';
    }
  };

  const getCellTypeLabel = () => (cellType === 'markdown' ? 'Markdown' : 'Code');

  return (
    <div className={cn(chatCardVariants())}>
      <div className={cn(chatCardHeaderVariants())}>
        <FileCode2 size={14} style={{ color: notebookColor }} />
        <span className="text-[11px] font-bold text-foreground">{getModeLabel()}</span>
        <span
          className={cn(badgeVariants({ variant: 'colored' }))}
          style={{ backgroundColor: `${notebookColor}15`, color: notebookColor }}
        >
          {getCellTypeLabel()}
        </span>
        {cellId && <span className={cn(badgeVariants())}>Cell: {cellId}</span>}
      </div>

      {/* Notebook path */}
      <div className="px-3 py-2">
        <div className="font-mono text-[11px] px-2 py-1.5 rounded flex items-center gap-2 bg-card border border-border">
          {directory !== '.' && <span className="text-muted-foreground opacity-60">{directory}/</span>}
          <span className="font-bold" style={{ color: notebookColor }}>
            {fileName}
          </span>
        </div>
      </div>

      {/* Source preview */}
      {newSource && editMode !== 'delete' && (
        <div className="px-3 pb-2">
          <div className="font-mono text-[10px] p-2 rounded max-h-32 overflow-auto bg-card border border-border text-muted-foreground">
            <pre className="whitespace-pre-wrap">{newSource}</pre>
          </div>
        </div>
      )}
    </div>
  );
});

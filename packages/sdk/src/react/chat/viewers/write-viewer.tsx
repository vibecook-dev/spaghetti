/**
 * WriteViewer Component
 */

import React, { memo } from 'react';
import { FilePenLine } from 'lucide-react';
import { cn } from '../../lib/utils.js';
import { chatCardVariants, chatCardHeaderVariants, badgeVariants } from '../variants';
import { toolColors, getFileTypeColor } from '../theme';

interface WriteViewerProps {
  input: Record<string, unknown>;
}

export const WriteViewer = memo(function WriteViewer({ input }: WriteViewerProps) {
  const filePath = String(input.file_path || '');
  const content = String(input.content || '');

  const parts = filePath.split('/');
  const fileName = parts.pop() || filePath;
  const directory = parts.join('/') || '.';
  const fileColor = getFileTypeColor(fileName);
  const lineCount = content.split('\n').length;

  return (
    <div className={cn(chatCardVariants())}>
      <div className={cn(chatCardHeaderVariants())}>
        <FilePenLine size={14} style={{ color: toolColors.write }} />
        <span className="text-[11px] font-bold text-foreground">Write File</span>
        <span
          className={cn(badgeVariants({ variant: 'colored' }))}
          style={{ backgroundColor: `${toolColors.write}15`, color: toolColors.write }}
        >
          {lineCount} lines
        </span>
      </div>
      <div className="px-3 py-2">
        <div className="font-mono text-[11px] px-2 py-1.5 rounded flex items-center gap-2 bg-card border border-border">
          {directory !== '.' && <span className="text-muted-foreground opacity-60">{directory}/</span>}
          <span className="font-bold" style={{ color: fileColor }}>
            {fileName}
          </span>
        </div>
      </div>
    </div>
  );
});

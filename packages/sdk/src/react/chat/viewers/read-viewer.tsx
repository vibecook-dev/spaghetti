/**
 * ReadViewer Component
 */

import React, { memo } from 'react';
import { FileText } from 'lucide-react';
import { cn } from '../../lib/utils.js';
import { chatCardVariants, chatCardHeaderVariants, badgeVariants } from '../variants';
import { toolColors, getFileTypeColor } from '../theme';

interface ReadViewerProps {
  input: Record<string, unknown>;
}

export const ReadViewer = memo(function ReadViewer({ input }: ReadViewerProps) {
  const filePath = String(input.file_path || '');
  const offset = input.offset as number | undefined;
  const limit = input.limit as number | undefined;

  const parts = filePath.split('/');
  const fileName = parts.pop() || filePath;
  const directory = parts.join('/') || '.';
  const fileColor = getFileTypeColor(fileName);

  const getLineRangeText = () => {
    if (offset !== undefined && limit !== undefined) return `Lines ${offset + 1}-${offset + limit}`;
    if (offset !== undefined) return `From line ${offset + 1}`;
    if (limit !== undefined) return `First ${limit} lines`;
    return null;
  };

  const lineRangeText = getLineRangeText();

  return (
    <div className={cn(chatCardVariants())}>
      <div className={cn(chatCardHeaderVariants())}>
        <FileText size={14} style={{ color: toolColors.read }} />
        <span className="text-[11px] font-bold text-foreground">Read File</span>
        {lineRangeText && (
          <span
            className={cn(badgeVariants({ variant: 'colored' }))}
            style={{ backgroundColor: `${toolColors.read}15`, color: toolColors.read }}
          >
            {lineRangeText}
          </span>
        )}
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

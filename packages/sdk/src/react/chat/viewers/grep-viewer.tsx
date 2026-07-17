/**
 * GrepViewer Component
 *
 * Displays search query operations with filter badges.
 */

import React, { memo } from 'react';
import { Search } from 'lucide-react';
import { cn } from '../../lib/utils.js';
import { chatCardVariants, chatCardHeaderVariants, badgeVariants } from '../variants';
import { toolColors } from '../theme';

interface GrepViewerProps {
  input: Record<string, unknown>;
}

export const GrepViewer = memo(function GrepViewer({ input }: GrepViewerProps) {
  const pattern = String(input.pattern || '');
  const path = String(input.path || '.');
  const glob = input.glob ? String(input.glob) : null;
  const fileType = input.type ? String(input.type) : null;
  const outputMode = String(input.output_mode || 'files_with_matches');
  const caseInsensitive = Boolean(input['-i']);
  const contextBefore = input['-B'] as number | undefined;
  const contextAfter = input['-A'] as number | undefined;
  const contextAround = input['-C'] as number | undefined;
  const multiline = Boolean(input.multiline);
  const headLimit = input.head_limit as number | undefined;

  const outputModeLabel = outputMode === 'content' ? 'Lines' : outputMode === 'count' ? 'Count' : 'Files';

  return (
    <div className={cn(chatCardVariants())}>
      {/* Search bar style header */}
      <div className={cn(chatCardHeaderVariants())}>
        <Search size={14} style={{ color: toolColors.search }} />
        <div
          className="flex-grow font-mono text-[12px] font-bold px-2 py-1 rounded bg-background"
          style={{ color: toolColors.search, border: `1px solid ${toolColors.search}40` }}
        >
          {pattern}
        </div>
      </div>

      {/* Details */}
      <div className="px-3 py-2 space-y-2">
        {/* Path */}
        <div className="flex items-center gap-2 text-[10px]">
          <span className="text-muted-foreground">in</span>
          <span className="font-mono px-1.5 py-0.5 rounded bg-card border border-border text-foreground">{path}</span>
        </div>

        {/* Filters row */}
        <div className="flex items-center gap-2 flex-wrap">
          <span
            className={cn(badgeVariants({ variant: 'colored' }))}
            style={{
              backgroundColor: `${toolColors.search}15`,
              color: toolColors.search,
              border: `1px solid ${toolColors.search}30`,
            }}
          >
            → {outputModeLabel}
          </span>

          {glob && <span className={cn(badgeVariants())}>glob: {glob}</span>}
          {fileType && <span className={cn(badgeVariants())}>type: {fileType}</span>}

          {caseInsensitive && (
            <span
              className={cn(badgeVariants({ variant: 'colored' }))}
              style={{ backgroundColor: 'rgba(139, 92, 246, 0.1)', color: '#8b5cf6' }}
            >
              -i
            </span>
          )}
          {multiline && (
            <span
              className={cn(badgeVariants({ variant: 'colored' }))}
              style={{ backgroundColor: 'rgba(139, 92, 246, 0.1)', color: '#8b5cf6' }}
            >
              multiline
            </span>
          )}

          {contextAround && <span className={cn(badgeVariants())}>-C {contextAround}</span>}
          {contextBefore && !contextAround && <span className={cn(badgeVariants())}>-B {contextBefore}</span>}
          {contextAfter && !contextAround && <span className={cn(badgeVariants())}>-A {contextAfter}</span>}
          {headLimit && <span className={cn(badgeVariants())}>limit: {headLimit}</span>}
        </div>
      </div>
    </div>
  );
});

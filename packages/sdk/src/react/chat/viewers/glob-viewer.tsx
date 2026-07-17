/**
 * GlobViewer Component
 *
 * Displays file pattern search with wildcard highlighting.
 */

import React, { memo } from 'react';
import { FileText } from 'lucide-react';
import { cn } from '../../lib/utils.js';
import { chatCardVariants, chatCardHeaderVariants } from '../variants';
import { toolColors, syntaxColors } from '../theme';

interface GlobViewerProps {
  input: Record<string, unknown>;
}

function highlightPattern(pattern: string): React.ReactNode[] {
  const parts = pattern.split(/(\*\*|\*|\?)/g);
  return parts.map((part, i) => {
    if (part === '**' || part === '*' || part === '?') {
      return (
        <span key={i} className="font-bold" style={{ color: syntaxColors.wildcard }}>
          {part}
        </span>
      );
    }
    return <span key={i}>{part}</span>;
  });
}

export const GlobViewer = memo(function GlobViewer({ input }: GlobViewerProps) {
  const pattern = String(input.pattern || '');
  const path = input.path ? String(input.path) : '.';

  return (
    <div className={cn(chatCardVariants())}>
      <div className={cn(chatCardHeaderVariants())}>
        <FileText size={14} style={{ color: toolColors.search }} />
        <span className="text-[11px] font-bold text-foreground">Find Files</span>
      </div>

      <div className="px-3 py-2 space-y-2">
        {/* Pattern */}
        <div className="font-mono text-[12px] px-2 py-1.5 rounded flex items-center gap-2 bg-card border border-border text-foreground">
          <span className="text-muted-foreground">pattern:</span>
          <span className="font-bold">{highlightPattern(pattern)}</span>
        </div>

        {/* Path */}
        {path !== '.' && (
          <div className="flex items-center gap-2 text-[10px]">
            <span className="text-muted-foreground">in</span>
            <span className="font-mono px-1.5 py-0.5 rounded bg-card border border-border text-foreground">{path}</span>
          </div>
        )}
      </div>
    </div>
  );
});

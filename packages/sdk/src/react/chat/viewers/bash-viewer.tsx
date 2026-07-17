/**
 * BashViewer Component
 *
 * Displays bash command execution with syntax highlighting.
 */

import React, { memo } from 'react';
import { Terminal } from 'lucide-react';
import { cn } from '../../lib/utils.js';
import { chatCardVariants, chatCardHeaderVariants, badgeVariants } from '../variants';
import { toolColors, syntaxColors } from '../theme';

interface BashViewerProps {
  input: Record<string, unknown>;
}

function highlightCommand(cmd: string): React.ReactNode[] {
  const parts = cmd.split(/(\s+)/);
  let isFirstWord = true;

  return parts.map((part, i) => {
    if (part.trim() === '') return <span key={i}>{part}</span>;
    if (part.startsWith('-'))
      return (
        <span key={i} style={{ color: syntaxColors.flag }}>
          {part}
        </span>
      );
    if (part.startsWith('"') || part.startsWith("'"))
      return (
        <span key={i} style={{ color: syntaxColors.string }}>
          {part}
        </span>
      );
    if (part === '|' || part === '>' || part === '>>' || part === '<' || part === '&&' || part === '||') {
      return (
        <span key={i} className="font-bold" style={{ color: syntaxColors.operator }}>
          {part}
        </span>
      );
    }
    if (isFirstWord) {
      isFirstWord = false;
      return (
        <span key={i} className="font-bold" style={{ color: syntaxColors.command }}>
          {part}
        </span>
      );
    }
    if (part.includes('/') || part.includes('.'))
      return (
        <span key={i} style={{ color: syntaxColors.path }}>
          {part}
        </span>
      );
    return <span key={i}>{part}</span>;
  });
}

export const BashViewer = memo(function BashViewer({ input }: BashViewerProps) {
  const command = String(input.command || '');
  const description = input.description ? String(input.description) : null;
  const timeout = input.timeout as number | undefined;
  const runInBackground = Boolean(input.run_in_background);

  const formatTimeout = (ms: number) => {
    if (ms >= 60000) return `${(ms / 60000).toFixed(0)}m`;
    return `${(ms / 1000).toFixed(0)}s`;
  };

  return (
    <div className={cn(chatCardVariants())}>
      <div className={cn(chatCardHeaderVariants())}>
        <Terminal size={14} style={{ color: toolColors.bash }} />
        <span className="text-[11px] font-bold text-foreground">{description || 'Execute Command'}</span>
        <div className="ml-auto flex items-center gap-2">
          {runInBackground && (
            <span
              className={cn(badgeVariants({ variant: 'colored' }))}
              style={{ backgroundColor: 'rgba(139, 92, 246, 0.1)', color: '#8b5cf6' }}
            >
              background
            </span>
          )}
          {timeout && <span className={cn(badgeVariants())}>{formatTimeout(timeout)} timeout</span>}
        </div>
      </div>
      <div className="px-3 py-2" style={{ backgroundColor: '#1e1e1e' }}>
        <div className="font-mono text-[11px] leading-relaxed">
          <span style={{ color: '#4ade80' }}>&#10095; </span>
          <span style={{ color: '#e5e5e5' }}>{highlightCommand(command)}</span>
        </div>
      </div>
    </div>
  );
});

/**
 * TaskViewer Component
 *
 * Displays agent spawn/task operations.
 */

import React, { memo } from 'react';
import { Cpu, Play } from 'lucide-react';
import { cn } from '../../lib/utils.js';
import { chatCardVariants, chatCardHeaderVariants, badgeVariants } from '../variants';
import { toolColors } from '../theme';

interface TaskViewerProps {
  input: Record<string, unknown>;
}

export const TaskViewer = memo(function TaskViewer({ input }: TaskViewerProps) {
  const description = String(input.description || '');
  const prompt = String(input.prompt || '');
  const subagentType = String(input.subagent_type || 'general-purpose');
  const model = input.model ? String(input.model) : null;
  const runInBackground = Boolean(input.run_in_background);

  return (
    <div className={cn(chatCardVariants())}>
      {/* Header */}
      <div className={cn(chatCardHeaderVariants())}>
        <Cpu size={14} style={{ color: toolColors.task }} />
        <span className="text-[11px] font-bold text-foreground">Spawn Agent</span>
        <span
          className={cn(badgeVariants({ variant: 'colored' }))}
          style={{
            backgroundColor: `${toolColors.task}20`,
            color: toolColors.task,
          }}
        >
          {subagentType}
        </span>
        {model && <span className={cn(badgeVariants())}>{model}</span>}
        {runInBackground && (
          <span
            className={cn(badgeVariants({ variant: 'colored' }))}
            style={{
              backgroundColor: 'rgba(139, 92, 246, 0.1)',
              color: '#8b5cf6',
            }}
          >
            background
          </span>
        )}
      </div>

      {/* Content */}
      <div className="p-3 space-y-2">
        {/* Description */}
        {description && (
          <div className="flex items-start gap-2">
            <Play size={10} className="shrink-0 mt-1" style={{ color: toolColors.task }} />
            <div className="text-[11px] font-medium text-foreground">{description}</div>
          </div>
        )}

        {/* Prompt preview */}
        {prompt && (
          <div className="text-[10px] font-mono leading-snug p-2 rounded border max-h-32 overflow-y-auto bg-card border-border text-muted-foreground">
            {prompt.length > 300 ? prompt.slice(0, 300) + '...' : prompt}
          </div>
        )}
      </div>
    </div>
  );
});

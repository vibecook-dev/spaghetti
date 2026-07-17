/**
 * TaskOutputViewer Component
 *
 * Displays task output retrieval operations.
 */

import React, { memo } from 'react';
import { FileOutput } from 'lucide-react';
import { cn } from '../../lib/utils.js';
import { chatCardVariants, badgeVariants } from '../variants';

interface TaskOutputViewerProps {
  input: Record<string, unknown>;
}

const outputColor = '#0ea5e9';

export const TaskOutputViewer = memo(function TaskOutputViewer({ input }: TaskOutputViewerProps) {
  const taskId = String(input.task_id || '');
  const block = input.block !== false;
  const timeout = input.timeout as number | undefined;

  return (
    <div className={cn(chatCardVariants())}>
      {/* Header */}
      <div className="flex items-center gap-2 px-3 py-2 bg-card">
        <FileOutput size={14} style={{ color: outputColor }} />
        <span className="text-[11px] font-bold text-foreground">Task Output</span>
        {taskId && (
          <span
            className={cn(badgeVariants({ variant: 'colored' }))}
            style={{
              backgroundColor: `${outputColor}15`,
              color: outputColor,
            }}
          >
            {taskId.length > 20 ? taskId.slice(0, 20) + '...' : taskId}
          </span>
        )}
        {!block && <span className={cn(badgeVariants({ variant: 'warning' }))}>non-blocking</span>}
        {timeout && <span className="text-[9px] font-mono text-muted-foreground">{timeout}ms</span>}
      </div>
    </div>
  );
});

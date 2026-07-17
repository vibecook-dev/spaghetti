/**
 * KillShellViewer Component
 */

import React, { memo } from 'react';
import { XCircle } from 'lucide-react';
import { cn } from '../../lib/utils.js';
import { chatCardVariants, chatCardHeaderVariants } from '../variants';
import { toolColors } from '../theme';

interface KillShellViewerProps {
  input: Record<string, unknown>;
}

export const KillShellViewer = memo(function KillShellViewer({ input }: KillShellViewerProps) {
  const shellId = String(input.shell_id || input.task_id || 'shell');

  return (
    <div className={cn(chatCardVariants())}>
      <div className={cn(chatCardHeaderVariants())}>
        <XCircle size={14} style={{ color: toolColors.kill }} />
        <span className="text-[11px] font-bold text-foreground">Kill Process</span>
        <span className="text-[10px] font-mono text-muted-foreground ml-auto">{shellId}</span>
      </div>
    </div>
  );
});

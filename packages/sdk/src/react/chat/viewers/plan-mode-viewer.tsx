/**
 * PlanModeViewer Component
 *
 * Displays EnterPlanMode and ExitPlanMode operations.
 * For ExitPlanMode, shows the full plan content using MarkdownContent.
 */

import React, { memo, useState } from 'react';
import { ClipboardList, ClipboardCheck, ChevronDown, ChevronRight } from 'lucide-react';
import { cn } from '../../lib/utils.js';
import { chatCardVariants, badgeVariants } from '../variants';
import { MarkdownContent } from '../content';

interface PlanModeViewerProps {
  mode: 'enter' | 'exit';
  plan?: string;
}

const planColor = '#a855f7';

export const PlanModeViewer = memo(function PlanModeViewer({ mode, plan }: PlanModeViewerProps) {
  const isEnter = mode === 'enter';
  const isExit = mode === 'exit';
  const Icon = isEnter ? ClipboardList : ClipboardCheck;
  const [isExpanded, setIsExpanded] = useState(true);

  const planTitle = plan?.match(/^#\s+(.+)$/m)?.[1] || 'Implementation Plan';

  return (
    <div className={cn(chatCardVariants())}>
      {/* Header */}
      <div
        className="flex items-center gap-2 px-3 py-2 cursor-pointer bg-card"
        onClick={() => isExit && plan && setIsExpanded(!isExpanded)}
      >
        <Icon size={14} style={{ color: planColor }} />
        <span className="text-[11px] font-bold text-foreground">{isEnter ? 'Enter Plan Mode' : 'Exit Plan Mode'}</span>
        <span
          className={cn(badgeVariants({ variant: 'colored' }))}
          style={{
            backgroundColor: `${planColor}15`,
            color: planColor,
          }}
        >
          {isEnter ? 'Planning...' : 'Ready'}
        </span>
        {isExit && plan && (
          <>
            <span className="text-[10px] ml-2 truncate flex-1 text-muted-foreground">{planTitle}</span>
            <div className="ml-auto opacity-60">
              {isExpanded ? <ChevronDown size={12} /> : <ChevronRight size={12} />}
            </div>
          </>
        )}
      </div>

      {/* Plan Content — reuses MarkdownContent for rendering */}
      {isExit && plan && isExpanded && (
        <div className="px-4 py-3 text-[11px] leading-relaxed overflow-auto max-h-[500px] bg-background text-foreground">
          <MarkdownContent content={plan} />
        </div>
      )}
    </div>
  );
});

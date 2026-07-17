/**
 * SkillViewer Component
 *
 * Displays skill invocation operations.
 */

import React, { memo } from 'react';
import { Zap } from 'lucide-react';
import { cn } from '../../lib/utils.js';
import { chatCardVariants, chatCardHeaderVariants, badgeVariants } from '../variants';

interface SkillViewerProps {
  input: Record<string, unknown>;
}

const skillColor = '#ec4899';

export const SkillViewer = memo(function SkillViewer({ input }: SkillViewerProps) {
  const skill = String(input.skill || '');
  const args = input.args as string | undefined;

  return (
    <div className={cn(chatCardVariants())}>
      {/* Header */}
      <div className={cn(chatCardHeaderVariants())}>
        <Zap size={14} style={{ color: skillColor }} />
        <span className="text-[11px] font-bold text-foreground">Skill</span>
        <span
          className={cn(badgeVariants({ variant: 'colored' }))}
          style={{
            backgroundColor: `${skillColor}15`,
            color: skillColor,
          }}
        >
          /{skill}
        </span>
      </div>

      {/* Arguments */}
      {args && (
        <div className="px-3 py-2">
          <div className="font-mono text-[10px] px-2 py-1.5 rounded bg-card border border-border text-muted-foreground">
            {args}
          </div>
        </div>
      )}
    </div>
  );
});

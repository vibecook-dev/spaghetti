/**
 * TimelineCompactSummary Component
 *
 * Timeline-style message display for compact summary messages.
 */

import React, { useState, useCallback, memo } from 'react';
import { ChevronDown, ChevronRight, Sparkles } from 'lucide-react';
import { cn } from '../../lib/utils.js';
import { TimelineDot, TimelineRow } from '../timeline';
import { MarkdownContent, RawJsonViewer } from '../content';
import { chatCardVariants, timelineHeaderVariants } from '../variants';
import { messageColors, hexToRgba } from '../theme';
import type { SessionMessage, ConnectorInfo } from '../types';

interface TimelineCompactSummaryProps {
  message: SessionMessage;
  connector: ConnectorInfo;
}

const summaryExpandedStyle = {
  backgroundColor: hexToRgba(messageColors.summary, 5),
  borderColor: hexToRgba(messageColors.summary, 30),
};

export const TimelineCompactSummary = memo(function TimelineCompactSummary({
  message,
  connector,
}: TimelineCompactSummaryProps) {
  const [isExpanded, setIsExpanded] = useState(false);
  const color = messageColors.summary;

  const handleToggle = useCallback(() => setIsExpanded((prev) => !prev), []);

  const icon = <TimelineDot color={color} icon={Sparkles} />;

  return (
    <TimelineRow
      icon={icon}
      indent={connector.indent}
      nextIndent={connector.nextIndent}
      hasNext={connector.hasNext}
      color={color}
      isAgent={connector.isAgent}
    >
      {/* Header */}
      <div className={cn(timelineHeaderVariants())} onClick={handleToggle}>
        <span className="text-[9px] font-bold uppercase tracking-wider" style={{ color }}>
          Summary
        </span>
        <div className="ml-auto opacity-30 group-hover/header:opacity-100 transition-opacity" style={{ color }}>
          {isExpanded ? <ChevronDown size={10} /> : <ChevronRight size={10} />}
        </div>
      </div>

      {/* Expanded content */}
      {isExpanded && (
        <div className={cn(chatCardVariants({ variant: 'tinted' }), 'mt-1.5 p-2')} style={summaryExpandedStyle}>
          <div className="text-[11px] leading-snug whitespace-pre-wrap font-mono text-muted-foreground">
            <MarkdownContent content={message.content || ''} />
          </div>
          <RawJsonViewer data={message.rawJson} />
        </div>
      )}
    </TimelineRow>
  );
});

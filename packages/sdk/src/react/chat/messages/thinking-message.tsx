/**
 * TimelineThinking Component
 *
 * Timeline-style message display for extended thinking messages.
 */

import React, { useState, useMemo, useCallback, memo } from 'react';
import { ChevronDown, ChevronRight, Brain, Zap, Clock } from 'lucide-react';
import { cn } from '../../lib/utils.js';
import { TimelineDot, TimelineRow } from '../timeline';
import { RawJsonViewer, MarkdownContent } from '../content';
import { chatCardVariants, chatCardHeaderVariants, badgeVariants, timelineHeaderVariants } from '../variants';
import { formatTokens, formatDuration } from '../utils/helpers';
import type { SessionMessage, ConnectorInfo } from '../types';

interface TimelineThinkingProps {
  message: SessionMessage;
  prevTimestamp?: string;
  connector: ConnectorInfo;
}

const thinkingColor = '#a855f7';

export const TimelineThinking = memo(function TimelineThinking({
  message,
  prevTimestamp,
  connector,
}: TimelineThinkingProps) {
  const [isExpanded, setIsExpanded] = useState(false);
  const handleToggle = useCallback(() => setIsExpanded((prev) => !prev), []);

  const previewText = useMemo(() => {
    const content = message.content || '';
    const firstLine = content.split('\n')[0];
    return firstLine.length > 100 ? firstLine.slice(0, 100) + '...' : firstLine;
  }, [message.content]);

  const responseTime = useMemo(() => {
    if (!prevTimestamp || !message.timestamp) return null;
    const prev = new Date(prevTimestamp).getTime();
    const curr = new Date(message.timestamp).getTime();
    const diff = curr - prev;
    if (diff <= 0 || diff > 600000) return null;
    return diff;
  }, [prevTimestamp, message.timestamp]);

  const hasUsage = message.usage && (message.usage.inputTokens > 0 || message.usage.outputTokens > 0);
  const totalTokens = message.usage ? message.usage.inputTokens + message.usage.outputTokens : 0;

  const icon = <TimelineDot color={thinkingColor} icon={Brain} />;

  return (
    <TimelineRow
      icon={icon}
      indent={connector.indent}
      nextIndent={connector.nextIndent}
      hasNext={connector.hasNext}
      color={thinkingColor}
      isAgent={connector.isAgent}
    >
      {/* Clickable header */}
      <div className={cn(timelineHeaderVariants())} onClick={handleToggle}>
        <span className="text-[10px] font-bold uppercase tracking-wider shrink-0" style={{ color: thinkingColor }}>
          Thinking
        </span>
        {!isExpanded && (
          <div
            className="text-[12px] truncate font-semibold font-mono flex-grow transition-all"
            style={{ color: thinkingColor, opacity: 0.8 }}
          >
            {previewText}
          </div>
        )}
        <div className="flex items-center gap-1.5 shrink-0">
          {hasUsage && (
            <span
              className="flex items-center gap-0.5 text-[9px] font-mono opacity-50"
              title={`Input: ${message.usage!.inputTokens} | Output: ${message.usage!.outputTokens}`}
            >
              <Zap size={9} />
              {formatTokens(totalTokens)}
            </span>
          )}
          {responseTime && (
            <span
              className="flex items-center gap-0.5 text-[9px] font-mono opacity-50"
              title={`Response time: ${responseTime}ms`}
            >
              <Clock size={9} />
              {formatDuration(responseTime)}
            </span>
          )}
        </div>
        <div className="opacity-20 group-hover/header:opacity-100 transition-opacity shrink-0">
          {isExpanded ? <ChevronDown size={10} /> : <ChevronRight size={10} />}
        </div>
      </div>

      {/* Expanded content */}
      {isExpanded && (
        <div className="mt-1">
          <div className={cn(chatCardVariants())}>
            {/* Header */}
            <div
              className={cn(chatCardHeaderVariants({ variant: 'tinted' }))}
              style={{ backgroundColor: 'rgba(168, 85, 247, 0.05)' }}
            >
              <Brain size={14} style={{ color: thinkingColor }} />
              <span className="text-[11px] font-bold" style={{ color: thinkingColor }}>
                Extended Thinking
              </span>

              <div className="ml-auto flex items-center gap-2">
                {hasUsage && (
                  <>
                    <span
                      className={cn(badgeVariants({ variant: 'colored' }))}
                      style={{
                        backgroundColor: 'rgba(6, 182, 212, 0.1)',
                        color: '#06b6d4',
                      }}
                      title="Input tokens"
                    >
                      ↓{formatTokens(message.usage!.inputTokens)}
                    </span>
                    <span
                      className={cn(badgeVariants({ variant: 'colored' }))}
                      style={{
                        backgroundColor: 'rgba(34, 197, 94, 0.1)',
                        color: '#22c55e',
                      }}
                      title="Output tokens"
                    >
                      ↑{formatTokens(message.usage!.outputTokens)}
                    </span>
                  </>
                )}
                {message.model && (
                  <span className={cn(badgeVariants())}>
                    {message.model.replace('claude-', '').replace(/-\d+$/, '')}
                  </span>
                )}
              </div>
            </div>

            {/* Content */}
            <div className="px-3 py-2 leading-relaxed max-h-96 overflow-y-auto text-foreground">
              <MarkdownContent content={message.content || ''} />
            </div>
          </div>
          <RawJsonViewer data={message.rawJson} />
        </div>
      )}
    </TimelineRow>
  );
});

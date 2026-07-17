/**
 * TimelineAssistant Component
 *
 * Timeline-style message display for assistant messages.
 */

import React, { useState, useMemo, useCallback, memo } from 'react';
import { ChevronDown, ChevronRight, Zap, Clock } from 'lucide-react';
import { cn } from '../../lib/utils.js';
import { AssistantDot } from '../timeline';
import { TimelineRow } from '../timeline';
import { RawJsonViewer, MarkdownContent } from '../content';
import { chatCardVariants, chatCardHeaderVariants, badgeVariants, timelineHeaderVariants } from '../variants';
import { timeline, typography } from '../theme';
import { formatTokens, formatDuration } from '../utils/helpers';
import type { SessionMessage, ConnectorInfo } from '../types';

interface TimelineAssistantProps {
  message: SessionMessage;
  prevTimestamp?: string;
  connector: ConnectorInfo;
}

const ASSISTANT_DOT_SIZE = 32;
const AGENT_COLOR = timeline.agentColor;

export const TimelineAssistant = memo(function TimelineAssistant({
  message,
  prevTimestamp,
  connector,
}: TimelineAssistantProps) {
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

  const accentColor = connector.isAgent ? AGENT_COLOR : 'var(--accent)';
  const icon = <AssistantDot size={ASSISTANT_DOT_SIZE} />;

  return (
    <TimelineRow
      icon={icon}
      indent={connector.indent}
      nextIndent={connector.nextIndent}
      hasNext={connector.hasNext}
      color={accentColor}
      isAgent={connector.isAgent}
    >
      {/* Clickable header */}
      <div className={cn(timelineHeaderVariants())} onClick={handleToggle}>
        <span className="text-[10px] font-bold uppercase tracking-wider shrink-0" style={{ color: accentColor }}>
          Claude
        </span>
        {!isExpanded && (
          <div
            className="text-[12px] truncate font-semibold font-mono flex-grow transition-all"
            style={{ color: accentColor }}
          >
            {previewText}
          </div>
        )}
        <div className="flex items-center gap-1.5 shrink-0">
          {hasUsage && (
            <span
              className="flex items-center gap-0.5 text-[9px] font-mono opacity-50"
              title={`Input: ${message.usage!.inputTokens} | Output: ${message.usage!.outputTokens} | Cache Read: ${message.usage!.cacheReadInputTokens} | Cache Write: ${message.usage!.cacheCreationInputTokens}`}
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
          <div className={cn(chatCardVariants({ variant: 'expanded' }))}>
            {/* Header */}
            <div className={cn(chatCardHeaderVariants())}>
              <Zap size={14} style={{ color: accentColor }} />
              <span className="text-[11px] font-bold text-foreground">Response</span>

              {/* Token badges */}
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
                    {message.usage!.cacheReadInputTokens > 0 && (
                      <span
                        className={cn(badgeVariants({ variant: 'colored' }))}
                        style={{
                          backgroundColor: 'rgba(168, 85, 247, 0.1)',
                          color: '#a855f7',
                        }}
                        title="Cache read tokens"
                      >
                        ⚡{formatTokens(message.usage!.cacheReadInputTokens)}
                      </span>
                    )}
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
            <div
              className="px-3 py-2 leading-relaxed max-h-96 overflow-y-auto text-foreground"
              style={{ fontFamily: typography.mono }}
            >
              <MarkdownContent content={message.content || ''} />
            </div>
          </div>
          <RawJsonViewer data={message.rawJson} />
        </div>
      )}
    </TimelineRow>
  );
});

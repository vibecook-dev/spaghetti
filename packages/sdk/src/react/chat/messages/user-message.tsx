/**
 * UserMessage Component
 *
 * Bubble-style message display for user messages.
 */

import React, { memo } from 'react';
import { Brain } from 'lucide-react';
import { cn } from '../../lib/utils.js';
import { RawJsonViewer } from '../content';
import { badgeVariants } from '../variants';
import { typography } from '../theme';
import type { SessionMessage, ConnectorInfo } from '../types';

function getThinkingLevelStyle(level: string): { bg: string; color: string; label: string } {
  switch (level) {
    case 'high':
      return { bg: 'rgba(168, 85, 247, 0.15)', color: '#a855f7', label: 'High' };
    case 'medium':
      return { bg: 'rgba(59, 130, 246, 0.15)', color: '#3b82f6', label: 'Med' };
    case 'low':
      return { bg: 'rgba(34, 197, 94, 0.15)', color: '#22c55e', label: 'Low' };
    default:
      return { bg: 'rgba(107, 114, 128, 0.15)', color: '#6b7280', label: level };
  }
}

interface UserMessageProps {
  message: SessionMessage;
  connector: ConnectorInfo;
}

export const UserMessage = memo(function UserMessage({ message }: UserMessageProps) {
  return (
    <div className="flex flex-col items-end mb-4 mt-2 animate-fadeInUp">
      <div className="max-w-[85%] flex flex-col items-end">
        <div
          className="px-3 py-2 text-white chat-card-shadow relative group text-[12px] rounded-lg border border-border"
          style={{
            lineBreak: 'anywhere',
            backgroundColor: 'var(--accent)',
            fontFamily: typography.mono,
          }}
        >
          <p className="leading-snug whitespace-pre-wrap font-medium">{message.content}</p>
          {/* Tail */}
          <div
            className="absolute -right-1.5 top-3 w-3 h-3 transform rotate-45 border-t border-r border-border"
            style={{ backgroundColor: 'var(--accent)' }}
          />
        </div>
        {/* Controls below the bubble */}
        <div className="self-start flex items-center gap-1.5 mt-0.5 w-full relative">
          <RawJsonViewer data={message.rawJson} />
          {message.thinkingMetadata && !message.thinkingMetadata.disabled && (
            <div
              className={cn(
                badgeVariants({ variant: 'colored' }),
                'flex items-center gap-1 font-medium absolute right-0 top-2',
              )}
              style={{
                backgroundColor: getThinkingLevelStyle(message.thinkingMetadata.level).bg,
                color: getThinkingLevelStyle(message.thinkingMetadata.level).color,
              }}
              title={`Extended thinking: ${message.thinkingMetadata.level}${message.thinkingMetadata.triggers?.length ? ` (${message.thinkingMetadata.triggers.join(', ')})` : ''}`}
            >
              <Brain size={10} />
              <span>{getThinkingLevelStyle(message.thinkingMetadata.level).label}</span>
            </div>
          )}
        </div>
      </div>
    </div>
  );
});

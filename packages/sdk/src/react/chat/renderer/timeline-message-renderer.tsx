/**
 * TimelineMessageRenderer
 *
 * Main component for rendering chat messages in timeline format.
 * Uses SVG-based connectors for ALL lines (straight and curved).
 * Follows a grid-based layout matching the reference design.
 */

import React, { memo } from 'react';
import { Bot } from 'lucide-react';
import {
  UserMessage,
  TimelineAssistant,
  TimelineToolUse,
  TimelineCompactSummary,
  TimelineThinking,
  TimelineCheckpoint,
  TimelineSystem,
  TimelineSummary,
  TimelineQueueOperation,
} from '../messages';
import { timeline } from '../theme';
import type { SessionMessage, ConnectorInfo } from '../types';

// =============================================================================
// TYPES
// =============================================================================

interface TimelineMessageRendererProps {
  message: SessionMessage;
  isLast?: boolean;
  connectToNext?: boolean;
  prevTimestamp?: string;
  prevAgentId?: string;
  nextAgentId?: string;
  nextIsSidechain?: boolean;
}

// =============================================================================
// CONSTANTS
// =============================================================================

const AGENT_COLOR = timeline.agentColor;
const AGENT_DOT_SIZE = 32;

// =============================================================================
// AGENT HEADER COMPONENT
// =============================================================================

interface AgentHeaderProps {
  agentId?: string;
}

/**
 * Header shown at the start of each agent/sub-thread group.
 * Displays a robot icon and the agent ID.
 */
const AgentHeader = memo(function AgentHeader({ agentId }: AgentHeaderProps) {
  return (
    <div
      className="relative grid gap-4 animate-fadeInUp"
      style={{
        gridTemplateColumns: `${timeline.railWidth}px 1fr`,
        minHeight: '48px',
      }}
    >
      {/* Rail with icon */}
      <div className="relative h-full w-full">
        <div
          className="absolute z-10 flex items-center justify-center"
          style={{
            left: timeline.indentX - AGENT_DOT_SIZE / 2,
            top: timeline.iconOffsetY - AGENT_DOT_SIZE / 2,
            width: AGENT_DOT_SIZE,
            height: AGENT_DOT_SIZE,
          }}
        >
          {/* Agent icon with circle background */}
          <div
            className="relative flex items-center justify-center rounded-full shadow-lg shrink-0 transition-transform duration-300 hover:scale-110"
            style={{
              width: AGENT_DOT_SIZE,
              height: AGENT_DOT_SIZE,
              backgroundColor: `${AGENT_COLOR}15`,
              border: `2px solid ${AGENT_COLOR}`,
            }}
          >
            {/* Inner glow */}
            <div className="absolute inset-1 rounded-full opacity-20" style={{ backgroundColor: AGENT_COLOR }} />
            <Bot size={AGENT_DOT_SIZE * 0.45} style={{ color: AGENT_COLOR }} />
          </div>
        </div>
      </div>

      {/* Content - Agent label */}
      <div className="pt-5 pb-2 pr-4 flex items-center" style={{ paddingLeft: '20px' }}>
        <span className="text-[11px] font-bold uppercase tracking-wider" style={{ color: AGENT_COLOR }}>
          {agentId ? `Agent: ${agentId}` : 'Sub-Agent'}
        </span>
      </div>
    </div>
  );
});

// =============================================================================
// HELPER FUNCTIONS
// =============================================================================

/**
 * Check if a message type is a timeline type (not user message)
 */
export function isTimelineType(type: string): boolean {
  return [
    'assistant',
    'tool_use',
    'compact_summary',
    'thinking',
    'checkpoint',
    'system',
    'summary',
    'queue-operation',
  ].includes(type);
}

/**
 * Calculate connector info based on message and neighbor properties
 */
function getConnectorInfo(
  message: SessionMessage,
  isLast: boolean,
  connectToNext: boolean,
  prevAgentId?: string,
  nextAgentId?: string,
  nextIsSidechain?: boolean,
): ConnectorInfo {
  const isSidechain = !!message.isSidechain;
  const agentId = message.agentId;

  // Current indent: 0 for main thread, 1 for sub-agent
  const indent: 0 | 1 = isSidechain ? 1 : 0;

  // Calculate next indent based on next message's sidechain status
  let nextIndent: 0 | 1 = 0;
  if (!isLast && connectToNext) {
    nextIndent = nextIsSidechain ? 1 : 0;
  }

  // Determine if there's a next message to connect to
  const hasNext = !isLast || connectToNext;

  // Detect agent group boundaries
  const isFirstInAgentGroup = isSidechain && agentId !== prevAgentId;

  return {
    indent,
    nextIndent,
    hasNext,
    isAgent: isSidechain,
    isFirstInAgentGroup,
    agentId,
  };
}

// =============================================================================
// MAIN COMPONENT
// =============================================================================

function TimelineMessageRendererInner({
  message,
  isLast = true,
  connectToNext = false,
  prevTimestamp,
  prevAgentId,
  nextAgentId,
  nextIsSidechain,
}: TimelineMessageRendererProps): React.ReactElement | null {
  // Calculate connector info for this message
  const connector = getConnectorInfo(message, isLast, connectToNext, prevAgentId, nextAgentId, nextIsSidechain);

  // Helper to wrap content with agent header if needed
  const wrapWithAgentHeader = (content: React.ReactElement) => {
    if (connector.isFirstInAgentGroup) {
      return (
        <>
          <AgentHeader agentId={connector.agentId} />
          {content}
        </>
      );
    }
    return content;
  };

  // User message - bubble style (doesn't use timeline row)
  if (message.type === 'user' && message.content) {
    return wrapWithAgentHeader(<UserMessage message={message} connector={connector} />);
  }

  // Assistant message - timeline style
  if (message.type === 'assistant' && message.content) {
    return wrapWithAgentHeader(
      <TimelineAssistant message={message} prevTimestamp={prevTimestamp} connector={connector} />,
    );
  }

  // Tool use message - timeline style (includes merged tool result)
  if (message.type === 'tool_use' && message.toolUse) {
    return wrapWithAgentHeader(<TimelineToolUse message={message} connector={connector} />);
  }

  // Compact summary message - timeline style
  if (message.type === 'compact_summary' && message.content) {
    return wrapWithAgentHeader(<TimelineCompactSummary message={message} connector={connector} />);
  }

  // Thinking message - timeline style
  if (message.type === 'thinking' && message.content) {
    return wrapWithAgentHeader(
      <TimelineThinking message={message} prevTimestamp={prevTimestamp} connector={connector} />,
    );
  }

  // Checkpoint message (file-history-snapshot) - timeline style
  if (message.type === 'checkpoint') {
    return wrapWithAgentHeader(<TimelineCheckpoint message={message} connector={connector} />);
  }

  // System message - timeline style
  if (message.type === 'system') {
    return wrapWithAgentHeader(<TimelineSystem message={message} connector={connector} />);
  }

  // Summary message - timeline style
  if (message.type === 'summary') {
    return wrapWithAgentHeader(<TimelineSummary message={message} connector={connector} />);
  }

  // Queue operation message - timeline style
  if (message.type === 'queue-operation') {
    return wrapWithAgentHeader(<TimelineQueueOperation message={message} connector={connector} />);
  }

  return null;
}

// =============================================================================
// MEMOIZED EXPORT
// =============================================================================

export const TimelineMessageRenderer = memo(TimelineMessageRendererInner, (prevProps, nextProps) => {
  return (
    prevProps.message.uuid === nextProps.message.uuid &&
    prevProps.isLast === nextProps.isLast &&
    prevProps.connectToNext === nextProps.connectToNext &&
    prevProps.prevTimestamp === nextProps.prevTimestamp &&
    prevProps.prevAgentId === nextProps.prevAgentId &&
    prevProps.nextAgentId === nextProps.nextAgentId &&
    prevProps.nextIsSidechain === nextProps.nextIsSidechain
  );
});

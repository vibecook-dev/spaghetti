/**
 * TimelineSystemEvents
 *
 * Timeline components for system events:
 * - TimelineCheckpoint: File history snapshots
 * - TimelineSystem: System messages (local commands, compact boundaries)
 * - TimelineSummary: Chat segment summaries
 * - TimelineQueueOperation: Background task notifications
 */

import React, { memo, useState, useMemo, useCallback } from 'react';
import {
  Archive,
  Terminal,
  Scissors,
  FileText,
  Bell,
  ChevronDown,
  ChevronRight,
  StopCircle,
  Webhook,
  AlertCircle,
  CheckCircle2,
  CheckCircle,
} from 'lucide-react';
import { cn } from '../../lib/utils.js';
import { TimelineDot, TimelineRow } from '../timeline';
import { RawJsonViewer } from '../content';
import { chatCardVariants, badgeVariants, timelineHeaderVariants } from '../variants';
import { hexToRgba } from '../theme';
import { formatTime, formatTokens, formatDuration } from '../utils/helpers';
import type { SessionMessage, ConnectorInfo, HookInfo } from '../types';

// =============================================================================
// TYPES
// =============================================================================

interface TimelineEventProps {
  message: SessionMessage;
  connector: ConnectorInfo;
}

// =============================================================================
// COLORS
// =============================================================================

const colors = {
  checkpoint: '#6366f1',
  system: '#64748b',
  compact: '#f59e0b',
  summary: '#8b5cf6',
  queue: '#22c55e',
  hook: '#06b6d4',
  stop: '#ef4444',
  done: '#22c55e',
};

// =============================================================================
// TIMELINE CHECKPOINT
// =============================================================================

export const TimelineCheckpoint = memo(function TimelineCheckpoint({
  message,
  connector,
}: TimelineEventProps): React.ReactElement {
  const [isExpanded, setIsExpanded] = useState(false);
  const color = colors.checkpoint;

  const handleToggle = useCallback(() => setIsExpanded((prev) => !prev), []);

  const time = formatTime(message.timestamp);
  const isUpdate = message.checkpointData?.isUpdate;
  const fileCount = message.checkpointData?.fileCount || 0;

  const icon = <TimelineDot color={color} icon={Archive} />;

  return (
    <TimelineRow
      icon={icon}
      indent={connector.indent}
      nextIndent={connector.nextIndent}
      hasNext={connector.hasNext}
      color={color}
      isAgent={connector.isAgent}
    >
      <div className={cn(timelineHeaderVariants())} onClick={handleToggle}>
        <span className="text-[9px] font-bold uppercase tracking-wider shrink-0" style={{ color }}>
          {isUpdate ? 'Checkpoint Updated' : 'Checkpoint Created'}
        </span>
        {fileCount > 0 && (
          <span className="text-[11px] font-mono opacity-60" style={{ color }}>
            {fileCount} {fileCount === 1 ? 'file' : 'files'}
          </span>
        )}
        {time && <span className="text-[9px] font-mono opacity-40 ml-auto shrink-0 text-muted-foreground">{time}</span>}
        <div className="opacity-20 group-hover/header:opacity-100 transition-opacity shrink-0" style={{ color }}>
          {isExpanded ? <ChevronDown size={10} /> : <ChevronRight size={10} />}
        </div>
      </div>

      {isExpanded && (
        <div
          className={cn(chatCardVariants({ variant: 'tinted' }), 'mt-1.5 p-2')}
          style={{
            backgroundColor: hexToRgba(color, 5),
            borderColor: hexToRgba(color, 30),
          }}
        >
          <div className="text-[11px] font-mono space-y-1 text-muted-foreground">
            <div>
              <span className="opacity-50">Message ID: </span>
              <span>{message.checkpointData?.messageId || 'N/A'}</span>
            </div>
            <div>
              <span className="opacity-50">Type: </span>
              <span>{isUpdate ? 'Update' : 'New checkpoint'}</span>
            </div>
            <div>
              <span className="opacity-50">Files tracked: </span>
              <span>{fileCount}</span>
            </div>
          </div>
          <RawJsonViewer data={message.rawJson} />
        </div>
      )}
    </TimelineRow>
  );
});

// =============================================================================
// TIMELINE SYSTEM
// =============================================================================

export const TimelineSystem = memo(function TimelineSystem({
  message,
  connector,
}: TimelineEventProps): React.ReactElement {
  const [isExpanded, setIsExpanded] = useState(false);

  const isCompactBoundary = message.systemSubtype === 'compact_boundary';
  const isLocalCommand = message.systemSubtype === 'local_command';
  const isStopHook = message.systemSubtype === 'stop_hook_summary';
  const isTurnDuration = message.systemSubtype === 'turn_duration';
  const isHook = message.systemSubtype?.includes('hook');

  const rawJson = message.rawJson as Record<string, unknown> | undefined;
  const hookCount = rawJson?.hookCount as number | undefined;
  const hookInfos = rawJson?.hookInfos as HookInfo[] | undefined;
  const hookErrors = rawJson?.hookErrors as string[] | undefined;
  const preventedContinuation = rawJson?.preventedContinuation as boolean | undefined;
  const hasOutput = rawJson?.hasOutput as boolean | undefined;
  const durationMs = rawJson?.durationMs as number | undefined;

  let color = colors.system;
  let Icon = Terminal;
  let label = 'System';

  if (isTurnDuration) {
    color = colors.done;
    Icon = CheckCircle;
    const duration = durationMs ? formatDuration(durationMs) : '';
    label = duration ? `Done in ${duration}` : 'Done';
  } else if (isCompactBoundary) {
    color = colors.compact;
    Icon = Scissors;
    label = 'Context Compacted';
  } else if (isStopHook) {
    color = colors.hook;
    Icon = StopCircle;
    label = 'Stop Hook';
  } else if (isHook) {
    color = colors.hook;
    Icon = Webhook;
    label = message.systemSubtype?.replace(/_/g, ' ').replace(/\b\w/g, (l) => l.toUpperCase()) || 'Hook';
  } else if (isLocalCommand) {
    let commandName = '';
    if (message.content) {
      const match = message.content.match(/<command-name>(.*?)<\/command-name>/);
      if (match) commandName = match[1];
    }
    label = commandName || 'Command';
  }

  const handleToggle = useCallback(() => setIsExpanded((prev) => !prev), []);
  const time = formatTime(message.timestamp);
  const icon = <TimelineDot color={color} icon={Icon} />;

  return (
    <TimelineRow
      icon={icon}
      indent={connector.indent}
      nextIndent={connector.nextIndent}
      hasNext={connector.hasNext}
      color={color}
      isAgent={connector.isAgent}
    >
      <div className={cn(timelineHeaderVariants())} onClick={handleToggle}>
        <span className="text-[10px] font-bold uppercase tracking-wider shrink-0" style={{ color }}>
          {label}
        </span>

        {isHook && hookCount !== undefined && hookCount > 0 && (
          <span
            className={cn(badgeVariants({ variant: 'colored' }))}
            style={{ backgroundColor: hexToRgba(color, 15), color }}
          >
            {hookCount} {hookCount === 1 ? 'hook' : 'hooks'}
          </span>
        )}

        {hookErrors && hookErrors.length > 0 && (
          <span className={cn(badgeVariants({ variant: 'error' }), 'flex items-center gap-0.5')}>
            <AlertCircle size={9} />
            {hookErrors.length} {hookErrors.length === 1 ? 'error' : 'errors'}
          </span>
        )}

        {isHook && hasOutput && (!hookErrors || hookErrors.length === 0) && (
          <span className="flex items-center gap-0.5 text-[9px] font-mono opacity-60" style={{ color: '#22c55e' }}>
            <CheckCircle2 size={9} />
          </span>
        )}

        {isCompactBoundary && message.compactMetadata && (
          <span className="text-[9px] font-mono opacity-50" style={{ color }}>
            {formatTokens(message.compactMetadata.preTokens)} tokens
          </span>
        )}

        {time && <span className="text-[9px] font-mono opacity-40 ml-auto shrink-0 text-muted-foreground">{time}</span>}

        <div className="opacity-20 group-hover/header:opacity-100 transition-opacity shrink-0" style={{ color }}>
          {isExpanded ? <ChevronDown size={10} /> : <ChevronRight size={10} />}
        </div>
      </div>

      {isExpanded && (
        <div
          className={cn(chatCardVariants({ variant: 'tinted' }), 'mt-1.5 p-2')}
          style={{
            backgroundColor: hexToRgba(color, 5),
            borderColor: hexToRgba(color, 30),
          }}
        >
          {isHook && (
            <div className="space-y-2">
              {hookInfos && hookInfos.length > 0 && (
                <div>
                  <div className="text-[9px] font-bold uppercase tracking-wider mb-1 text-muted-foreground">
                    Commands
                  </div>
                  {hookInfos.map((info, i) => (
                    <div key={i} className="text-[11px] font-mono px-2 py-1 rounded mb-1 bg-background text-foreground">
                      $ {info.command}
                    </div>
                  ))}
                </div>
              )}

              {hookErrors && hookErrors.length > 0 && (
                <div>
                  <div className="text-[9px] font-bold uppercase tracking-wider mb-1" style={{ color: colors.stop }}>
                    Errors
                  </div>
                  {hookErrors.map((err, i) => (
                    <div
                      key={i}
                      className="text-[11px] font-mono px-2 py-1 rounded mb-1"
                      style={{
                        backgroundColor: hexToRgba(colors.stop, 10),
                        color: colors.stop,
                      }}
                    >
                      {err}
                    </div>
                  ))}
                </div>
              )}

              <div
                className="text-[10px] font-mono space-y-0.5 pt-1 border-t text-muted-foreground"
                style={{ borderColor: hexToRgba(color, 20) }}
              >
                {preventedContinuation !== undefined && (
                  <div>
                    <span className="opacity-50">Prevented continuation: </span>
                    <span style={{ color: preventedContinuation ? colors.stop : '#22c55e' }}>
                      {preventedContinuation ? 'Yes' : 'No'}
                    </span>
                  </div>
                )}
                {hasOutput !== undefined && (
                  <div>
                    <span className="opacity-50">Has output: </span>
                    <span>{hasOutput ? 'Yes' : 'No'}</span>
                  </div>
                )}
              </div>
            </div>
          )}

          {!isHook && message.content && (
            <pre className="text-[11px] leading-snug whitespace-pre-wrap font-mono text-muted-foreground">
              {message.content}
            </pre>
          )}

          <RawJsonViewer data={message.rawJson} />
        </div>
      )}
    </TimelineRow>
  );
});

// =============================================================================
// TIMELINE SUMMARY
// =============================================================================

export const TimelineSummary = memo(function TimelineSummary({
  message,
  connector,
}: TimelineEventProps): React.ReactElement {
  const [isExpanded, setIsExpanded] = useState(false);
  const color = colors.summary;

  const handleToggle = useCallback(() => setIsExpanded((prev) => !prev), []);

  const previewText = useMemo(() => {
    const content = message.content || '';
    const firstLine = content.split('\n')[0];
    return firstLine.length > 80 ? firstLine.slice(0, 80) + '...' : firstLine;
  }, [message.content]);

  const time = formatTime(message.timestamp);
  const icon = <TimelineDot color={color} icon={FileText} />;

  return (
    <TimelineRow
      icon={icon}
      indent={connector.indent}
      nextIndent={connector.nextIndent}
      hasNext={connector.hasNext}
      color={color}
      isAgent={connector.isAgent}
    >
      <div className={cn(timelineHeaderVariants())} onClick={handleToggle}>
        <span className="text-[10px] font-bold uppercase tracking-wider shrink-0" style={{ color }}>
          Chat Summary
        </span>

        {!isExpanded && (
          <span className="text-[11px] font-mono truncate opacity-60 flex-grow" style={{ color }}>
            {previewText}
          </span>
        )}

        {time && <span className="text-[9px] font-mono opacity-40 ml-auto shrink-0 text-muted-foreground">{time}</span>}

        <div className="opacity-20 group-hover/header:opacity-100 transition-opacity shrink-0" style={{ color }}>
          {isExpanded ? <ChevronDown size={10} /> : <ChevronRight size={10} />}
        </div>
      </div>

      {isExpanded && message.content && (
        <div
          className={cn(chatCardVariants({ variant: 'tinted' }), 'mt-1.5 p-2')}
          style={{
            backgroundColor: hexToRgba(color, 5),
            borderColor: hexToRgba(color, 30),
          }}
        >
          <div className="text-[11px] leading-snug whitespace-pre-wrap text-foreground">{message.content}</div>
          <RawJsonViewer data={message.rawJson} />
        </div>
      )}
    </TimelineRow>
  );
});

// =============================================================================
// TIMELINE QUEUE OPERATION
// =============================================================================

export const TimelineQueueOperation = memo(function TimelineQueueOperation({
  message,
  connector,
}: TimelineEventProps): React.ReactElement {
  const [isExpanded, setIsExpanded] = useState(false);
  const color = colors.queue;

  const handleToggle = useCallback(() => setIsExpanded((prev) => !prev), []);

  let summary = '';
  let status = '';
  if (message.content) {
    const summaryMatch = message.content.match(/<summary>(.*?)<\/summary>/);
    const statusMatch = message.content.match(/<status>(.*?)<\/status>/);
    if (summaryMatch) summary = summaryMatch[1];
    if (statusMatch) status = statusMatch[1];
  }

  if (message.queueOperation !== 'enqueue' || !message.content) {
    return <></>;
  }

  const time = formatTime(message.timestamp);
  const icon = <TimelineDot color={color} icon={Bell} />;

  return (
    <TimelineRow
      icon={icon}
      indent={connector.indent}
      nextIndent={connector.nextIndent}
      hasNext={connector.hasNext}
      color={color}
      isAgent={connector.isAgent}
    >
      <div className={cn(timelineHeaderVariants())} onClick={handleToggle}>
        <span className="text-[10px] font-bold uppercase tracking-wider shrink-0" style={{ color }}>
          Background Task
        </span>

        {status && (
          <span className={cn(badgeVariants({ variant: status === 'completed' ? 'success' : 'warning' }), 'uppercase')}>
            {status}
          </span>
        )}

        {!isExpanded && summary && (
          <span className="text-[11px] font-mono truncate opacity-60 flex-grow text-muted-foreground">{summary}</span>
        )}

        {time && <span className="text-[9px] font-mono opacity-40 ml-auto shrink-0 text-muted-foreground">{time}</span>}

        <div className="opacity-20 group-hover/header:opacity-100 transition-opacity shrink-0" style={{ color }}>
          {isExpanded ? <ChevronDown size={10} /> : <ChevronRight size={10} />}
        </div>
      </div>

      {isExpanded && message.content && (
        <div
          className={cn(chatCardVariants({ variant: 'tinted' }), 'mt-1.5 p-2')}
          style={{
            backgroundColor: hexToRgba(color, 5),
            borderColor: hexToRgba(color, 30),
          }}
        >
          <pre className="text-[11px] leading-snug whitespace-pre-wrap font-mono text-muted-foreground">
            {message.content}
          </pre>
          <RawJsonViewer data={message.rawJson} />
        </div>
      )}
    </TimelineRow>
  );
});

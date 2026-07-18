/**
 * Transform raw spaghetti SessionMessage JSONL records into the flattened
 * chat UI SessionMessage format (split thinking / tool_use, merge tool results).
 *
 * Ported from ui-v2 RawJsonLineViewer parseJsonlEntry + sequential tool-result merge.
 *
 * Pass `sourceId` when the raw rows are non-Claude (Codex RolloutLine, Grok
 * chat_history) so they are adapted to the Claude envelope first. Without it,
 * Codex/Grok lines are silently dropped and the session view looks empty.
 */

import type { SessionMessage, ToolResultInfo } from './types.js';
import { adaptMessagesForDisplay } from '../../sources/adapt-display-messages.js';

type AnyMsg = Record<string, any>;

export interface TransformRawMessagesOptions {
  /** Agent source that produced the rows (`codex`, `grok`, `claude-code`, …). */
  sourceId?: string;
}

function toolResultContent(content: unknown): string {
  if (typeof content === 'string') return content;
  if (content == null) return '';
  try {
    return JSON.stringify(content, null, 2);
  } catch {
    return String(content);
  }
}

/**
 * Convert a batch of raw transcript messages (SDK `SessionMessage` / JSONL shape)
 * into timeline display messages for {@link TimelineMessageRenderer}.
 */
export function transformRawMessagesToTimeline(
  rawMessages: AnyMsg[],
  options?: TransformRawMessagesOptions,
): SessionMessage[] {
  const sourceId = options?.sourceId;
  const input: AnyMsg[] =
    sourceId && sourceId !== 'claude-code'
      ? (adaptMessagesForDisplay(rawMessages, sourceId) as unknown as AnyMsg[])
      : rawMessages;

  const out: SessionMessage[] = [];
  /** tool_use_id → result, filled as we scan user tool_result blocks */
  const pendingResults = new Map<string, ToolResultInfo>();
  /** tool_use messages waiting for a result */
  const openTools = new Map<string, SessionMessage>();

  for (const entry of input) {
    if (!entry || typeof entry !== 'object') continue;
    const type = String(entry.type ?? '');

    if (type === 'user') {
      const message = entry.message as { content?: string | AnyMsg[]; role?: string } | undefined;
      if (!message?.content) continue;

      if (Array.isArray(message.content)) {
        const contentArray = message.content;
        const hasToolResults = contentArray.some((c) => c?.type === 'tool_result');

        if (hasToolResults) {
          for (const item of contentArray) {
            if (item?.type !== 'tool_result') continue;
            const toolId = String(item.tool_use_id ?? '');
            const result: ToolResultInfo = {
              toolId,
              isError: item.is_error === true,
              content: toolResultContent(item.content),
              rawJson: item,
            };
            pendingResults.set(toolId, result);
            const open = openTools.get(toolId);
            if (open?.toolUse) {
              open.toolUse = { ...open.toolUse, result };
              openTools.delete(toolId);
            }
          }
          // tool_result-only user rows are not rendered as user messages
          const hasText = contentArray.some((c) => c?.type === 'text' && c.text);
          if (!hasText) continue;
          const textParts = contentArray.filter((c) => c?.type === 'text').map((c) => String(c.text ?? ''));
          out.push({
            uuid: String(entry.uuid ?? cryptoRandom()),
            parentUuid: (entry.parentUuid as string) ?? null,
            type: entry.isCompactSummary ? 'compact_summary' : 'user',
            timestamp: String(entry.timestamp ?? ''),
            sessionId: String(entry.sessionId ?? ''),
            content: textParts.join('\n'),
            role: 'user',
            isCompactSummary: entry.isCompactSummary === true || undefined,
            agentId: entry.agentId as string | undefined,
            isSidechain: entry.isSidechain === true || undefined,
            rawJson: entry,
          });
          continue;
        }

        const textParts = contentArray.filter((c) => c?.type === 'text').map((c) => String(c.text ?? ''));
        const content = textParts.join('\n');
        if (!content && !entry.isCompactSummary) continue;

        out.push({
          uuid: String(entry.uuid ?? cryptoRandom()),
          parentUuid: (entry.parentUuid as string) ?? null,
          type: entry.isCompactSummary || entry.isVisibleInTranscriptOnly ? 'compact_summary' : 'user',
          timestamp: String(entry.timestamp ?? ''),
          sessionId: String(entry.sessionId ?? ''),
          content,
          role: 'user',
          isCompactSummary: entry.isCompactSummary === true || undefined,
          agentId: entry.agentId as string | undefined,
          isSidechain: entry.isSidechain === true || undefined,
          rawJson: entry,
        });
      } else if (typeof message.content === 'string') {
        out.push({
          uuid: String(entry.uuid ?? cryptoRandom()),
          parentUuid: (entry.parentUuid as string) ?? null,
          type: entry.isCompactSummary ? 'compact_summary' : 'user',
          timestamp: String(entry.timestamp ?? ''),
          sessionId: String(entry.sessionId ?? ''),
          content: message.content,
          role: 'user',
          isCompactSummary: entry.isCompactSummary === true || undefined,
          agentId: entry.agentId as string | undefined,
          isSidechain: entry.isSidechain === true || undefined,
          rawJson: entry,
        });
      }
      continue;
    }

    if (type === 'assistant') {
      const message = entry.message as
        | {
            content?: AnyMsg[];
            model?: string;
            usage?: {
              input_tokens?: number;
              output_tokens?: number;
              cache_creation_input_tokens?: number;
              cache_read_input_tokens?: number;
            };
            stop_reason?: string;
          }
        | undefined;
      if (!message) continue;

      const usage = message.usage
        ? {
            inputTokens: message.usage.input_tokens || 0,
            outputTokens: message.usage.output_tokens || 0,
            cacheCreationInputTokens: message.usage.cache_creation_input_tokens || 0,
            cacheReadInputTokens: message.usage.cache_read_input_tokens || 0,
          }
        : undefined;

      const thinkingMsgs: SessionMessage[] = [];
      const toolMsgs: SessionMessage[] = [];
      const textParts: string[] = [];
      let thinkingIndex = 0;

      if (Array.isArray(message.content)) {
        for (const part of message.content) {
          if (!part) continue;
          if (part.type === 'text') {
            textParts.push(String(part.text ?? ''));
          } else if (part.type === 'thinking') {
            thinkingMsgs.push({
              uuid: `${entry.uuid}-thinking-${thinkingIndex++}`,
              parentUuid: (entry.uuid as string) ?? null,
              type: 'thinking',
              timestamp: String(entry.timestamp ?? ''),
              sessionId: String(entry.sessionId ?? ''),
              content: String(part.thinking ?? ''),
              model: message.model,
              usage,
              agentId: entry.agentId as string | undefined,
              isSidechain: entry.isSidechain === true || undefined,
              rawJson: part,
            });
          } else if (part.type === 'tool_use') {
            const toolId = String(part.id ?? '');
            const pending = pendingResults.get(toolId);
            const toolMsg: SessionMessage = {
              uuid: `${entry.uuid}-${toolId || 'tool'}`,
              parentUuid: (entry.uuid as string) ?? null,
              type: 'tool_use',
              timestamp: String(entry.timestamp ?? ''),
              sessionId: String(entry.sessionId ?? ''),
              toolUse: {
                toolName: String(part.name ?? 'Unknown Tool'),
                toolId,
                input: (part.input as Record<string, unknown>) || {},
                result: pending,
              },
              agentId: entry.agentId as string | undefined,
              isSidechain: entry.isSidechain === true || undefined,
              rawJson: part,
            };
            if (pending) pendingResults.delete(toolId);
            else openTools.set(toolId, toolMsg);
            toolMsgs.push(toolMsg);
          }
        }
      }

      out.push(...thinkingMsgs);
      const textContent = textParts.join('\n');
      if (textContent) {
        out.push({
          uuid: String(entry.uuid ?? cryptoRandom()),
          parentUuid: (entry.parentUuid as string) ?? null,
          type: 'assistant',
          timestamp: String(entry.timestamp ?? ''),
          sessionId: String(entry.sessionId ?? ''),
          content: textContent,
          role: 'assistant',
          model: message.model,
          usage,
          stopReason: message.stop_reason,
          agentId: entry.agentId as string | undefined,
          isSidechain: entry.isSidechain === true || undefined,
          rawJson: entry,
        });
      }
      out.push(...toolMsgs);
      continue;
    }

    // Some sources and development fixtures expose an already-normalized
    // standalone result. Claude tool_result blocks still take the merge path
    // above, while this preserves orphaned/streamed results as visible rows.
    if (type === 'tool_result') {
      const rawResult = (entry.toolResult ?? entry.tool_result ?? entry) as AnyMsg;
      out.push({
        uuid: String(entry.uuid ?? cryptoRandom()),
        parentUuid: (entry.parentUuid as string) ?? null,
        type: 'tool_result',
        timestamp: String(entry.timestamp ?? ''),
        sessionId: String(entry.sessionId ?? ''),
        toolResult: {
          toolId: String(rawResult.toolId ?? rawResult.tool_use_id ?? ''),
          isError: rawResult.isError === true || rawResult.is_error === true,
          content: toolResultContent(rawResult.content),
          rawJson: rawResult.rawJson ?? entry,
        },
        agentId: entry.agentId as string | undefined,
        isSidechain: entry.isSidechain === true || undefined,
        rawJson: entry,
      });
      continue;
    }

    if (type === 'summary') {
      out.push({
        uuid: String(entry.uuid ?? cryptoRandom()),
        parentUuid: null,
        type: 'summary',
        timestamp: String(entry.timestamp ?? ''),
        sessionId: String(entry.sessionId ?? ''),
        content: String(entry.summary ?? ''),
        leafUuid: entry.leafUuid as string | undefined,
        rawJson: entry,
      });
      continue;
    }

    if (type === 'file-history-snapshot') {
      const snapshot = entry.snapshot as
        | {
            trackedFileBackups?: Record<string, { backupTime?: string }>;
            timestamp?: string;
          }
        | undefined;
      const tracked = snapshot?.trackedFileBackups;
      out.push({
        uuid: String(entry.uuid ?? entry.messageId ?? cryptoRandom()),
        parentUuid: null,
        type: 'checkpoint',
        timestamp: String(snapshot?.timestamp ?? entry.timestamp ?? ''),
        sessionId: String(entry.sessionId ?? ''),
        content: 'Checkpoint Created',
        checkpointData: {
          messageId: String(entry.messageId ?? ''),
          isUpdate: entry.isSnapshotUpdate === true,
          fileCount: tracked ? Object.keys(tracked).length : 0,
        },
        rawJson: entry,
      });
      continue;
    }

    if (type === 'system') {
      out.push({
        uuid: String(entry.uuid ?? cryptoRandom()),
        parentUuid: (entry.parentUuid as string) ?? null,
        type: 'system',
        timestamp: String(entry.timestamp ?? ''),
        sessionId: String(entry.sessionId ?? ''),
        systemSubtype: String(entry.subtype ?? ''),
        content: typeof entry.content === 'string' ? entry.content : undefined,
        agentId: entry.agentId as string | undefined,
        isSidechain: entry.isSidechain === true || undefined,
        compactMetadata: entry.compactMetadata as SessionMessage['compactMetadata'],
        rawJson: entry,
      });
      continue;
    }

    if (type === 'queue-operation') {
      out.push({
        uuid: String(entry.uuid ?? cryptoRandom()),
        parentUuid: null,
        type: 'queue-operation',
        timestamp: String(entry.timestamp ?? ''),
        sessionId: String(entry.sessionId ?? ''),
        queueOperation: String(entry.operation ?? entry.queueOperation ?? ''),
        rawJson: entry,
      });
    }
  }

  // Attach any late tool results (results that appeared before tool_use in page window)
  for (const [toolId, result] of pendingResults) {
    const open = openTools.get(toolId);
    if (open?.toolUse) open.toolUse = { ...open.toolUse, result };
  }

  return out;
}

function cryptoRandom(): string {
  return `msg-${Math.random().toString(36).slice(2, 11)}`;
}

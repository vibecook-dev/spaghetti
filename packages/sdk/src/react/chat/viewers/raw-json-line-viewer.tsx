/**
 * RawJsonLineViewer
 *
 * Displays a single JSONL line with toggle between raw and parsed views.
 * Used in the Raw JSONL debug view to help understand data transformations.
 */

import React, { useState, useMemo } from 'react';
import { Code, FileJson } from 'lucide-react';

interface RawJsonLineViewerProps {
  line: string;
  lineNumber: number;
}

interface ParsedMessage {
  uuid: string;
  parentUuid: string | null;
  type: string;
  timestamp: string;
  sessionId: string;
  content?: string;
  thinking?: string;
  role?: string;
  model?: string;
  toolUse?: {
    toolName: string;
    toolId: string;
    input: Record<string, unknown>;
  };
  usage?: {
    inputTokens: number;
    outputTokens: number;
    cacheCreationInputTokens: number;
    cacheReadInputTokens: number;
  };
  isCompactSummary?: boolean;
  systemSubtype?: string;
  leafUuid?: string;
  checkpointData?: {
    messageId: string;
    isUpdate: boolean;
    fileCount: number;
  };
  agentId?: string;
  isSidechain?: boolean;
  _splitMessages?: ParsedMessage[];
  _toolResults?: Array<{ toolId: string; isError: boolean; content: string }>;
}

/**
 * Simulate parsing a single JSONL entry into SessionMessage format.
 * This mimics the logic in claude-data-parser.ts for debugging purposes.
 */
function parseJsonlEntry(entry: Record<string, unknown>): {
  messages: ParsedMessage[];
  toolResults: Array<{ toolId: string; isError: boolean; content: string }>;
  notes: string[];
} {
  const messages: ParsedMessage[] = [];
  const toolResults: Array<{ toolId: string; isError: boolean; content: string }> = [];
  const notes: string[] = [];

  const baseMessage: ParsedMessage = {
    uuid: entry.uuid as string,
    parentUuid: (entry.parentUuid as string) || null,
    type: entry.type as string,
    timestamp: entry.timestamp as string,
    sessionId: entry.sessionId as string,
    agentId: entry.agentId as string | undefined,
    isSidechain: entry.isSidechain as boolean | undefined,
  };

  if (entry.type === 'user') {
    const message = entry.message as { content?: string | unknown[]; role?: string } | undefined;
    if (message?.content) {
      const userContent = message.content;

      if (Array.isArray(userContent)) {
        const contentArray = userContent as Array<{
          type?: string;
          text?: string;
          tool_use_id?: string;
          is_error?: boolean;
          content?: unknown;
        }>;
        const hasToolResults = contentArray.some((c) => c.type === 'tool_result');

        if (hasToolResults) {
          notes.push('Contains tool_result blocks → extracted to toolResultsMap, message SKIPPED');
          for (const item of contentArray) {
            if (item.type === 'tool_result') {
              toolResults.push({
                toolId: item.tool_use_id || '',
                isError: item.is_error || false,
                content: typeof item.content === 'string' ? item.content : JSON.stringify(item.content),
              });
            }
          }
          return { messages: [], toolResults, notes };
        } else {
          const textParts = contentArray.filter((c) => c.type === 'text').map((c) => c.text || '');
          baseMessage.content = textParts.join('\n');
          notes.push('Extracted text from content array');
        }
      } else if (typeof userContent === 'string') {
        baseMessage.content = userContent;
      }

      baseMessage.role = 'user';

      if (entry.isVisibleInTranscriptOnly && entry.isCompactSummary) {
        baseMessage.type = 'compact_summary';
        baseMessage.isCompactSummary = true;
        notes.push('Type changed: user → compact_summary');
      }

      messages.push(baseMessage);
    }
  } else if (entry.type === 'assistant') {
    const message = entry.message as
      | {
          content?: unknown[];
          model?: string;
          usage?: {
            input_tokens?: number;
            output_tokens?: number;
            cache_creation_input_tokens?: number;
            cache_read_input_tokens?: number;
          };
        }
      | undefined;

    if (message) {
      baseMessage.role = 'assistant';
      baseMessage.model = message.model;

      if (message.usage) {
        baseMessage.usage = {
          inputTokens: message.usage.input_tokens || 0,
          outputTokens: message.usage.output_tokens || 0,
          cacheCreationInputTokens: message.usage.cache_creation_input_tokens || 0,
          cacheReadInputTokens: message.usage.cache_read_input_tokens || 0,
        };
      }

      const splitMessages: ParsedMessage[] = [];

      if (Array.isArray(message.content)) {
        const textParts: string[] = [];
        let thinkingIndex = 0;

        for (const part of message.content) {
          const p = part as {
            type?: string;
            text?: string;
            thinking?: string;
            id?: string;
            name?: string;
            input?: unknown;
          };
          if (p.type === 'text') {
            textParts.push(p.text || '');
          } else if (p.type === 'thinking') {
            const thinkingMsg: ParsedMessage = {
              uuid: `${entry.uuid}-thinking-${thinkingIndex++}`,
              parentUuid: entry.uuid as string,
              type: 'thinking',
              timestamp: entry.timestamp as string,
              sessionId: entry.sessionId as string,
              content: p.thinking,
              model: message.model,
              usage: baseMessage.usage,
            };
            splitMessages.push(thinkingMsg);
            notes.push(`Split out: thinking block → type: 'thinking'`);
          } else if (p.type === 'tool_use') {
            const toolMsg: ParsedMessage = {
              uuid: `${entry.uuid}-${p.id || 'tool'}`,
              parentUuid: entry.uuid as string,
              type: 'tool_use',
              timestamp: entry.timestamp as string,
              sessionId: entry.sessionId as string,
              toolUse: {
                toolName: p.name || 'Unknown Tool',
                toolId: p.id || '',
                input: (p.input as Record<string, unknown>) || {},
              },
            };
            splitMessages.push(toolMsg);
            notes.push(`Split out: tool_use block → type: 'tool_use' (${p.name})`);
          }
        }

        baseMessage.content = textParts.join('\n');
      }

      // Output order: thinking → assistant → tool_use
      const thinkingMsgs = splitMessages.filter((m) => m.type === 'thinking');
      const toolMsgs = splitMessages.filter((m) => m.type === 'tool_use');

      messages.push(...thinkingMsgs);
      if (baseMessage.content) {
        messages.push(baseMessage);
      } else {
        notes.push('No text content → assistant message NOT added');
      }
      messages.push(...toolMsgs);

      if (splitMessages.length > 0) {
        notes.push(
          `Order: ${thinkingMsgs.length} thinking → ${baseMessage.content ? '1 assistant' : '0 assistant'} → ${toolMsgs.length} tool_use`,
        );
      }
    }
  } else if (entry.type === 'summary') {
    baseMessage.type = 'summary';
    baseMessage.content = (entry as { summary?: string }).summary || '';
    baseMessage.leafUuid = (entry as { leafUuid?: string }).leafUuid;
    messages.push(baseMessage);
  } else if (entry.type === 'file-history-snapshot') {
    baseMessage.type = 'checkpoint';
    baseMessage.content = 'Checkpoint Created';
    notes.push('Type changed: file-history-snapshot → checkpoint');

    const snapshot = entry.snapshot as
      | {
          trackedFileBackups?: Record<string, { backupTime?: string }>;
          timestamp?: string;
        }
      | undefined;

    const trackedFiles = snapshot?.trackedFileBackups;
    const isUpdate = (entry.isSnapshotUpdate as boolean) || false;

    if (isUpdate) {
      notes.push('isSnapshotUpdate=true → using latest backupTime');
      if (trackedFiles && typeof trackedFiles === 'object') {
        let latestTime: string | null = null;
        for (const filePath of Object.keys(trackedFiles)) {
          const backup = trackedFiles[filePath];
          if (backup?.backupTime) {
            if (!latestTime || backup.backupTime > latestTime) {
              latestTime = backup.backupTime;
            }
          }
        }
        if (latestTime) {
          baseMessage.timestamp = latestTime;
          notes.push(`Timestamp: ${latestTime} (latest backupTime)`);
        }
      }
    } else {
      notes.push('isSnapshotUpdate=false → using snapshot.timestamp');
      if (snapshot?.timestamp) {
        baseMessage.timestamp = snapshot.timestamp;
        notes.push(`Timestamp: ${snapshot.timestamp} (snapshot creation time)`);
      }
    }

    baseMessage.checkpointData = {
      messageId: entry.messageId as string,
      isUpdate,
      fileCount: trackedFiles ? Object.keys(trackedFiles).length : 0,
    };
    messages.push(baseMessage);
  } else if (entry.type === 'system') {
    baseMessage.content = (entry as { content?: string }).content || '';
    baseMessage.systemSubtype = (entry as { subtype?: string }).subtype;
    messages.push(baseMessage);
  } else if (entry.type === 'tool_result') {
    notes.push('Standalone tool_result → stored in toolResultsMap for later merge');
    const content = entry.content;
    let resultContent = '';
    if (typeof content === 'string') {
      resultContent = content;
    } else if (Array.isArray(content)) {
      resultContent = content.map((c: { text?: string }) => c.text || '').join('\n');
    }
    toolResults.push({
      toolId: (entry as { tool_use_id?: string }).tool_use_id || '',
      isError: (entry as { is_error?: boolean }).is_error || false,
      content: resultContent,
    });
    return { messages: [], toolResults, notes };
  } else {
    // Unknown type, pass through
    messages.push(baseMessage);
    notes.push(`Unknown type: ${entry.type} → passed through as-is`);
  }

  return { messages, toolResults, notes };
}

export function RawJsonLineViewer({ line, lineNumber }: RawJsonLineViewerProps): React.ReactElement | null {
  const [showParsed, setShowParsed] = useState(false);

  const { parsed, parseResult, parseError } = useMemo(() => {
    try {
      const parsed = JSON.parse(line);
      const parseResult = parseJsonlEntry(parsed);
      return { parsed, parseResult, parseError: null };
    } catch (e) {
      return { parsed: null, parseResult: null, parseError: e };
    }
  }, [line]);

  if (!line.trim()) return null;

  if (parseError || !parsed) {
    return <div className="mb-2 opacity-70">{line}</div>;
  }

  return (
    <div className="mb-4 pb-4 border-b border-border">
      {/* Header with line number and toggle */}
      <div className="flex items-center justify-between mb-2">
        <span className="text-[9px] opacity-50">Line {lineNumber}</span>
        <button
          onClick={() => setShowParsed(!showParsed)}
          className="flex items-center gap-1 px-2 py-0.5 rounded text-[9px] font-mono transition-colors"
          style={{
            backgroundColor: showParsed
              ? 'var(--accent)'
              : 'color-mix(in srgb, var(--muted-foreground) 15%, transparent)',
            color: showParsed ? 'white' : 'var(--muted-foreground)',
          }}
        >
          {showParsed ? <Code size={10} /> : <FileJson size={10} />}
          {showParsed ? 'Raw' : 'Parsed'}
        </button>
      </div>

      {showParsed ? (
        /* Parsed View */
        <div className="space-y-3">
          {/* Notes/Transformations */}
          {parseResult && parseResult.notes.length > 0 && (
            <div
              className="p-2 rounded text-[10px]"
              style={{
                backgroundColor: 'color-mix(in srgb, var(--accent) 10%, transparent)',
                border: '1px solid color-mix(in srgb, var(--accent) 30%, transparent)',
              }}
            >
              <div className="font-bold mb-1" style={{ color: 'var(--accent)' }}>
                Transformations:
              </div>
              <ul className="list-disc list-inside space-y-0.5 text-muted-foreground">
                {parseResult.notes.map((note, i) => (
                  <li key={i}>{note}</li>
                ))}
              </ul>
            </div>
          )}

          {/* Tool Results (if extracted) */}
          {parseResult && parseResult.toolResults.length > 0 && (
            <div
              className="p-2 rounded text-[10px]"
              style={{
                backgroundColor: 'rgba(245, 158, 11, 0.1)',
                border: '1px solid rgba(245, 158, 11, 0.3)',
              }}
            >
              <div className="font-bold mb-1" style={{ color: '#f59e0b' }}>
                Extracted Tool Results (for later merge):
              </div>
              {parseResult.toolResults.map((tr, i) => (
                <div key={i} className="mt-1 p-1 rounded bg-background">
                  <div>
                    <strong>toolId:</strong> {tr.toolId}
                  </div>
                  <div>
                    <strong>isError:</strong> {String(tr.isError)}
                  </div>
                  <div>
                    <strong>content:</strong> {tr.content.slice(0, 200)}
                    {tr.content.length > 200 ? '...' : ''}
                  </div>
                </div>
              ))}
            </div>
          )}

          {/* Output Messages */}
          {parseResult && parseResult.messages.length > 0 && (
            <div>
              <div className="text-[10px] font-bold mb-1" style={{ color: '#22c55e' }}>
                Output SessionMessage{parseResult.messages.length > 1 ? 's' : ''} ({parseResult.messages.length}):
              </div>
              {parseResult.messages.map((msg, i) => (
                <pre
                  key={i}
                  className="p-2 rounded text-[10px] mb-2 overflow-x-auto bg-background border border-border"
                >
                  {JSON.stringify(msg, null, 2)}
                </pre>
              ))}
            </div>
          )}

          {/* No output case */}
          {parseResult && parseResult.messages.length === 0 && parseResult.toolResults.length === 0 && (
            <div className="p-2 rounded text-[10px] bg-muted-foreground/10 text-muted-foreground">
              No SessionMessage output (entry was skipped or only produced tool results)
            </div>
          )}
        </div>
      ) : (
        /* Raw View */
        <pre className="whitespace-pre-wrap break-all">{JSON.stringify(parsed, null, 2)}</pre>
      )}
    </div>
  );
}

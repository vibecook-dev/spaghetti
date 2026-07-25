/**
 * TimelineToolUse Component
 *
 * Timeline-style message display for tool use messages.
 */

import React, { useState, useMemo, useCallback, memo } from 'react';
import {
  ChevronDown,
  ChevronRight,
  Terminal,
  FileText,
  Edit3,
  Search,
  Wrench,
  FilePenLine,
  ListTodo,
  Cpu,
  Globe,
  HelpCircle,
  ClipboardList,
  Zap,
  Plug,
  XCircle,
  FileOutput,
  FileCode2,
  CheckCircle2,
  Bot,
} from 'lucide-react';
import { cn } from '../../lib/utils.js';
import { TimelineDot, TimelineRow } from '../timeline';
import { RawJsonViewer, ToolResultRenderer } from '../content';
import {
  EditDiffViewer,
  ReadViewer,
  WriteViewer,
  NotebookEditViewer,
  BashViewer,
  GrepViewer,
  GlobViewer,
  TodoWriteViewer,
  TaskViewer,
  TaskOutputViewer,
  KillShellViewer,
  WebFetchViewer,
  WebSearchViewer,
  AskUserQuestionViewer,
  PlanModeViewer,
  SkillViewer,
  MCPToolViewer,
} from '../viewers';
import { chatCardVariants, chatCardHeaderVariants, timelineHeaderVariants } from '../variants';
import { getToolConfig, typography } from '../theme';
import type { SessionMessage, ConnectorInfo } from '../types';

// =============================================================================
// TYPES
// =============================================================================

interface TodoItem {
  content: string;
  status: 'pending' | 'in_progress' | 'completed';
  activeForm?: string;
}

interface TimelineToolUseProps {
  message: SessionMessage;
  connector: ConnectorInfo;
}

// =============================================================================
// HELPERS
// =============================================================================

function getToolIcon(name: string) {
  if (name.startsWith('mcp__')) return Plug;

  switch (name) {
    case 'Bash':
      return Terminal;
    case 'Read':
      return FileText;
    case 'Write':
      return FilePenLine;
    case 'Edit':
      return Edit3;
    case 'NotebookEdit':
      return FileCode2;
    case 'Grep':
      return Search;
    case 'Glob':
      return Search;
    case 'Task':
      return Cpu;
    case 'TaskOutput':
      return FileOutput;
    case 'KillShell':
      return XCircle;
    case 'WebFetch':
      return Globe;
    case 'WebSearch':
      return Globe;
    case 'TodoWrite':
      return ListTodo;
    case 'AskUserQuestion':
      return HelpCircle;
    case 'EnterPlanMode':
    case 'ExitPlanMode':
      return ClipboardList;
    case 'Skill':
      return Zap;
    default:
      return Wrench;
  }
}

const ToolSummary = memo(function ToolSummary({
  input,
  toolName,
}: {
  input: Record<string, unknown>;
  toolName: string;
}) {
  if (input.command) {
    return <span className="font-mono font-semibold text-accent">$ {String(input.command)}</span>;
  }
  if (input.file_path) {
    return <span className="font-mono font-bold text-foreground">{String(input.file_path)}</span>;
  }
  if (input.pattern) {
    return <span className="font-mono italic text-foreground">"{String(input.pattern)}"</span>;
  }
  return <span>{toolName}</span>;
});

// =============================================================================
// MAIN COMPONENT
// =============================================================================

export const TimelineToolUse = memo(function TimelineToolUse({ message, connector }: TimelineToolUseProps) {
  const toolUse = message.toolUse!;
  const { toolName, input } = toolUse;

  const isEditTool = toolName === 'Edit';
  const isWriteTool = toolName === 'Write';
  const isNotebookEditTool = toolName === 'NotebookEdit';
  const isTodoWriteTool = toolName === 'TodoWrite';
  const isTaskTool = toolName === 'Task';
  const taskSpawnedAgent = isTaskTool && (toolUse.result as any)?.agentId;
  const isTaskOutputTool = toolName === 'TaskOutput';
  const isKillShellTool = toolName === 'KillShell';
  const isGrepTool = toolName === 'Grep';
  const isGlobTool = toolName === 'Glob';
  const isReadTool = toolName === 'Read';
  const isBashTool = toolName === 'Bash';
  const isWebFetchTool = toolName === 'WebFetch';
  const isWebSearchTool = toolName === 'WebSearch';
  const isAskUserQuestionTool = toolName === 'AskUserQuestion';
  const isEnterPlanModeTool = toolName === 'EnterPlanMode';
  const isExitPlanModeTool = toolName === 'ExitPlanMode';
  const isSkillTool = toolName === 'Skill';
  const isMCPTool = toolName.startsWith('mcp__');

  const hasSpecialViewer =
    isEditTool ||
    isWriteTool ||
    isNotebookEditTool ||
    isTodoWriteTool ||
    isTaskTool ||
    isTaskOutputTool ||
    isKillShellTool ||
    isGrepTool ||
    isGlobTool ||
    isReadTool ||
    isBashTool ||
    isWebFetchTool ||
    isWebSearchTool ||
    isAskUserQuestionTool ||
    isEnterPlanModeTool ||
    isExitPlanModeTool ||
    isSkillTool ||
    isMCPTool;

  const [isExpanded, setIsExpanded] = useState(hasSpecialViewer);

  const config = useMemo(() => getToolConfig(toolName), [toolName]);
  const Icon = useMemo(() => getToolIcon(toolName), [toolName]);
  const inputJson = useMemo(() => JSON.stringify(input, null, 2), [input]);

  const handleToggle = useCallback(() => setIsExpanded((prev) => !prev), []);

  const getSummaryText = () => {
    if (isTodoWriteTool) {
      const todos = (input as { todos?: TodoItem[] }).todos || [];
      const completed = todos.filter((t) => t.status === 'completed').length;
      return `${completed}/${todos.length} tasks`;
    }
    if (isTaskTool) return (input as { description?: string }).description || 'Agent task';
    if (isTaskOutputTool) {
      const taskId = (input as { task_id?: string }).task_id || '';
      return taskId.length > 20 ? taskId.slice(0, 20) + '...' : taskId;
    }
    if (isKillShellTool) return (input as { shell_id?: string }).shell_id || 'shell';
    if (isGrepTool) {
      const pattern = (input as { pattern?: string }).pattern || '';
      return `"${pattern.length > 30 ? pattern.slice(0, 30) + '...' : pattern}"`;
    }
    if (isGlobTool) return (input as { pattern?: string }).pattern || '';
    if (isReadTool || isWriteTool) {
      const filePath = (input as { file_path?: string }).file_path || '';
      return filePath.split('/').pop() || filePath;
    }
    if (isNotebookEditTool) {
      const notebookPath = (input as { notebook_path?: string }).notebook_path || '';
      return notebookPath.split('/').pop() || notebookPath;
    }
    if (isBashTool) {
      const cmd = (input as { command?: string }).command || '';
      return cmd.length > 50 ? cmd.slice(0, 50) + '...' : cmd;
    }
    if (isWebFetchTool) {
      const url = (input as { url?: string }).url || '';
      try {
        return new URL(url).hostname;
      } catch {
        return url.slice(0, 30);
      }
    }
    if (isWebSearchTool) {
      const query = (input as { query?: string }).query || '';
      return `"${query.length > 30 ? query.slice(0, 30) + '...' : query}"`;
    }
    if (isAskUserQuestionTool) {
      const questions = (input as { questions?: { question: string }[] }).questions || [];
      return questions.length > 0 ? questions[0].question.slice(0, 40) + '...' : 'Question';
    }
    if (isEnterPlanModeTool) return 'Entering plan mode...';
    if (isExitPlanModeTool) return 'Exiting plan mode';
    if (isSkillTool) return `/${(input as { skill?: string }).skill || 'skill'}`;
    if (isMCPTool) {
      const parts = toolName.split('__');
      return `${parts[1] || 'mcp'} → ${parts.slice(2).join('__') || 'tool'}`;
    }
    return null;
  };

  const summaryText = getSummaryText();

  const renderViewer = () => {
    if (isEditTool) return <EditDiffViewer input={input as Record<string, unknown>} />;
    if (isWriteTool) return <WriteViewer input={input as Record<string, unknown>} />;
    if (isNotebookEditTool) return <NotebookEditViewer input={input as Record<string, unknown>} />;
    if (isReadTool) return <ReadViewer input={input as Record<string, unknown>} />;
    if (isGrepTool) return <GrepViewer input={input as Record<string, unknown>} />;
    if (isGlobTool) return <GlobViewer input={input as Record<string, unknown>} />;
    if (isBashTool) return <BashViewer input={input as Record<string, unknown>} />;
    if (isTaskTool) return <TaskViewer input={input as Record<string, unknown>} />;
    if (isTaskOutputTool) return <TaskOutputViewer input={input as Record<string, unknown>} />;
    if (isKillShellTool) return <KillShellViewer input={input as Record<string, unknown>} />;
    if (isWebFetchTool) return <WebFetchViewer input={input as Record<string, unknown>} />;
    if (isWebSearchTool) return <WebSearchViewer input={input as Record<string, unknown>} />;
    if (isTodoWriteTool) return <TodoWriteViewer input={input as Record<string, unknown>} />;
    if (isAskUserQuestionTool) return <AskUserQuestionViewer input={input as Record<string, unknown>} />;
    if (isEnterPlanModeTool) return <PlanModeViewer mode="enter" />;
    if (isExitPlanModeTool) return <PlanModeViewer mode="exit" plan={(input as { plan?: string }).plan} />;
    if (isSkillTool) return <SkillViewer input={input as Record<string, unknown>} />;
    if (isMCPTool) return <MCPToolViewer toolName={toolName} input={input as Record<string, unknown>} />;

    return (
      <div
        className="border border-border p-2 text-[10px] font-mono opacity-90 overflow-x-auto shadow-sm leading-tight rounded bg-background"
        style={{ fontFamily: typography.mono }}
      >
        {inputJson}
      </div>
    );
  };

  const icon = <TimelineDot color={config.color} icon={Icon} />;

  return (
    <TimelineRow
      icon={icon}
      indent={connector.indent}
      nextIndent={connector.nextIndent}
      hasNext={connector.hasNext}
      color={config.color}
      isAgent={connector.isAgent}
    >
      {/* Header */}
      <div className={cn(timelineHeaderVariants())} onClick={handleToggle}>
        <span
          className="text-[9px] font-bold uppercase tracking-wider w-10 text-right shrink-0 transition-opacity group-hover/header:opacity-100"
          style={{ color: config.color }}
        >
          {config.label}
        </span>
        <div className="text-[11px] truncate opacity-90 group-hover/header:opacity-100 transition-opacity font-mono">
          {summaryText || <ToolSummary input={input as Record<string, unknown>} toolName={toolName} />}
        </div>
        <div className="ml-auto opacity-20 group-hover/header:opacity-100 transition-opacity">
          {isExpanded ? <ChevronDown size={10} /> : <ChevronRight size={10} />}
        </div>
      </div>

      {/* Expanded content */}
      {isExpanded && (
        <div className="mt-1">
          {renderViewer()}

          {/* Tool Result */}
          {taskSpawnedAgent ? (
            <div
              className={cn(chatCardVariants({ variant: 'agent' }), 'mt-2 flex items-center justify-between px-3 py-2')}
            >
              <div className="flex items-center gap-2">
                <Bot size={14} style={{ color: '#0EA5E9' }} />
                <span className="text-[11px] font-semibold" style={{ color: '#0EA5E9' }}>
                  Agent spawned
                </span>
                <span className="text-[10px] font-mono opacity-70" style={{ color: '#0EA5E9' }}>
                  {(toolUse.result as any)?.agentId}
                </span>
              </div>
              <span className="text-[9px] opacity-50" style={{ color: '#0EA5E9' }}>
                ↓ thread below
              </span>
            </div>
          ) : (
            toolUse.result && (
              <div className={cn(chatCardVariants({ variant: toolUse.result.isError ? 'error' : 'default' }), 'mt-2')}>
                {/* Result header */}
                <div className={cn(chatCardHeaderVariants(), 'py-1.5')}>
                  {toolUse.result.isError ? (
                    <XCircle size={12} style={{ color: '#ef4444' }} />
                  ) : (
                    <CheckCircle2 size={12} style={{ color: '#10b981' }} />
                  )}
                  <span
                    className="text-[10px] font-bold"
                    style={{ color: toolUse.result.isError ? '#ef4444' : '#10b981' }}
                  >
                    {toolUse.result.isError ? 'Error' : 'Result'}
                  </span>
                </div>
                {/* Result content */}
                <div className="px-3 py-2 max-h-48 overflow-auto bg-background">
                  <ToolResultRenderer
                    toolName={toolName}
                    content={toolUse.result.content}
                    isError={toolUse.result.isError}
                    rawJson={toolUse.result.rawJson}
                  />
                </div>
              </div>
            )
          )}

          <RawJsonViewer
            data={{
              tool_use: message.rawJson,
              ...(toolUse.result?.rawJson ? { tool_result: toolUse.result.rawJson } : {}),
            }}
          />
        </div>
      )}
    </TimelineRow>
  );
});

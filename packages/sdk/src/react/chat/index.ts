// Types
export type {
  SessionMessage,
  ToolUseInfo,
  ToolResultInfo,
  ThinkingMetadata,
  HookInfo,
  ConnectorInfo,
} from './types.js';

// Theme
export {
  timeline,
  toolColors,
  messageColors,
  syntaxColors,
  fileTypeColors,
  statusColors,
  toolConfig,
  typography,
  hexToRgba,
  getFileTypeColor,
  getToolConfig,
  getStatusColors,
} from './theme.js';

// Variants
export { chatCardVariants, chatCardHeaderVariants, badgeVariants, timelineHeaderVariants } from './variants.js';

// Timeline
export {
  TimelineRow,
  TimelineDot,
  AssistantDot,
  NodeConnector,
  TimelineLine,
  TimeGroupSeparator,
} from './timeline/index.js';

// Content
export { MarkdownContent, CodeBlock, RawJsonViewer, ToolResultRenderer } from './content/index.js';

// Messages
export {
  UserMessage,
  TimelineAssistant,
  TimelineToolUse,
  TimelineThinking,
  TimelineCompactSummary,
  TimelineCheckpoint,
  TimelineSystem,
  TimelineSummary,
  TimelineQueueOperation,
} from './messages/index.js';

// Viewers
export {
  BashViewer,
  EditDiffViewer,
  ReadViewer,
  WriteViewer,
  NotebookEditViewer,
  GrepViewer,
  GlobViewer,
  TaskViewer,
  TaskOutputViewer,
  TodoWriteViewer,
  WebFetchViewer,
  WebSearchViewer,
  AskUserQuestionViewer,
  KillShellViewer,
  PlanModeViewer,
  SkillViewer,
  MCPToolViewer,
  RawJsonLineViewer,
} from './viewers/index.js';

// Renderer
export { TimelineMessageRenderer, isTimelineType } from './renderer/index.js';

// Utilities
export {
  formatTokens,
  formatDuration,
  formatTime,
  useIsDark,
  shouldShowTimestamp,
  formatChatTime,
} from './utils/index.js';

// Raw spaghetti messages → timeline SessionMessage[]
export { transformRawMessagesToTimeline, type TransformRawMessagesOptions } from './transform-messages.js';

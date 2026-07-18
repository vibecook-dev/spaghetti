/**
 * Agent sources — adapters for local agent products.
 */

export type {
  AgentSource,
  AgentSourceId,
  AgentSourcePaths,
  ClaudeCodePaths,
  ExtractedMessage,
  MessageExtractor,
  IngestHooks,
  SessionTokenApi,
  MessageTokenBag,
  SessionMessageTextRow,
} from './types.js';
export {
  createClaudeCodeSource,
  defaultClaudeDir,
  defaultSpaghettiStateDir,
  buildClaudeCodePaths,
  ClaudeCodeLifecycleOwner,
  createClaudeCodeParser,
  createProjectParser,
  classifyClaudePath,
  type ClaudeCodeSourceOptions,
  type ClaudeCodeAgentSource,
  type ClaudeCodeParser,
  type ClaudeCodeParserOptions,
} from './claude-code/index.js';
export {
  createCodexSource,
  createCodexReader,
  codexMessageExtractor,
  CodexReader,
  CodexLifecycleOwner,
  createCodexIngestHooks,
  classifyCodexPath,
  defaultCodexDir,
  buildCodexPaths,
  parseCodexTokenCount,
  countTextTokens,
  estimateTokensFromMessageRows,
  type CodexSourceOptions,
  type CodexTokenUsage,
  type ParsedCodexTokenCount,
  type EstimatedMessageTokens,
} from './codex/index.js';
export {
  createGrokSource,
  createGrokReader,
  grokMessageExtractor,
  GrokReader,
  GrokLifecycleOwner,
  createGrokLiveWatch,
  classifyGrokPath,
  defaultGrokDir,
  buildGrokPaths,
  type GrokSourceOptions,
  type GrokReadOptions,
  type GrokLiveWatch,
} from './grok/index.js';
export { sourceReportsPerMessageTokens, sourceDisplayName, sourceDisplayRoot } from './capabilities.js';
export { adaptMessageForDisplay, adaptMessagesForDisplay } from './adapt-display-messages.js';
export {
  createLifecycleOwnerForSource,
  isLifecycleOwnerRegistered,
  registeredLifecycleOwnerIds,
  type LifecycleOwnerFactoryDeps,
  type LifecycleOwnerFactory,
} from './registry.js';

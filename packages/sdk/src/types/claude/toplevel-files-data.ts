/**
 * TypeScript interfaces for top-level files in ~/.claude/
 */

export interface StatusLineConfig {
  type: string;
  command?: string;
}

export interface PermissionsConfig {
  allow: string[];
}

export interface HookCommand {
  type: string;
  command: string;
  timeout?: number;
}

export interface HookMatcher {
  matcher?: string;
  hooks: HookCommand[];
}

export interface ExtraKnownMarketplace {
  source: {
    source: string;
    repo?: string;
    path?: string;
  };
}

export interface SettingsFile {
  permissions: PermissionsConfig;
  /**
   * Default model alias, e.g. `opus[1m]`. The `[1m]` suffix selects the
   * 1M-token context variant, so this is not a bare model id.
   */
  model?: string;
  effortLevel?: string;
  enabledPlugins?: Record<string, boolean>;
  alwaysThinkingEnabled?: boolean;
  statusLine?: StatusLineConfig;
  env?: Record<string, string>;
  cleanupPeriodDays?: number;
  extraKnownMarketplaces?: Record<string, ExtraKnownMarketplace>;
  hooks?: Record<string, HookMatcher[]>;
  /** UI mode, e.g. 'fullscreen'. */
  tui?: string;
  autoCompactEnabled?: boolean;
  agentPushNotifEnabled?: boolean;
  /** Enable Claude Code's inline prompt-completion suggestions. */
  promptSuggestionEnabled?: boolean;
  skipWorkflowUsageWarning?: boolean;
  skipAutoPermissionPrompt?: boolean;
  /** Co-authorship trailers Claude Code appends, e.g. `{ commit: '' }`. */
  attribution?: Record<string, string>;
}

export interface DailyActivity {
  date: string;
  messageCount: number;
  sessionCount: number;
  toolCallCount: number;
}

export interface DailyModelTokens {
  date: string;
  tokensByModel: Record<string, number>;
}

export interface ModelUsageStats {
  inputTokens: number;
  outputTokens: number;
  cacheReadInputTokens: number;
  cacheCreationInputTokens: number;
  webSearchRequests: number;
  costUSD: number;
  contextWindow: number;
  maxOutputTokens: number;
}

export interface LongestSession {
  sessionId: string;
  duration: number;
  messageCount: number;
  timestamp: string;
}

export interface StatsCacheFile {
  version: number;
  lastComputedDate: string;
  dailyActivity: DailyActivity[];
  dailyModelTokens: DailyModelTokens[];
  /**
   * Schema version for `dailyModelTokens` alone, tracked separately from the
   * file-level `version` so Claude Code can rebuild that projection without
   * invalidating the rest of the cache.
   */
  dailyModelTokensVersion?: number;
  modelUsage: Record<string, ModelUsageStats>;
  totalSessions: number;
  totalMessages: number;
  longestSession: LongestSession;
  firstSessionDate: string;
  hourCounts: Record<string, number>;
  /** Dropped by Claude Code around stats-cache v5; still present in older caches. */
  totalSpeculationTimeSavedMs?: number;
}

export interface HistoryPastedContent {
  id: number;
  type: string;
  /**
   * Present on older entries. Claude Code migrated paste storage to a
   * content-addressed `contentHash` (paste-cache/), so most recent
   * entries carry `contentHash` and omit inline `content`.
   */
  content?: string;
  contentHash?: string;
}

export interface HistoryEntry {
  display: string;
  pastedContents: Record<string, HistoryPastedContent>;
  timestamp: number;
  project: string;
  sessionId: string;
}

export interface HistoryFile {
  entries: HistoryEntry[];
}

export interface StatusLineCommandFile {
  content: string;
  size: number;
}

/** ~/.claude/mcp-needs-auth-cache.json */
export interface McpNeedsAuthCache {
  // Some entries also carry an auth `id`; most carry only `timestamp`.
  [serverName: string]: { timestamp: number; id?: string };
}

/** ~/.claude/sessions/{PID}.json — maps running PIDs to session IDs */
export interface ActiveSessionFile {
  pid: number;
  sessionId: string;
  cwd: string;
  startedAt: number;
  kind?: string;
  entrypoint?: string;
  name?: string;
  /**
   * Opaque native timestamp-like metadata for the current name. Its transition
   * semantics are not yet fixture-proven, so consumers must not use it as an
   * activity, ordering, or session-lifecycle timestamp.
   */
  nameSince?: number;
  // Fields Claude Code now writes in the active-session registry.
  status?: string;
  updatedAt?: number;
  statusUpdatedAt?: number;
  procStart?: string;
  version?: string;
  peerProtocol?: number;
  nameSource?: string;
  bridgeSessionId?: string;
  messagingSocketPath?: string;
}

/**
 * Metadata for a `sessions/{pid}.{sessionHash}.key` bridge-auth sidecar.
 *
 * The key material is deliberately not exposed through this type: callers may
 * inventory the file without moving a live session secret into application
 * data or logs.
 */
export interface ActiveSessionKeyFile {
  pid: number;
  sessionHash: string;
  size: number;
}

export interface TopLevelFiles {
  settings: SettingsFile;
  statsCache: StatsCacheFile;
  history: HistoryFile;
  statusLineCommand?: StatusLineCommandFile;
}

/**
 * Public type barrel — re-exports Claude on-disk shapes + Spaghetti runtime
 * types, plus Claude aggregation models used by parsers and the store.
 *
 * Layout:
 * - `types/claude/`     — ~/.claude on-disk product shapes
 *
 * Prefer importing from `types/claude` in product code;
 * this barrel remains stable for external consumers.
 */

// ── Claude on-disk ─────────────────────────────────────────────────────────
export * from './claude/index.js';

// ── Spaghetti runtime ──────────────────────────────────────────────────────

// ═══════════════════════════════════════════════════════════════════════════
// CLAUDE CODE AGENT — top-level aggregation (parser output models)
// ═══════════════════════════════════════════════════════════════════════════

import type {
  SessionsIndex,
  SessionIndexEntry,
  SessionMessage,
  SubagentType,
  PersistedToolResult,
  ProjectMemory,
} from './claude/projects.js';
import type { FileHistorySession } from './claude/file-history-data.js';
import type { TodoFile } from './claude/todos.js';
import type { TaskEntry } from './claude/tasks.js';
import type { PlanFile } from './claude/plans-data.js';
import type {
  SettingsFile,
  StatusLineCommandFile,
  StatsCacheFile,
  HistoryFile,
  McpNeedsAuthCache,
} from './claude/toplevel-files-data.js';
import type { PluginsDirectory } from './claude/plugins-data.js';
import type { StatsigDirectory } from './claude/statsig-data.js';
import type { IdeDirectory } from './claude/ide-data.js';
import type { ShellSnapshotsDirectory } from './claude/shell-snapshots-data.js';
import type { CacheDirectory } from './claude/cache-data.js';
import type { TelemetryDirectory } from './claude/telemetry-data.js';
import type { DebugLogFile, DebugLatestSymlink } from './claude/debug.js';
import type { PasteCacheDirectory } from './claude/paste-cache-data.js';
import type { SessionEnvDirectory } from './claude/session-env.js';
import type { TeamDirectory } from './claude/teams-data.js';

export interface ClaudeCodeAgentData {
  projects: Project[];
  config: AgentConfig;
  analytics: AgentAnalytic;
}

export interface Project {
  slug: string;
  originalPath: string;
  sessionsIndex: SessionsIndex;
  sessions: Session[];
  memory: ProjectMemory | null;
}

export interface Session {
  sessionId: string;
  indexEntry: SessionIndexEntry;
  messages: SessionMessage[];
  subagents: SubagentTranscript[];
  toolResults: PersistedToolResult[];
  fileHistory: FileHistorySession | null;
  todos: TodoFile[];
  task: TaskEntry | null;
  plan: PlanFile | null;
  workflows: WorkflowRun[];
}

/**
 * One agent-orchestration workflow run (the Workflow tool), recorded at
 * `projects/{slug}/{sessionId}/workflows/{runId}.json`. Session-scoped.
 * Its subagent transcripts live under
 * `subagents/workflows/{runId}/agent-*.jsonl` and are grouped to this
 * run via `SubagentTranscript.workflowId`.
 */
export interface WorkflowRun {
  /** `wf_...` — the run id (matches the `workflows/` dir entry name). */
  workflowId: string;
  name: string;
  status: string;
  agentCount: number;
  totalTokens: number;
  totalToolCalls: number;
  durationMs: number;
  /** Number of nested subagent transcripts discovered for this run. */
  subagentCount: number;
  /** The full raw `{runId}.json` run record (phases/result/progress/logs/...). */
  data: Record<string, unknown>;
  /** Parsed `journal.jsonl` entries (started/result events), if present. */
  journal: unknown[];
}

/** Subagent metadata from agent-{id}.meta.json files */
export interface SubagentMeta {
  /** Free-form agent type (e.g. 'general-purpose', 'Explore', 'agent-workflow'). */
  agentType: string;
  /** Present on flat subagent metas; absent on nested workflow metas. */
  description?: string;
  name?: string;
  spawnDepth?: number;
  worktreePath?: string;
  /** Parent agent identifier used by newer nested-agent records. */
  parentAgentId?: string;
  /** Model and scheduling metadata emitted by team/parallel-agent runs. */
  model?: string;
  taskKind?: string;
  teamName?: string;
  color?: string;
  planModeRequired?: boolean;
  permissionMode?: string;
  /**
   * `tool_use` id of the Agent call that spawned this subagent — the join
   * back to the assistant message that launched it.
   */
  toolUseId?: string;
}

export interface SubagentTranscript {
  agentId: string;
  agentType: SubagentType;
  fileName: string;
  messages: SessionMessage[];
  meta?: SubagentMeta;
  /**
   * The `wf_...` run this transcript belongs to, or `''` for a
   * top-level (non-workflow) subagent. Groups nested workflow
   * transcripts under their run.
   */
  workflowId: string;
}

export interface AgentConfig {
  settings: SettingsFile;
  /**
   * `settings.local.json` — per-directory overrides layered over
   * `settings`. Null when absent. Effective permissions/hooks/env are
   * `settings` merged with `settingsLocal` (local wins); consumers that
   * display "effective" config must apply that merge rather than reading
   * `settings` alone.
   */
  settingsLocal: SettingsFile | null;
  plugins: PluginsDirectory;
  statsig: StatsigDirectory;
  ide: IdeDirectory;
  shellSnapshots: ShellSnapshotsDirectory;
  cache: CacheDirectory;
  statusLineCommand: StatusLineCommandFile | null;
  teams: TeamDirectory[];
  /** `mcp-needs-auth-cache.json` — MCP servers awaiting auth. Null when absent. */
  mcpNeedsAuth: McpNeedsAuthCache | null;
}

export interface AgentAnalytic {
  statsCache: StatsCacheFile;
  history: HistoryFile;
  telemetry: TelemetryDirectory;
  debugLogs: DebugLogFile[];
  debugLatest: DebugLatestSymlink | null;
  pasteCache: PasteCacheDirectory;
  sessionEnv: SessionEnvDirectory;
}

import * as path from 'node:path';
import type { FileService } from '../../../io/index.js';
import type {
  Project,
  Session,
  SessionsIndex,
  SessionIndexEntry,
  SessionMessage,
  SubagentTranscript,
  SubagentType,
  SubagentMeta,
  PersistedToolResult,
  ProjectMemory,
  FileHistorySession,
  FileHistorySnapshotFile,
  TodoFile,
  TodoItem,
  TaskEntry,
  PlanFile,
  WorkflowRun,
} from '../../../types/index.js';
import type { ProjectParseSink } from '../../../data/parse-sink.js';
import { extractClaudeHumanPrompt } from '../session-metadata.js';
import {
  parseSubagentFilename,
  inferSubagentType,
  parseTodoFilename,
  parseFileHistoryFilename,
  parsePlanFilename,
} from './filename-conventions.js';

// ═══════════════════════════════════════════════════════════════════════════════
// PROJECT PARSER OPTIONS
// ═══════════════════════════════════════════════════════════════════════════════

export interface ProjectParserOptions {
  skipSessionMessages?: boolean;
}

// ═══════════════════════════════════════════════════════════════════════════════
// SLUG SHAPE
// ═══════════════════════════════════════════════════════════════════════════════

export interface SlugShape {
  /** Precedes the first segment: `/` on POSIX, `D:\` on Windows. */
  prefix: string;
  /** Joins segments: `/` on POSIX, `\` on Windows. */
  sep: string;
  /** The dash-encoded remainder, still to be split. */
  rest: string;
}

/**
 * Split a project slug into its fixed prefix, separator, and encoded tail.
 *
 * Claude Code derives a slug from the project's absolute cwd, so the
 * leading characters tell us which platform wrote it:
 *
 * | cwd                     | slug                | shape      |
 * |-------------------------|---------------------|------------|
 * | `/Users/me/app`         | `-Users-me-app`     | `/`, `/`   |
 * | `D:\Projects\app`       | `D--Projects-app`   | `D:\`, `\` |
 * | `D:\Projects\app` (old) | `D:-Projects-app`   | `D:\`, `\` |
 *
 * A POSIX cwd is always absolute, so it always yields a leading `-`.
 * That makes "no leading dash, but starts with `<letter>--` or
 * `<letter>:-`" an unambiguous Windows drive marker. Both Windows
 * spellings occur in the wild: current Claude Code folds the colon into
 * `-` (giving the doubled dash), older builds and the Codex/Grok readers
 * preserve it.
 *
 * Detection is deliberately platform-independent rather than keyed off
 * `path.sep` — a synced `~/.claude` must decode to the same path text on
 * any host, and the CLI compares these against `process.cwd()` verbatim.
 * Keep in sync with `slug_shape` in
 * `crates/spaghetti-napi/src/claude/project_parser.rs`.
 */
export function slugShape(slug: string): SlugShape {
  // `X--…` or `X:-…` — a Windows drive letter. Length 3 is the bare drive
  // root (`D--` → `D:\`), so `>=` not `>`.
  if (slug.length >= 3 && /[A-Za-z]/.test(slug[0]!) && (slug[1] === '-' || slug[1] === ':') && slug[2] === '-') {
    return { prefix: `${slug[0]!.toUpperCase()}:\\`, sep: '\\', rest: slug.slice(3) };
  }
  if (slug.startsWith('-')) {
    return { prefix: '/', sep: '/', rest: slug.slice(1) };
  }
  return { prefix: '', sep: '/', rest: slug };
}

// ═══════════════════════════════════════════════════════════════════════════════
// PROJECT PARSER INTERFACE
// ═══════════════════════════════════════════════════════════════════════════════

export interface ProjectParser {
  parseAllProjects(rootDir: string, options?: ProjectParserOptions): Project[];
  parseAllProjectsStreaming(rootDir: string, sink: ProjectParseSink, options?: ProjectParserOptions): void;
  /** Parse a single project in streaming mode, sending data to the sink as it's discovered. */
  parseProjectStreaming(rootDir: string, slug: string, sink: ProjectParseSink, options?: ProjectParserOptions): void;
  parseProject(rootDir: string, slug: string, options?: ProjectParserOptions): Project | null;
  parseSession(rootDir: string, slug: string, sessionId: string): Session | null;
}

// ═══════════════════════════════════════════════════════════════════════════════
// IMPLEMENTATION
// ═══════════════════════════════════════════════════════════════════════════════

export class ProjectParserImpl implements ProjectParser {
  // Cached plan index to avoid re-scanning plan files for every project.
  // Keyed by rootDir so it auto-invalidates if the directory changes.
  private cachedPlanIndex: Map<string, PlanFile> | null = null;
  private cachedPlanIndexRootDir: string | null = null;

  constructor(private fileService: FileService) {}

  /**
   * Get or build the plan index, caching it for the lifetime of this parser
   * instance (i.e. for one full cold/warm start cycle).
   */
  private getPlanIndex(rootDir: string): Map<string, PlanFile> {
    if (this.cachedPlanIndex && this.cachedPlanIndexRootDir === rootDir) {
      return this.cachedPlanIndex;
    }
    this.cachedPlanIndex = this.buildPlanIndex(rootDir);
    this.cachedPlanIndexRootDir = rootDir;
    return this.cachedPlanIndex;
  }

  parseAllProjects(rootDir: string, options?: ProjectParserOptions): Project[] {
    const projectsDir = path.join(rootDir, 'projects');
    const projects: Project[] = [];
    const planIndex = this.getPlanIndex(rootDir);

    try {
      const projectPaths = this.fileService.scanDirectorySync(projectsDir, {
        directoriesOnly: true,
      });

      for (const projectPath of projectPaths) {
        try {
          const slug = path.basename(projectPath);
          const project = this.parseProjectInternal(rootDir, slug, options, planIndex);
          if (project) projects.push(project);
        } catch {
          // skip bad project
        }
      }
    } catch {
      // projects dir doesn't exist
    }

    return projects;
  }

  parseAllProjectsStreaming(rootDir: string, sink: ProjectParseSink, options?: ProjectParserOptions): void {
    const projectsDir = path.join(rootDir, 'projects');
    const planIndex = this.getPlanIndex(rootDir);

    // Emit all plans first
    for (const [planSlug, plan] of planIndex) {
      sink.onPlan(planSlug, plan);
    }

    try {
      const projectPaths = this.fileService.scanDirectorySync(projectsDir, {
        directoriesOnly: true,
      });

      for (const projectPath of projectPaths) {
        try {
          const slug = path.basename(projectPath);
          this.parseProjectStreamingInternal(rootDir, slug, sink, options, planIndex);
        } catch {
          // skip bad project
        }
      }
    } catch {
      // projects dir doesn't exist
    }
  }

  parseProjectStreaming(rootDir: string, slug: string, sink: ProjectParseSink, options?: ProjectParserOptions): void {
    const planIndex = this.getPlanIndex(rootDir);

    // Emit all plans first (same as parseAllProjectsStreaming)
    for (const [planSlug, plan] of planIndex) {
      sink.onPlan(planSlug, plan);
    }

    this.parseProjectStreamingInternal(rootDir, slug, sink, options, planIndex);
  }

  private parseProjectStreamingInternal(
    rootDir: string,
    slug: string,
    sink: ProjectParseSink,
    options: ProjectParserOptions | undefined,
    _planIndex: Map<string, PlanFile>,
  ): void {
    const projectDir = path.join(rootDir, 'projects', slug);
    const sessionsIndex = this.parseSessionsIndex(projectDir);
    const originalPath = sessionsIndex.originalPath ?? this.slugToPath(slug);
    const skipMessages = options?.skipSessionMessages ?? false;

    sink.onProject(slug, originalPath, sessionsIndex);

    // Emit project memory if present
    const memory = this.parseProjectMemory(slug, projectDir);
    if (memory) {
      sink.onProjectMemory(slug, memory.content);
    }

    // Process each session
    for (const entry of sessionsIndex.entries) {
      try {
        const sessionId = entry.sessionId;
        sink.onSession(slug, entry);

        if (!skipMessages) {
          // Stream messages using the streaming JSONL reader.
          // Try the canonical path first; fall back to entry.fullPath if
          // the canonical path doesn't exist (e.g. stale index entries
          // that reference relocated files).
          const canonicalPath = path.join(projectDir, `${sessionId}.jsonl`);
          const filePath = this.fileService.exists(canonicalPath)
            ? canonicalPath
            : entry.fullPath && this.fileService.exists(entry.fullPath)
              ? entry.fullPath
              : canonicalPath;
          let messageCount = 0;
          let lastBytePosition = 0;

          try {
            const streamResult = this.fileService.readJsonlStreaming<SessionMessage>(
              filePath,
              (message, index, byteOffset) => {
                sink.onMessage(slug, sessionId, message, index, byteOffset);
                messageCount++;
                lastBytePosition = byteOffset;
              },
            );
            lastBytePosition = streamResult.finalBytePosition;
          } catch {
            // JSONL file doesn't exist or is unreadable
          }

          // Subagents (incl. nested workflow transcripts, tagged by workflowId)
          const subagents = this.parseSubagents(projectDir, sessionId);
          for (const subagent of subagents) {
            sink.onSubagent(slug, sessionId, subagent);
          }

          // Workflow run records (agent-orchestration analytics)
          const workflows = this.parseWorkflows(projectDir, sessionId);
          for (const workflow of workflows) {
            sink.onWorkflow(slug, sessionId, workflow);
          }

          // Tool results
          const toolResults = this.parseToolResults(projectDir, sessionId);
          for (const toolResult of toolResults) {
            sink.onToolResult(slug, sessionId, toolResult);
          }

          sink.onSessionComplete(slug, sessionId, messageCount, lastBytePosition);
        } else {
          sink.onSessionComplete(slug, sessionId, 0, 0);
        }

        // File history (always parsed, not gated by skipMessages)
        const fileHistory = this.parseFileHistory(rootDir, sessionId);
        if (fileHistory) {
          sink.onFileHistory(sessionId, fileHistory);
        }

        // Todos
        const todos = this.parseTodos(rootDir, sessionId);
        for (const todo of todos) {
          sink.onTodo(sessionId, todo);
        }

        // Task
        const task = this.parseTask(rootDir, sessionId);
        if (task) {
          sink.onTask(sessionId, task);
        }
      } catch {
        // skip bad session
      }
    }

    sink.onProjectComplete(slug);
  }

  parseProject(rootDir: string, slug: string, options?: ProjectParserOptions): Project | null {
    const planIndex = this.getPlanIndex(rootDir);
    return this.parseProjectInternal(rootDir, slug, options, planIndex);
  }

  private parseProjectInternal(
    rootDir: string,
    slug: string,
    options: ProjectParserOptions | undefined,
    planIndex: Map<string, PlanFile>,
  ): Project | null {
    const projectDir = path.join(rootDir, 'projects', slug);
    const sessionsIndex = this.parseSessionsIndex(projectDir);
    const originalPath = sessionsIndex.originalPath ?? this.slugToPath(slug);

    const sessions: Session[] = [];
    for (const entry of sessionsIndex.entries) {
      try {
        const session = this.buildSession(rootDir, projectDir, slug, entry, options, planIndex);
        sessions.push(session);
      } catch {
        // skip bad session
      }
    }

    const memory = this.parseProjectMemory(slug, projectDir);

    return { slug, originalPath, sessionsIndex, sessions, memory };
  }

  parseSession(rootDir: string, slug: string, sessionId: string): Session | null {
    const projectDir = path.join(rootDir, 'projects', slug);
    const sessionsIndex = this.parseSessionsIndex(projectDir);
    const entry = sessionsIndex.entries.find((e) => e.sessionId === sessionId);
    if (!entry) return null;

    const planIndex = this.getPlanIndex(rootDir);
    try {
      return this.buildSession(rootDir, projectDir, slug, entry, undefined, planIndex);
    } catch {
      return null;
    }
  }

  private buildSession(
    rootDir: string,
    projectDir: string,
    slug: string,
    entry: SessionIndexEntry,
    options: ProjectParserOptions | undefined,
    planIndex: Map<string, PlanFile>,
  ): Session {
    const sessionId = entry.sessionId;
    const skipMessages = options?.skipSessionMessages ?? false;

    const messages = skipMessages ? [] : this.parseSessionMessages(projectDir, sessionId, entry.fullPath);

    const planSlug =
      messages.length > 0
        ? this.extractPlanSlugFromMessages(messages, planIndex)
        : this.peekPlanSlug(projectDir, sessionId, planIndex);

    return {
      sessionId,
      indexEntry: entry,
      messages,
      subagents: skipMessages ? [] : this.parseSubagents(projectDir, sessionId),
      workflows: skipMessages ? [] : this.parseWorkflows(projectDir, sessionId),
      toolResults: skipMessages ? [] : this.parseToolResults(projectDir, sessionId),
      fileHistory: this.parseFileHistory(rootDir, sessionId),
      todos: this.parseTodos(rootDir, sessionId),
      task: this.parseTask(rootDir, sessionId),
      plan: planSlug ? (planIndex.get(planSlug) ?? null) : null,
    };
  }

  private parseSessionsIndex(projectDir: string): SessionsIndex {
    try {
      const index = this.fileService.readJsonSync<SessionsIndex>(path.join(projectDir, 'sessions-index.json'));
      if (index && index.entries.length > 0) {
        // The sessions-index.json may be stale — it can list sessions whose
        // JSONL files no longer exist, and miss JSONL files that do exist on
        // disk.  Merge any on-disk JSONL files that the index doesn't know
        // about so we never silently drop messages.
        const merged = this.mergeWithDiscoveredEntries(index.entries, projectDir, index.originalPath);
        return { ...index, entries: merged };
      }
      if (index?.originalPath) {
        return { ...index, entries: this.discoverSessionEntries(projectDir, index.originalPath) };
      }
    } catch {
      // sessions-index.json missing or unreadable
    }
    return {
      version: 1,
      entries: this.discoverSessionEntries(projectDir, undefined),
    };
  }

  /**
   * Merge entries from sessions-index.json with JSONL files discovered on
   * disk.  Any on-disk JSONL file whose session ID is NOT already in the
   * index gets a freshly-built entry appended.  This handles the common case
   * where the index is stale (e.g. after a Claude upgrade or migration).
   */
  private mergeWithDiscoveredEntries(
    indexEntries: SessionIndexEntry[],
    projectDir: string,
    originalPath: string | undefined,
  ): SessionIndexEntry[] {
    const indexedIds = new Set(indexEntries.map((e) => e.sessionId));
    const discovered = this.discoverSessionEntries(projectDir, originalPath);

    const extra = discovered.filter((e) => !indexedIds.has(e.sessionId));
    if (extra.length === 0) return indexEntries;

    // Sorted, because the source is a directory listing and neither engine
    // gets a guaranteed order from one. NTFS returns entries sorted while
    // ext4 and APFS do not, so an unsorted merge agreed on Windows and
    // disagreed on Linux and macOS — a cross-engine divergence that depended
    // on the developer's filesystem (RFC 008 Phase 5).
    extra.sort((a, b) => a.sessionId.localeCompare(b.sessionId));
    return [...indexEntries, ...extra];
  }

  private discoverSessionEntries(projectDir: string, originalPath: string | undefined): SessionIndexEntry[] {
    const UUID_JSONL = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}\.jsonl$/;
    const entries: SessionIndexEntry[] = [];

    let filePaths: string[];
    try {
      filePaths = this.fileService.scanDirectorySync(projectDir, { pattern: '*.jsonl' });
    } catch {
      return entries;
    }

    for (const filePath of filePaths) {
      const fileName = path.basename(filePath);
      if (!UUID_JSONL.test(fileName)) continue;

      const sessionId = fileName.replace('.jsonl', '');
      const stats = this.fileService.getStats(filePath);
      if (!stats) continue;

      // Stream the file and retain only the first genuine human prompt.
      // `readJsonlStreaming` deliberately contains callback errors, so a
      // thrown sentinel cannot be used to stop iteration here.
      let firstPrompt = '';
      try {
        this.fileService.readJsonlStreaming<Record<string, unknown>>(filePath, (msg) => {
          if (firstPrompt) return;
          const candidate = extractClaudeHumanPrompt(msg);
          if (candidate) {
            firstPrompt = candidate;
          }
        });
      } catch {
        // Ignore read errors for discovery.
      }

      const modifiedIso = new Date(stats.mtimeMs).toISOString();
      entries.push({
        sessionId,
        fullPath: filePath,
        fileMtime: stats.mtimeMs,
        firstPrompt: firstPrompt || 'No prompt',
        summary: '',
        messageCount: 0,
        created: modifiedIso,
        modified: modifiedIso,
        gitBranch: '',
        projectPath: originalPath ?? this.slugToPath(path.basename(projectDir)),
        isSidechain: false,
      });
    }

    return entries;
  }

  private parseSessionMessages(projectDir: string, sessionId: string, fullPath?: string): SessionMessage[] {
    try {
      const canonicalPath = path.join(projectDir, `${sessionId}.jsonl`);
      const filePath = this.fileService.exists(canonicalPath)
        ? canonicalPath
        : fullPath && this.fileService.exists(fullPath)
          ? fullPath
          : canonicalPath;
      const result = this.fileService.readJsonlSync<SessionMessage>(filePath);
      return result.entries;
    } catch {
      return [];
    }
  }

  private parseSubagents(projectDir: string, sessionId: string): SubagentTranscript[] {
    const subagentsDir = path.join(projectDir, sessionId, 'subagents');
    const transcripts: SubagentTranscript[] = [];

    // Top-level subagent transcripts (not associated with a workflow).
    try {
      const filePaths = this.fileService.scanDirectorySync(subagentsDir, { pattern: '*.jsonl' });
      for (const filePath of filePaths) {
        const transcript = this.readSubagentTranscript(filePath, '');
        if (transcript) transcripts.push(transcript);
      }
    } catch {
      // subagents dir doesn't exist
    }

    // Nested workflow subagent transcripts:
    //   subagents/workflows/{wf_id}/agent-*.jsonl  (journal.jsonl is skipped
    //   by the `agent-*` glob). Prior to this the parser only walked the
    //   flat subagents/ dir, so every workflow-orchestrated transcript was
    //   invisible to both engines. Grouped to its run via `workflowId`.
    try {
      const workflowsDir = path.join(subagentsDir, 'workflows');
      const wfDirs = this.fileService.scanDirectorySync(workflowsDir, { directoriesOnly: true });
      for (const wfDir of wfDirs) {
        const workflowId = path.basename(wfDir);
        try {
          const agentFiles = this.fileService.scanDirectorySync(wfDir, { pattern: 'agent-*.jsonl' });
          for (const filePath of agentFiles) {
            const transcript = this.readSubagentTranscript(filePath, workflowId);
            if (transcript) transcripts.push(transcript);
          }
        } catch {
          // skip bad workflow dir
        }
      }
    } catch {
      // no subagents/workflows/ subtree
    }

    return transcripts;
  }

  private readSubagentTranscript(filePath: string, workflowId: string): SubagentTranscript | null {
    try {
      const fileName = path.basename(filePath);
      const result = this.fileService.readJsonlSync<SessionMessage>(filePath);
      // Sibling `agent-{id}.meta.json` carries the real (free-form) agent
      // type + description; the filename regex only distinguishes
      // task/prompt_suggestion/compact.
      const meta = this.fileService.readJsonSync<SubagentMeta>(filePath.replace(/\.jsonl$/, '.meta.json'));
      return {
        agentId: this.extractAgentId(fileName),
        agentType: this.inferAgentType(fileName),
        fileName,
        messages: result.entries,
        workflowId,
        ...(meta ? { meta } : {}),
      };
    } catch {
      return null;
    }
  }

  /**
   * Parse the workflow run records under `projects/{slug}/{sid}/workflows/`.
   * Each `wf_*.json` is an agent-orchestration run record; its `journal.jsonl`
   * (started/result events) lives beside the run's nested transcripts under
   * `subagents/workflows/{runId}/`.
   */
  private parseWorkflows(projectDir: string, sessionId: string): WorkflowRun[] {
    const workflowsDir = path.join(projectDir, sessionId, 'workflows');
    const runs: WorkflowRun[] = [];

    try {
      const filePaths = this.fileService.scanDirectorySync(workflowsDir, { pattern: 'wf_*.json' });
      for (const filePath of filePaths) {
        try {
          const data = this.fileService.readJsonSync<Record<string, unknown>>(filePath);
          if (!data) continue;
          const workflowId = (typeof data.runId === 'string' && data.runId) || path.basename(filePath, '.json');
          const num = (v: unknown): number => (typeof v === 'number' ? v : 0);
          runs.push({
            workflowId,
            name: (typeof data.workflowName === 'string' && data.workflowName) || workflowId,
            status: typeof data.status === 'string' ? data.status : '',
            agentCount: num(data.agentCount),
            totalTokens: num(data.totalTokens),
            totalToolCalls: num(data.totalToolCalls),
            durationMs: num(data.durationMs),
            subagentCount: this.countWorkflowSubagents(projectDir, sessionId, workflowId),
            data,
            journal: this.parseWorkflowJournal(projectDir, sessionId, workflowId),
          });
        } catch {
          // skip bad workflow record
        }
      }
    } catch {
      // no workflows/ dir
    }

    return runs.sort((a, b) => a.workflowId.localeCompare(b.workflowId));
  }

  private countWorkflowSubagents(projectDir: string, sessionId: string, workflowId: string): number {
    try {
      const dir = path.join(projectDir, sessionId, 'subagents', 'workflows', workflowId);
      return this.fileService.scanDirectorySync(dir, { pattern: 'agent-*.jsonl' }).length;
    } catch {
      return 0;
    }
  }

  private parseWorkflowJournal(projectDir: string, sessionId: string, workflowId: string): unknown[] {
    try {
      const journalPath = path.join(projectDir, sessionId, 'subagents', 'workflows', workflowId, 'journal.jsonl');
      return this.fileService.readJsonlSync<unknown>(journalPath).entries;
    } catch {
      return [];
    }
  }

  private extractAgentId(fileName: string): string {
    const parsed = parseSubagentFilename(fileName);
    // Cold-start fallback: when the strict `agent-<id>.jsonl` shape
    // doesn't match, drop the extension so bespoke transcript names
    // still produce an agentId.
    return parsed ? parsed.agentId : fileName.replace(/\.jsonl$/, '');
  }

  private inferAgentType(fileName: string): SubagentType {
    return inferSubagentType(fileName);
  }

  private parseToolResults(projectDir: string, sessionId: string): PersistedToolResult[] {
    const resultsDir = path.join(projectDir, sessionId, 'tool-results');
    const results: PersistedToolResult[] = [];

    try {
      const filePaths = this.fileService.scanDirectorySync(resultsDir, { pattern: '*.txt' });

      for (const filePath of filePaths) {
        try {
          const fileName = path.basename(filePath);
          const toolUseId = fileName.replace(/\.txt$/, '');
          const content = this.fileService.readFileSync(filePath);
          results.push({ toolUseId, content });
        } catch {
          // skip bad tool result
        }
      }
    } catch {
      // tool-results dir doesn't exist
    }

    return results;
  }

  private parseProjectMemory(projectSlug: string, projectDir: string): ProjectMemory | null {
    try {
      const content = this.fileService.readFileSync(path.join(projectDir, 'memory', 'MEMORY.md'));
      return { projectSlug, content };
    } catch {
      return null;
    }
  }

  private parseFileHistory(rootDir: string, sessionId: string): FileHistorySession | null {
    const historyDir = path.join(rootDir, 'file-history', sessionId);

    try {
      const filePaths = this.fileService.scanDirectorySync(historyDir);
      if (filePaths.length === 0) return null;

      const snapshots: FileHistorySnapshotFile[] = [];
      for (const filePath of filePaths) {
        try {
          const fileName = path.basename(filePath);
          const parsed = parseFileHistoryFilename(fileName);
          if (!parsed) continue;

          const content = this.fileService.readFileSync(filePath);
          const stats = this.fileService.getStats(filePath);

          snapshots.push({
            hash: parsed.hash,
            version: parsed.version,
            fileName,
            content,
            size: stats?.size ?? 0,
          });
        } catch {
          // skip bad snapshot file
        }
      }

      return snapshots.length > 0 ? { sessionId, snapshots } : null;
    } catch {
      return null;
    }
  }

  private parseTodos(rootDir: string, sessionId: string): TodoFile[] {
    const todosDir = path.join(rootDir, 'todos');
    const todoFiles: TodoFile[] = [];

    try {
      const filePaths = this.fileService.scanDirectorySync(todosDir, {
        pattern: `${sessionId}-agent-*.json`,
      });

      for (const filePath of filePaths) {
        try {
          const fileName = path.basename(filePath);
          const parsed = parseTodoFilename(fileName);
          if (!parsed) continue;

          const items = this.fileService.readJsonSync<TodoItem[]>(filePath) ?? [];

          todoFiles.push({
            sessionId: parsed.sessionId,
            agentId: parsed.agentId,
            items: Array.isArray(items) ? items : [],
          });
        } catch {
          // skip bad todo file
        }
      }
    } catch {
      // todos dir doesn't exist
    }

    return todoFiles;
  }

  private parseTask(rootDir: string, sessionId: string): TaskEntry | null {
    const taskDir = path.join(rootDir, 'tasks', sessionId);

    try {
      const lockExists = this.fileService.exists(path.join(taskDir, '.lock'));
      if (!lockExists) return null;

      let hasHighwatermark = false;
      let highwatermark: number | null = null;

      try {
        const hwContent = this.fileService.readFileSync(path.join(taskDir, '.highwatermark'));
        hasHighwatermark = true;
        highwatermark = parseInt(hwContent.trim(), 10);
        if (isNaN(highwatermark)) highwatermark = null;
      } catch {
        // no highwatermark file
      }

      return { taskId: sessionId, hasHighwatermark, highwatermark, lockExists: true };
    } catch {
      return null;
    }
  }

  private buildPlanIndex(rootDir: string): Map<string, PlanFile> {
    const index = new Map<string, PlanFile>();
    const plansDir = path.join(rootDir, 'plans');

    try {
      const filePaths = this.fileService.scanDirectorySync(plansDir, { pattern: '*.md' });

      for (const filePath of filePaths) {
        try {
          const fileName = path.basename(filePath);
          const parsed = parsePlanFilename(fileName);
          if (!parsed) continue;
          const planSlug = parsed.slug;
          const content = this.fileService.readFileSync(filePath);
          const stats = this.fileService.getStats(filePath);

          const titleMatch = content.match(/^#\s+(.+)$/m);
          const title = titleMatch ? titleMatch[1] : planSlug;

          index.set(planSlug, { slug: planSlug, title, content, size: stats?.size ?? 0 });
        } catch {
          // skip bad plan file
        }
      }
    } catch {
      // plans dir doesn't exist
    }

    return index;
  }

  private extractPlanSlugFromMessages(messages: SessionMessage[], planIndex: Map<string, PlanFile>): string | null {
    for (const msg of messages) {
      const raw = msg as unknown as Record<string, unknown>;
      const slug = raw.slug;
      if (typeof slug === 'string' && planIndex.has(slug)) {
        return slug;
      }
    }
    return null;
  }

  private peekPlanSlug(projectDir: string, sessionId: string, planIndex: Map<string, PlanFile>): string | null {
    if (planIndex.size === 0) return null;

    try {
      const filePath = path.join(projectDir, `${sessionId}.jsonl`);
      const content = this.fileService.readFileSync(filePath);

      const slugPattern = /"slug"\s*:\s*"([^"]+)"/;
      const match = content.match(slugPattern);
      if (match) {
        const candidate = match[1];
        if (planIndex.has(candidate)) return candidate;
      }
    } catch {
      // file doesn't exist
    }

    return null;
  }

  private slugToPath(slug: string): string {
    const { prefix, sep, rest } = slugShape(slug);
    if (rest === '') return prefix;

    const parts = rest.split('-');
    let resolved = '';
    let i = 0;
    while (i < parts.length) {
      let matched = false;
      for (let end = parts.length; end > i; end--) {
        const candidate = parts.slice(i, end).join('-');
        const probe = resolved === '' ? prefix + candidate : prefix + resolved + sep + candidate;
        if (this.fileService.getStats(probe)) {
          resolved = resolved === '' ? candidate : resolved + sep + candidate;
          i = end;
          matched = true;
          break;
        }
      }
      if (!matched) {
        resolved = resolved === '' ? parts[i]! : resolved + sep + parts[i]!;
        i++;
      }
    }

    return prefix + resolved;
  }
}

// ═══════════════════════════════════════════════════════════════════════════════
// FACTORY
// ═══════════════════════════════════════════════════════════════════════════════

export function createProjectParser(fileService: FileService): ProjectParser {
  return new ProjectParserImpl(fileService);
}

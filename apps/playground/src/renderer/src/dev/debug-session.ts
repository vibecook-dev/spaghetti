import type { ProjectListItem, SessionListItem } from '@vibecook/spaghetti-sdk';

type RawMessage = Record<string, unknown>;

const SESSION_ID = 'debug-message-gallery';
const PROJECT_SLUG = '-debug-spaghetti-message-gallery';
const START = Date.parse('2026-06-12T16:00:00.000Z');
let sequence = 0;

function timestamp(seconds: number): string {
  return new Date(START + seconds * 1000).toISOString();
}

function base(type: string, seconds: number, extra: RawMessage = {}): RawMessage {
  sequence += 1;
  return {
    type,
    uuid: `debug-message-${String(sequence).padStart(3, '0')}`,
    parentUuid: sequence === 1 ? null : `debug-message-${String(sequence - 1).padStart(3, '0')}`,
    timestamp: timestamp(seconds),
    sessionId: SESSION_ID,
    cwd: '/debug/spaghetti-message-gallery',
    version: 'debug-fixture',
    gitBranch: 'ui/message-gallery',
    isSidechain: false,
    userType: 'external',
    ...extra,
  };
}

function user(seconds: number, content: string | RawMessage[], extra: RawMessage = {}): RawMessage {
  return base('user', seconds, {
    message: { role: 'user', content },
    ...extra,
  });
}

function assistant(seconds: number, content: RawMessage[], extra: RawMessage = {}): RawMessage {
  return base('assistant', seconds, {
    requestId: `debug-request-${sequence + 1}`,
    message: {
      id: `debug-response-${sequence + 1}`,
      type: 'message',
      role: 'assistant',
      model: 'claude-opus-4-6',
      content,
      stop_reason: content.some((part) => part.type === 'tool_use') ? 'tool_use' : 'end_turn',
      stop_sequence: null,
      usage: {
        input_tokens: 1842,
        output_tokens: 376,
        cache_creation_input_tokens: 128,
        cache_read_input_tokens: 4096,
      },
    },
    ...extra,
  });
}

interface MockToolResult {
  content: unknown;
  isError?: boolean;
  extra?: RawMessage;
}

function toolExchange(
  seconds: number,
  toolName: string,
  input: RawMessage,
  result?: MockToolResult,
  messageExtra: RawMessage = {},
): RawMessage[] {
  const toolId = `debug-tool-${toolName.toLowerCase().replace(/[^a-z0-9]+/g, '-')}-${seconds}`;
  const call = assistant(seconds, [{ type: 'tool_use', id: toolId, name: toolName, input }], messageExtra);
  if (!result) return [call];
  const response = user(
    seconds + 1,
    [
      {
        type: 'tool_result',
        tool_use_id: toolId,
        content: result.content,
        is_error: result.isError === true,
        ...result.extra,
      },
    ],
    messageExtra,
  );
  return [call, response];
}

const previewImage = btoa(`
  <svg xmlns="http://www.w3.org/2000/svg" width="360" height="132" viewBox="0 0 360 132">
    <rect width="360" height="132" fill="#f1ede4"/>
    <rect x="12" y="12" width="336" height="108" fill="none" stroke="#2b2623" stroke-opacity=".35"/>
    <path d="M36 88 C82 30 126 106 174 54 S270 92 326 38" fill="none" stroke="#9a3b28" stroke-width="2"/>
    <text x="30" y="42" fill="#2b2623" font-family="Georgia,serif" font-size="18">Archive preview result</text>
    <text x="30" y="108" fill="#2b2623" fill-opacity=".55" font-family="monospace" font-size="9">MIXED TEXT + IMAGE PAYLOAD</text>
  </svg>
`);

/** A development-only project row that keeps the gallery easy to reopen. */
export const DEBUG_PROJECT: ProjectListItem = {
  projectId: 'debug:message-gallery',
  members: [{ sourceId: 'claude-code', slug: PROJECT_SLUG }],
  slug: PROJECT_SLUG,
  sourceIds: ['claude-code'],
  folderName: '[ debug ] message gallery',
  absolutePath: '',
  sessionCount: 1,
  messageCount: 63,
  tokenUsage: {
    inputTokens: 48_320,
    outputTokens: 12_480,
    cacheCreationTokens: 2_048,
    cacheReadTokens: 96_000,
    totalTokens: 158_848,
  },
  tokensEstimated: false,
  lastActiveAt: timestamp(900),
  firstActiveAt: timestamp(0),
  latestGitBranch: 'ui/message-gallery',
  hasMemory: false,
};

export const DEBUG_SESSION: SessionListItem = {
  sessionId: SESSION_ID,
  sourceId: 'claude-code',
  projectSlug: PROJECT_SLUG,
  startTime: timestamp(0),
  lastUpdate: timestamp(900),
  lifespanMs: 15 * 60 * 1000,
  tokenUsage: DEBUG_PROJECT.tokenUsage,
  tokensEstimated: false,
  messageCount: DEBUG_PROJECT.messageCount,
  fullPath: '/debug/spaghetti-message-gallery/debug-message-gallery.jsonl',
  summary: 'Development fixture containing every transcript row and representative tool payload.',
  firstPrompt: 'Render the complete message gallery for visual QA.',
  gitBranch: 'ui/message-gallery',
  todoCount: 3,
  planSlug: 'message-gallery-plan',
  hasTask: true,
  isSidechain: false,
};

/**
 * Raw records intentionally enter through the same transformer as indexed
 * sessions. Keep examples broad: typography, overflow, errors, branching,
 * results, system events, and every specialized tool input shape.
 */
export const DEBUG_SESSION_MESSAGES: readonly RawMessage[] = [
  user(
    0,
    [
      {
        type: 'text',
        text: [
          '# Message gallery',
          '',
          'Please exercise **every transcript style**: prose, `inline code`, links, lists, quotes, and long wrapping text.',
          '',
          '> This fixture is synthetic and available only in development.',
          '',
          '- Primary message rows',
          '- Tool calls and results',
          '- Branches, errors, and system events',
        ].join('\n'),
      },
    ],
    {
      thinkingMetadata: { level: 'high', disabled: false, triggers: ['complexity', 'visual-qa'] },
      promptSource: 'typed',
    },
  ),
  assistant(4, [
    {
      type: 'thinking',
      thinking:
        'I should establish a clear visual rhythm, verify long lines and compact labels, then enumerate every state without relying on production data.',
    },
    {
      type: 'text',
      text: [
        '## Rich assistant response',
        '',
        'This paragraph includes **bold**, *emphasis*, [a reference link](https://example.com), and `const compact = true`.',
        '',
        '```ts',
        "const palette = { paper: '#f1ede4', ink: '#2b2623' };",
        'export function swatch(name: keyof typeof palette) {',
        '  return palette[name];',
        '}',
        '```',
        '',
        '| State | Expected treatment |',
        '| --- | --- |',
        '| Default | Warm paper |',
        '| Hover | Five-percent ink wash |',
        '| Error | Sanguine rule |',
      ].join('\n'),
    },
  ]),

  ...toolExchange(
    12,
    'Read',
    { file_path: '/src/components/ArchiveTranscript.tsx', offset: 118, limit: 24 },
    {
      content: '118→function MessageBody({ msg, kind }) {\n119→  return <article>{msg.content}</article>;\n120→}',
    },
  ),
  ...toolExchange(
    18,
    'Write',
    {
      file_path: '/tmp/message-gallery.md',
      content: '# Fixture output\n\nA complete file payload with multiple lines.\n\n- alpha\n- beta\n- gamma\n',
    },
    { content: 'Wrote 7 lines to /tmp/message-gallery.md' },
  ),
  ...toolExchange(
    24,
    'Edit',
    {
      file_path: '/src/theme.css',
      old_string: '.message { color: #333;\n  padding: 8px;\n}',
      new_string: '.message { color: var(--archive-ink);\n  padding: 12px 16px;\n}',
      replace_all: false,
    },
    { content: 'Updated /src/theme.css successfully.' },
  ),
  ...toolExchange(
    30,
    'NotebookEdit',
    {
      notebook_path: '/research/message-density.ipynb',
      cell_id: 'cell-07',
      cell_type: 'code',
      edit_mode: 'replace',
      new_source: "samples = ['user', 'assistant', 'tool']\nprint({kind: samples.count(kind) for kind in samples})",
    },
    { content: 'Replaced code cell cell-07.' },
  ),
  ...toolExchange(
    36,
    'Glob',
    { pattern: '**/*.{tsx,css}', path: '/src' },
    { content: '/src/App.tsx\n/src/styles.css\n/src/components/ArchiveTranscript.tsx' },
  ),
  ...toolExchange(
    42,
    'Grep',
    {
      pattern: 'archive-(ink|paper)',
      path: '/src',
      glob: '*.{tsx,css}',
      output_mode: 'content',
      multiline: false,
      head_limit: 20,
    },
    { content: 'styles.css:31: --archive-ink-rgb: 43 38 35;\nApp.tsx:371: className="text-ink"' },
  ),
  ...toolExchange(
    48,
    'Bash',
    {
      command: 'pnpm typecheck && pnpm test',
      description: 'Validate the message gallery',
      timeout: 120000,
      run_in_background: false,
    },
    { content: 'Typecheck passed\n42 tests passed in 3.81s\nProcess exited with code 0' },
  ),
  ...toolExchange(
    54,
    'Bash',
    { command: 'pnpm test --filter missing-fixture', description: 'Demonstrate an error result' },
    {
      content: 'ERR_PNPM_NO_MATCHING_VERSION: Fixture package was not found\nExit code: 1',
      isError: true,
    },
  ),
  ...toolExchange(
    60,
    'TodoWrite',
    {
      todos: [
        { content: 'Inventory message variants', status: 'completed', activeForm: 'Inventorying variants' },
        { content: 'Align transcript spacing', status: 'in_progress', activeForm: 'Aligning spacing' },
        { content: 'Verify dark parchment', status: 'pending', activeForm: 'Verifying dark parchment' },
      ],
    },
    { content: 'Todo list updated.' },
  ),
  ...toolExchange(
    66,
    'AskUserQuestion',
    {
      questions: [
        {
          header: 'Density',
          question: 'Which transcript density feels closest to the archive reference?',
          multiSelect: false,
          options: [
            { label: 'Compact', description: 'More rows remain visible at once.' },
            { label: 'Comfortable', description: 'Balances rhythm and scanning.' },
            { label: 'Spacious', description: 'Gives long prose more air.' },
          ],
        },
        {
          header: 'Details',
          question: 'Which optional metadata should stay visible?',
          multiSelect: true,
          options: [
            { label: 'Timestamps', description: 'Show seconds on each row.' },
            { label: 'Token usage', description: 'Show model usage metadata.' },
          ],
        },
      ],
    },
    { content: 'User selected “Comfortable” and “Timestamps”.' },
  ),
  ...toolExchange(72, 'EnterPlanMode', {}, { content: 'Entered plan mode.' }),
  ...toolExchange(
    78,
    'ExitPlanMode',
    {
      plan: [
        '# Transcript refinement plan',
        '',
        '1. Normalize labels and timestamps.',
        '2. Tune prose measure and tool-card density.',
        '3. Compare light and dark archive palettes.',
      ].join('\n'),
    },
    { content: 'Plan approved.' },
  ),
  ...toolExchange(
    84,
    'Skill',
    { skill: 'browser-use', args: '--headed --session message-gallery' },
    { content: 'Loaded browser-use instructions.' },
  ),
  ...toolExchange(
    90,
    'WebSearch',
    {
      query: 'archival editorial interface typography',
      allowed_domains: ['fonts.google.com', 'developer.mozilla.org'],
      blocked_domains: ['pinterest.com'],
    },
    {
      content:
        'Search results for query “archival editorial interface typography”\n1. EB Garamond specimen\n2. CSS typography guide',
    },
  ),
  ...toolExchange(
    96,
    'WebFetch',
    {
      url: 'https://example.com/design-system/archive-notes',
      prompt: 'Extract recommendations about paper colors, hairlines, and mono labels.',
    },
    { content: 'Use warm off-white surfaces, low-alpha ink rules, and compact uppercase metadata.' },
  ),
  ...toolExchange(
    102,
    'TaskOutput',
    { task_id: 'task-message-audit-7f23', block: true, timeout: 30000 },
    { content: 'Task completed\nReviewed 58 fixture rows\nNo missing renderer types found.' },
  ),
  ...toolExchange(
    108,
    'KillShell',
    { shell_id: 'shell-preview-42' },
    { content: 'Shell shell-preview-42 terminated.' },
  ),
  ...toolExchange(
    114,
    'mcp__design_archive__inspect_component',
    {
      component: 'TranscriptRow',
      states: ['default', 'hover', 'collapsed', 'error'],
      includeComputedStyles: true,
    },
    {
      content: [
        { type: 'text', text: 'Inspection complete. The image item verifies mixed rich tool results.' },
        { type: 'image', source: { type: 'base64', media_type: 'image/svg+xml', data: previewImage } },
      ],
    },
  ),

  ...toolExchange(
    122,
    'Task',
    {
      description: 'Audit transcript branch styling',
      prompt: 'Inspect branch rails, nested tool cards, and the return curve to the primary timeline.',
      subagent_type: 'Explore',
      model: 'sonnet',
      run_in_background: true,
    },
    { content: 'Agent debug-subagent-a17 spawned.', extra: { agentId: 'debug-subagent-a17' } },
  ),
  user(126, 'Inspect the branch independently and report visual mismatches.', {
    isSidechain: true,
    agentId: 'debug-subagent-a17',
  }),
  assistant(
    130,
    [
      { type: 'thinking', thinking: 'The dashed branch rail needs enough contrast without competing with prose.' },
      {
        type: 'text',
        text: 'Branch audit: the fork, nested indentation, and return curve are all represented by this sidechain.',
      },
    ],
    { isSidechain: true, agentId: 'debug-subagent-a17' },
  ),
  ...toolExchange(
    136,
    'Bash',
    { command: 'rg -n "branch_start|isSidechain" src', description: 'Inspect branch implementation' },
    {
      content:
        'ArchiveTranscript.tsx:20:type RailKind = ... branch_start\nArchiveTranscript.tsx:57:return msg.isSidechain ? 1 : 0',
    },
    { isSidechain: true, agentId: 'debug-subagent-a17' },
  ),
  user(142, 'Thanks. Return to the primary thread and continue the gallery.'),

  base('tool_result', 148, {
    toolResult: {
      toolId: 'debug-orphan-result',
      isError: false,
      content: 'A standalone result row exercises the Result filter and fallback presentation.',
      rawJson: { source: 'debug', orphaned: true },
    },
  }),
  ...toolExchange(154, 'UnknownCustomTool', {
    deeply_nested: {
      values: [1, true, null, { label: 'A deliberately long generic payload that wraps across the tool card.' }],
    },
    unicode: '紙 / ink / Δ / ✦',
  }),

  user(160, 'Earlier context was compacted to keep the session within its context window.', {
    isCompactSummary: true,
    isVisibleInTranscriptOnly: true,
  }),
  base('file-history-snapshot', 166, {
    messageId: 'checkpoint-debug-001',
    isSnapshotUpdate: false,
    snapshot: {
      timestamp: timestamp(166),
      trackedFileBackups: {
        '/src/App.tsx': { backupFileName: 'App.tsx.1', version: 1, backupTime: timestamp(166) },
        '/src/styles.css': { backupFileName: 'styles.css.1', version: 1, backupTime: timestamp(166) },
        '/src/ArchiveTranscript.tsx': {
          backupFileName: 'ArchiveTranscript.tsx.1',
          version: 1,
          backupTime: timestamp(166),
        },
      },
    },
  }),
  base('file-history-snapshot', 172, {
    messageId: 'checkpoint-debug-001',
    isSnapshotUpdate: true,
    snapshot: {
      timestamp: timestamp(172),
      trackedFileBackups: {
        '/src/App.tsx': { backupFileName: 'App.tsx.2', version: 2, backupTime: timestamp(172) },
      },
    },
  }),
  base('system', 178, {
    subtype: 'local_command',
    content: '<command-name>/compact</command-name>\n<command-message>Compact the conversation</command-message>',
  }),
  base('system', 184, {
    subtype: 'compact_boundary',
    content: 'Conversation context compacted successfully.',
    compactMetadata: { trigger: 'manual', preTokens: 142_380 },
  }),
  base('system', 190, {
    subtype: 'microcompact_boundary',
    content: 'Old tool results were compressed, saving 18,240 tokens.',
    microcompactMetadata: {
      trigger: 'auto',
      preTokens: 96_000,
      tokensSaved: 18_240,
      compactedToolIds: ['debug-tool-read-12', 'debug-tool-grep-42'],
    },
  }),
  base('system', 196, {
    subtype: 'stop_hook_summary',
    hookCount: 2,
    hookInfos: [{ command: 'pnpm lint' }, { command: 'pnpm typecheck' }],
    hookErrors: [],
    preventedContinuation: false,
    stopReason: 'Hooks completed',
    hasOutput: true,
    toolUseID: 'debug-hook-tool',
  }),
  base('system', 202, {
    subtype: 'turn_duration',
    durationMs: 48_320,
    messageCount: 42,
  }),
  base('system', 208, {
    subtype: 'api_error',
    level: 'error',
    content: 'The provider returned a transient 529 response. Retrying in 1.5 seconds.',
    cause: { status: 529, code: 'overloaded_error' },
    error: { cause: { message: 'Overloaded' } },
    retryInMs: 1500,
    retryAttempt: 2,
    maxRetries: 5,
  }),
  base('system', 214, {
    subtype: 'bridge_status',
    url: 'https://bridge.example.test/debug-session',
    content: 'Remote bridge connected for transcript preview.',
  }),
  base('system', 220, {
    subtype: 'away_summary',
    content: 'While you were away, the audit completed and three visual inconsistencies were catalogued.',
  }),
  base('system', 226, {
    subtype: 'informational',
    level: 'suggestion',
    content: 'Development fixture reached the system-event section.',
  }),
  base('summary', 232, {
    summary:
      'The gallery exercised rich prose, successful and failed tools, a nested agent branch, compaction, checkpoints, and system metadata.',
    leafUuid: 'debug-message-leaf',
  }),
  base('queue-operation', 238, {
    operation: 'enqueue',
    content: '<summary>Run screenshot comparison</summary><status>pending</status>',
  }),
  base('queue-operation', 244, {
    operation: 'enqueue',
    content: '<summary>Review dark mode transcript</summary><status>completed</status>',
  }),
  assistant(250, [
    {
      type: 'text',
      text: 'The development message gallery is complete. Use the filter tray to isolate any row type or tool while refining the interface.',
    },
  ]),
];

export const DEBUG_PROJECT_KEY = {
  projectId: DEBUG_PROJECT.projectId,
} as const;

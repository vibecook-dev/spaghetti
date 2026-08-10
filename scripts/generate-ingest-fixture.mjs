#!/usr/bin/env node
/**
 * generate-ingest-fixture.mjs
 *
 * Deterministically generate a fake ~/.claude directory tree for ingest
 * correctness tests (RFC 003 commit 1.8 "correctness gate").
 *
 * Output layout (rooted at --out):
 *   <out>/.claude/projects/<slug-N>/
 *     sessions-index.json                — real index with one entry per JSONL
 *     <session-uuid>.jsonl               — mix of user/assistant/summary/thinking
 *     memory/MEMORY.md                   — project 1 only
 *     <session-uuid>/subagents/agent-a<id>.jsonl — project 2 only
 *     <session-uuid>/tool-results/<tool_use_id>.txt — project 2 only
 *   <out>/.claude/todos/<session>-agent-<agent>.json
 *   <out>/.claude/tasks/<session>/.lock + .highwatermark
 *   <out>/.claude/file-history/<session>/<hash>@v<version>
 *
 * Determinism:
 *   - A seeded LCG (seed default 42) drives every choice.
 *   - UUIDs are derived from `sha256("<seed>:<counter>")` sliced to fit
 *     the RFC 4122 grid — bit-identical output across runs.
 *   - File mtimes are SET to a fixed epoch so both TS and Rust see a
 *     stable `file_mtime` field. (The DB row's `updated_at` is still
 *     `Date.now()` in both paths — ignored by the diff harness.)
 *
 * Notable omissions — matched to what the Rust ingest (commit 1.7) can
 * actually write:
 *   - No plans/ directory. `project_parser.rs` does not emit Plan events
 *     yet, so including plan files in the fixture would just produce a
 *     TS-only diff.
 *   - No `source_files` fingerprint table writes on the Rust side either,
 *     so the diff harness ignores that whole table — see ingest-diff.ts.
 *
 * Usage:
 *   node scripts/generate-ingest-fixture.mjs --out <path> [--seed 42]
 */
import { createHash } from 'node:crypto';
import { mkdirSync, writeFileSync, utimesSync } from 'node:fs';
import * as path from 'node:path';
import { parseArgs } from 'node:util';

// ─── CLI ────────────────────────────────────────────────────────────────────

const { values } = parseArgs({
  options: {
    out: { type: 'string' },
    seed: { type: 'string', default: '42' },
  },
});

if (!values.out) {
  console.error('Usage: generate-ingest-fixture.mjs --out <path> [--seed 42]');
  process.exit(2);
}

const OUT = path.resolve(values.out);
const SEED = Number.parseInt(values.seed, 10);
if (!Number.isFinite(SEED)) {
  console.error(`bad --seed value: ${values.seed}`);
  process.exit(2);
}

// Pin every written file's mtime to a fixed epoch so regeneration is
// bit-identical even at the filesystem level.
const FIXED_MTIME = new Date('2026-04-01T00:00:00Z');

// ─── Seeded RNG + deterministic ID generator ────────────────────────────────

/**
 * Tiny LCG — glibc constants. Not cryptographically interesting; it just
 * needs to produce a reproducible stream for content choices.
 */
function mkRng(seed) {
  let state = seed >>> 0;
  return () => {
    state = (Math.imul(state, 1103515245) + 12345) >>> 0;
    return state / 0x1_0000_0000;
  };
}

const rng = mkRng(SEED);

let counter = 0;
function nextHex(nBytes) {
  counter++;
  return createHash('sha256').update(`${SEED}:${counter}`).digest('hex').slice(0, nBytes * 2);
}

/** Build a v4-shape UUID from the SHA256 stream. */
function nextUuid() {
  const h = nextHex(16);
  return `${h.slice(0, 8)}-${h.slice(8, 12)}-${h.slice(12, 16)}-${h.slice(16, 20)}-${h.slice(20, 32)}`;
}

/** Pick an int in [lo, hi] inclusive, driven by rng. */
function pickInt(lo, hi) {
  return lo + Math.floor(rng() * (hi - lo + 1));
}

/** Pick one element from an array. */
function pickOne(arr) {
  return arr[Math.floor(rng() * arr.length)];
}

// ─── IO helpers ────────────────────────────────────────────────────────────

function write(filePath, body) {
  mkdirSync(path.dirname(filePath), { recursive: true });
  writeFileSync(filePath, body);
  utimesSync(filePath, FIXED_MTIME, FIXED_MTIME);
}

// ─── Message builders — shapes match packages/sdk/src/types/projects.ts ────

const USER_CORPUS = [
  'How do I sort an array in place?',
  'Can you explain generics with an example?',
  'Refactor this function to be more readable.',
  'What does the error "cannot borrow as mutable" mean?',
  'Write a unit test for the above.',
  'Add error handling to the parser.',
  'Why is this loop slow on large inputs?',
  'Convert this callback-based API to async/await.',
];

const ASSISTANT_TEXT_CORPUS = [
  // Assistant text blocks are realistically longer than user prompts. The
  // lengths here (150–400 chars) are picked so the fixture weighs ~1–2 MB
  // at 3 projects × 8 sessions × 50 messages.
  'Here is one approach that keeps the logic straightforward. We iterate once, accumulate into a map keyed by the session id, and emit the flattened result at the end. This avoids the double scan and keeps allocations bounded.',
  'That error usually means another reference is still alive at the point you are trying to mutate. The borrow checker is telling you there is a read in scope. Try scoping the read to a smaller block, or reach for a RefCell / Arc<Mutex> if interior mutability is genuinely required.',
  'I will start by reading the surrounding code to understand the shape. Once I have a handle on the data flow I will draft a plan, run the tests to establish a baseline, and then make the change in small steps with a clean commit boundary per step.',
  'Let me run the tests to confirm the change is safe. Running the full suite takes ~30s locally. If anything breaks I will revert and investigate before re-attempting. I also want to run the correctness diff harness to catch any subtle regressions that the unit tests miss.',
  'This is a classic O(n^2) hot loop; we can do better. Building a HashMap of keys once and indexing into it per iteration drops the complexity to O(n) and makes the code read as what it actually does.',
  'I updated the handler to propagate errors explicitly rather than silently swallowing them. The change touches three call sites but only one of them needed real thought; the other two are mechanical.',
  'The fix is to hoist the expensive work out of the loop and cache the result. I verified the invariant holds by adding a debug assertion, then confirmed the benchmark moved from 400ms to 12ms on the same input.',
  'Done — I also added two additional assertions to lock the invariants we uncovered during the debugging session. If anyone changes this code in the future the tests will tell them exactly which assumption they broke.',
];

const TOOL_NAMES = ['Read', 'Write', 'Edit', 'Bash', 'Grep', 'Glob'];

const SUMMARY_CORPUS = [
  'Investigated failing test, fixed type mismatch in reducer.',
  'Added support for multi-line input in the CLI parser.',
  'Refactored the worker pool for easier teardown.',
];

function userMessage(sessionId, content) {
  return {
    type: 'user',
    uuid: nextUuid(),
    parentUuid: null,
    timestamp: '2026-04-01T00:00:00.000Z',
    sessionId,
    cwd: '/home/u/proj',
    version: '1.0.0',
    gitBranch: 'main',
    isSidechain: false,
    userType: 'external',
    message: { role: 'user', content },
  };
}

function assistantMessage(sessionId, blocks) {
  return {
    type: 'assistant',
    uuid: nextUuid(),
    parentUuid: null,
    timestamp: '2026-04-01T00:00:00.000Z',
    sessionId,
    cwd: '/home/u/proj',
    version: '1.0.0',
    gitBranch: 'main',
    isSidechain: false,
    userType: 'external',
    requestId: `req_${nextHex(6)}`,
    message: {
      model: 'claude-test',
      id: `msg_${nextHex(8)}`,
      type: 'message',
      role: 'assistant',
      content: blocks,
      stop_reason: 'end_turn',
      stop_sequence: null,
      usage: {
        input_tokens: pickInt(40, 4000),
        output_tokens: pickInt(20, 1000),
        cache_creation_input_tokens: pickInt(0, 200),
        cache_read_input_tokens: pickInt(0, 500),
      },
    },
  };
}

function summaryMessage() {
  return { type: 'summary', summary: pickOne(SUMMARY_CORPUS), leafUuid: nextUuid() };
}

function toolResultBlock(toolUseId) {
  return {
    type: 'tool_result',
    tool_use_id: toolUseId,
    content: 'stdout: ok\nstderr:\n(exit 0)',
  };
}

function thinkingBlock() {
  return { type: 'thinking', thinking: 'Considering the refactor shape...' };
}

/**
 * Build one session's JSONL lines. Interleaves user, assistant (text &
 * tool_use), thinking, summary, and tool_result messages — exercising
 * every fts_text branch plus token-usage extraction.
 *
 * Returns { lines: SessionMessage[], toolUseIds: string[] } so the caller
 * can drop matching .txt files into tool-results/.
 */
function buildSessionLines(sessionId, messageCount) {
  const lines = [];
  const toolUseIds = [];

  // Always lead with a user prompt so `first_prompt` (if discovered) is
  // well-defined. The generator writes a real sessions-index.json anyway.
  const firstContent = pickOne(USER_CORPUS);
  lines.push(userMessage(sessionId, firstContent));

  for (let i = 1; i < messageCount; i++) {
    const dice = rng();

    if (dice < 0.5) {
      // Assistant turn. 50/50 text-only vs text+tool_use.
      const blocks = [{ type: 'text', text: pickOne(ASSISTANT_TEXT_CORPUS) }];
      if (rng() < 0.5) {
        const toolUseId = `toolu_${nextHex(8)}`;
        toolUseIds.push(toolUseId);
        blocks.push({
          type: 'tool_use',
          id: toolUseId,
          name: pickOne(TOOL_NAMES),
          input: { path: '/src/index.ts' },
        });
      }
      if (rng() < 0.15) blocks.unshift(thinkingBlock());
      lines.push(assistantMessage(sessionId, blocks));
    } else if (dice < 0.85) {
      // Next user turn. Half the time it's a tool_result (answers the
      // previous tool_use), otherwise a plain prompt.
      if (toolUseIds.length > 0 && rng() < 0.5) {
        const tuid = toolUseIds[toolUseIds.length - 1];
        lines.push(userMessage(sessionId, [toolResultBlock(tuid)]));
      } else {
        lines.push(userMessage(sessionId, pickOne(USER_CORPUS)));
      }
    } else {
      lines.push(summaryMessage());
    }
  }

  return { lines, toolUseIds };
}

/**
 * The `tool_use_id` of the call that spawns the fixture's subagent.
 *
 * Fixed rather than generated so a test can assert the exact linkage rather
 * than merely that *something* was linked.
 */
const SPAWN_TOOL_USE_ID = 'toolu_fixture_spawn_0001';

// ─── sessions-index.json ───────────────────────────────────────────────────

function buildSessionEntry(sessionId, fullPath, firstPrompt, messageCount, projectOriginalPath) {
  return {
    sessionId,
    fullPath,
    fileMtime: FIXED_MTIME.getTime(),
    firstPrompt,
    summary: '',
    messageCount,
    created: '2026-04-01T00:00:00.000Z',
    modified: '2026-04-01T00:00:00.000Z',
    gitBranch: 'main',
    projectPath: projectOriginalPath,
    isSidechain: false,
  };
}

// ─── Main generation ───────────────────────────────────────────────────────

const CLAUDE_DIR = path.join(OUT, '.claude');

function generateProject(projectIdx, { sessionRange, includeMemory, includeSubagent, includeToolResults }) {
  const slug = `-Users-test-project${projectIdx + 1}`;
  const originalPath = `/Users/test/project${projectIdx + 1}`;
  const projectDir = path.join(CLAUDE_DIR, 'projects', slug);

  const sessionCount = pickInt(sessionRange[0], sessionRange[1]);
  const entries = [];
  const sessionIds = [];

  for (let s = 0; s < sessionCount; s++) {
    const sessionId = nextUuid();
    sessionIds.push(sessionId);
    const messageCount = pickInt(8, 18);
    const { lines, toolUseIds } = buildSessionLines(sessionId, messageCount);

    // Link the subagent to a spawning tool call.
    //
    // Claude records a subagent's result as a `tool_result` block in the
    // parent session whose content mentions the agent id; that block's
    // `tool_use_id` is the call that spawned it. Without this pair the
    // subagent is legitimately `unlinked` in both engines — which is why the
    // fixture agreed while a real corpus disagreed on 113 rows, and the Rust
    // writer's hardcoded `unlinked` went unnoticed for five phases
    // (RFC 008 Phase 5).
    if (includeSubagent && s === 0) {
      lines.push({
        parentUuid: null,
        isSidechain: false,
        userType: 'external',
        cwd: originalPath,
        sessionId,
        version: '1.0.0',
        gitBranch: 'main',
        type: 'user',
        // Fixed, not `nextUuid()`. This block is emitted conditionally, so
        // drawing from the deterministic id stream would shift every id after
        // it and make the fixture depend on which projects opted in.
        uuid: 'fixture-spawn-result-0001',
        timestamp: '2026-04-01T00:00:30.000Z',
        message: {
          role: 'user',
          content: [
            {
              type: 'tool_result',
              tool_use_id: SPAWN_TOOL_USE_ID,
              content: `Agent abc123 finished: found three TODO comments.`,
            },
          ],
        },
      });
    }

    // Write the JSONL
    const jsonlPath = path.join(projectDir, `${sessionId}.jsonl`);
    const body = lines.map((l) => JSON.stringify(l)).join('\n') + '\n';
    write(jsonlPath, body);

    // Grab the first user message's content for the index entry's
    // firstPrompt. Must match what the parsers would see.
    const firstLine = lines[0];
    let firstPrompt = '';
    if (firstLine.type === 'user' && typeof firstLine.message.content === 'string') {
      firstPrompt = firstLine.message.content.slice(0, 200);
    }

    entries.push(buildSessionEntry(sessionId, jsonlPath, firstPrompt, messageCount, originalPath));

    // Optionally drop a subagent transcript for session 0 of the chosen
    // project. Uses the id-prefix-"a" convention the Rust regex requires.
    if (includeSubagent && s === 0) {
      const subagentSessionId = nextUuid();
      const subagentLines = [
        userMessage(sessionId, 'Sub-task: find all TODO comments.'),
        assistantMessage(sessionId, [{ type: 'text', text: 'I found three TODO comments in src/.' }]),
      ];
      void subagentSessionId;
      const transcriptPath = path.join(projectDir, sessionId, 'subagents', 'agent-abc123.jsonl');
      write(transcriptPath, subagentLines.map((l) => JSON.stringify(l)).join('\n') + '\n');
    }

    // Tool-result .txt files matching the tool_use_ids we emitted.
    if (includeToolResults && toolUseIds.length > 0) {
      for (const tuid of toolUseIds.slice(0, 3)) {
        const resultPath = path.join(projectDir, sessionId, 'tool-results', `${tuid}.txt`);
        write(resultPath, `Result for ${tuid}:\nlines of output\n`);
      }
    }
  }

  // sessions-index.json
  const indexPath = path.join(projectDir, 'sessions-index.json');
  write(indexPath, JSON.stringify({ version: 1, originalPath, entries }, null, 2));

  // Optional MEMORY.md
  if (includeMemory) {
    write(path.join(projectDir, 'memory', 'MEMORY.md'), `# Memory for project ${projectIdx + 1}\n\nNotes:\n- build with vite\n- runtime: node 24\n`);
  }

  return { slug, sessionIds };
}

// ─── Go ────────────────────────────────────────────────────────────────────

const projectResults = [];
projectResults.push(
  generateProject(0, {
    sessionRange: [3, 5],
    includeMemory: true,
    includeSubagent: false,
    includeToolResults: false,
  }),
);
projectResults.push(
  generateProject(1, {
    sessionRange: [3, 5],
    includeMemory: false,
    includeSubagent: true,
    includeToolResults: true,
  }),
);
projectResults.push(
  generateProject(2, {
    sessionRange: [2, 4],
    includeMemory: false,
    includeSubagent: false,
    includeToolResults: false,
  }),
);

// Pull representative session IDs for the claude-level artifacts so
// todos/tasks/file-history actually associate with real sessions.
const todoSession = projectResults[0].sessionIds[0];
const taskSession = projectResults[1].sessionIds[0];
const historySession = projectResults[2].sessionIds[0];

// Todos: <rootDir>/todos/<session>-agent-<agent>.json
write(
  path.join(CLAUDE_DIR, 'todos', `${todoSession}-agent-agent_main.json`),
  JSON.stringify(
    [
      { content: 'Write the parser', status: 'completed' },
      { content: 'Port the writer', status: 'in_progress', activeForm: 'Porting' },
      { content: 'Add the diff harness', status: 'pending' },
    ],
    null,
    2,
  ),
);

// Tasks: <rootDir>/tasks/<session>/.lock + .highwatermark
write(path.join(CLAUDE_DIR, 'tasks', taskSession, '.lock'), '');
write(path.join(CLAUDE_DIR, 'tasks', taskSession, '.highwatermark'), '42\n');

// File history: <rootDir>/file-history/<session>/<hash>@v<version>
// We deliberately only emit one snapshot — the ingest code stores snapshots
// as a JSON array whose order comes straight from `readdir`, and readdir's
// order is not portable (macOS APFS and Rust's std::fs::read_dir disagree
// in the general case). A single snapshot avoids a spurious order-only diff.
write(path.join(CLAUDE_DIR, 'file-history', historySession, 'abc123@v1'), 'first snapshot contents\n');

console.log(`Wrote deterministic fixture to: ${CLAUDE_DIR}`);
console.log(`Projects: ${projectResults.length}`);
console.log(`Total sessions: ${projectResults.reduce((n, p) => n + p.sessionIds.length, 0)}`);

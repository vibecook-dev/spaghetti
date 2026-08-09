#!/usr/bin/env node
/**
 * generate-codex-fixture.mjs
 *
 * Deterministically generate a fake ~/.codex tree for the Codex native
 * cold-ingest correctness gate (RS ↔ TS ingest-diff) and for RFC 008's token
 * attribution work.
 *
 * Output layout (rooted at --out):
 *   <out>/.codex/sessions/YYYY/MM/DD/rollout-<ts>-<uuid>.jsonl
 *
 * ── Why synthesized rather than captured ────────────────────────────────────
 *
 * RFC 008 Phase 0 needs six token-attribution shapes. A survey of 18 real
 * sessions found only two of them (17× last+total, 1× no token_count), so four
 * would have to be authored regardless — and real sessions carry real prompts.
 * Synthesizing all six costs no extra coverage, needs no scrubbing, and gives
 * each behavior its own file so the fixture README can name it.
 *
 * The envelope is modelled on real output: every line is
 * `{timestamp, type, payload}` with `type` in
 * session_meta | response_item | event_msg | turn_context.
 *
 * ── The six shapes ─────────────────────────────────────────────────────────
 *
 *   01-official-per-turn   every assistant turn has last_token_usage
 *   02-no-token-count      no token_count events at all
 *   03-total-only          only cumulative total_token_usage, never per-turn
 *   04-mixed-coverage      official first turn, un-attributed second turn
 *   05-empty-internal      session_meta plus internal-only records, no turns
 *   06-live-growth         a tail that a warm run would append to
 *
 * `03` also exercises `info: null`, which real Codex emits (a token_count that
 * carries only rate limits). The Rust reader treats it as "no usage" — worth
 * pinning, since it is easy to mistake for an absent event.
 *
 * Usage:
 *   node scripts/generate-codex-fixture.mjs --out crates/spaghetti-napi/fixtures/small-codex
 */
import { mkdirSync, writeFileSync, utimesSync } from 'node:fs';
import * as path from 'node:path';
import { parseArgs } from 'node:util';

const { values } = parseArgs({ options: { out: { type: 'string' } } });

if (!values.out) {
  console.error('Usage: generate-codex-fixture.mjs --out <path>');
  process.exit(2);
}

const OUT = path.resolve(values.out);
const FIXED_MTIME = new Date('2026-04-01T00:00:00Z');
const CWD = '/home/fixture/project';

function writePinned(filePath, lines) {
  mkdirSync(path.dirname(filePath), { recursive: true });
  writeFileSync(filePath, lines.map((l) => JSON.stringify(l)).join('\n') + '\n');
  utimesSync(filePath, FIXED_MTIME, FIXED_MTIME);
}

/** Codex stamps every line with an ISO timestamp; keep them ordered and fixed. */
let clock = Date.parse('2026-04-01T10:00:00.000Z');
function ts() {
  clock += 1000;
  return new Date(clock).toISOString();
}

// ─── Line builders ─────────────────────────────────────────────────────────

function sessionMeta(id) {
  return {
    timestamp: ts(),
    type: 'session_meta',
    payload: {
      id,
      timestamp: new Date(clock).toISOString(),
      cwd: CWD,
      originator: 'codex_cli_rs',
      cli_version: '0.98.0',
      source: 'cli',
      model_provider: 'openai',
    },
  };
}

function turnContext(model = 'gpt-5.3-codex') {
  return {
    timestamp: ts(),
    type: 'turn_context',
    payload: {
      cwd: CWD,
      approval_policy: 'on-request',
      sandbox_policy: { type: 'read-only' },
      model,
      personality: 'pragmatic',
    },
  };
}

/** A developer/system preamble — internal, never a user-visible turn. */
function developerPreamble(text) {
  return {
    timestamp: ts(),
    type: 'response_item',
    payload: { type: 'message', role: 'developer', content: [{ type: 'input_text', text }] },
  };
}

/**
 * A user turn.
 *
 * This must be a `response_item`, not the `event_msg/user_message` that real
 * Codex *also* emits. Both appear in a real rollout — 18 real sessions carried
 * 747 `response_item` messages against 570 `event_msg` projections — but the
 * extractor deliberately keeps only the canonical `response_item` and skips
 * the UI projection, or every turn would be indexed twice.
 *
 * The first version of this generator emitted only the `event_msg` form, so
 * no fixture produced a user or assistant row at all and there was nothing for
 * `last_token_usage` to attribute onto. The token-attribution fixtures tested
 * nothing (RFC 008 Phase 3A).
 */
function userMessage(text) {
  return {
    timestamp: ts(),
    type: 'response_item',
    payload: { type: 'message', role: 'user', content: [{ type: 'input_text', text }] },
  };
}

/** An assistant turn — the row `last_token_usage` is attributed onto. */
function assistantMessage(text) {
  return {
    timestamp: ts(),
    type: 'response_item',
    payload: {
      type: 'message',
      role: 'assistant',
      content: [{ type: 'output_text', text }],
      phase: 'commentary',
    },
  };
}

/**
 * The UI projection of a turn. Real Codex emits these alongside the canonical
 * record; the extractor skips them. Kept so at least one fixture proves the
 * skip still holds and turns are not double-counted.
 */
function agentMessageEvent(text) {
  return { timestamp: ts(), type: 'event_msg', payload: { type: 'agent_message', message: text } };
}

const RATE_LIMITS = {
  primary: { used_percent: 1.5, window_minutes: 300, resets_at: 1770371653 },
  secondary: { used_percent: 0.5, window_minutes: 10080, resets_at: 1770958453 },
};

function usage(input, output, cached = 0, reasoning = 0) {
  return {
    input_tokens: input,
    cached_input_tokens: cached,
    output_tokens: output,
    reasoning_output_tokens: reasoning,
    total_tokens: input + output + reasoning,
  };
}

/** token_count carrying per-turn usage, and optionally the running total. */
function tokenCountLast(last, total) {
  const info = { last_token_usage: last };
  if (total) info.total_token_usage = total;
  return { timestamp: ts(), type: 'event_msg', payload: { type: 'token_count', info, rate_limits: RATE_LIMITS } };
}

/** token_count carrying only a cumulative total — no per-turn attribution. */
function tokenCountTotalOnly(total) {
  return {
    timestamp: ts(),
    type: 'event_msg',
    payload: { type: 'token_count', info: { total_token_usage: total }, rate_limits: RATE_LIMITS },
  };
}

/** Real Codex emits this: a token_count with no usage at all, only limits. */
function tokenCountInfoNull() {
  return {
    timestamp: ts(),
    type: 'event_msg',
    payload: { type: 'token_count', info: null, rate_limits: RATE_LIMITS },
  };
}

function functionCall(callId, name, args) {
  return {
    timestamp: ts(),
    type: 'response_item',
    payload: { type: 'function_call', name, arguments: JSON.stringify(args), call_id: callId },
  };
}

function functionOutput(callId, output) {
  return {
    timestamp: ts(),
    type: 'response_item',
    payload: { type: 'function_call_output', call_id: callId, output },
  };
}

// ─── The six sessions ──────────────────────────────────────────────────────

/** 01 — every assistant turn carries last_token_usage. The happy path. */
function officialPerTurn(id) {
  return [
    sessionMeta(id),
    developerPreamble('You are a coding agent operating in a fixture repository.'),
    turnContext(),
    userMessage('Rename the parser module.'),
    assistantMessage('Renamed it and updated the two imports.'),
    // The UI projection of the same turn. Must not become a second row.
    agentMessageEvent('Renamed it and updated the two imports.'),
    tokenCountLast(usage(120, 40), usage(120, 40)),
    userMessage('Now add a test.'),
    assistantMessage('Added one covering the rename.'),
    tokenCountLast(usage(90, 55), usage(210, 95)),
  ];
}

/** 02 — no token_count anywhere; every turn is un-attributed. */
function noTokenCount(id) {
  return [
    sessionMeta(id),
    turnContext(),
    userMessage('What does this function return on an empty slice?'),
    assistantMessage('None — the iterator terminates before the first yield.'),
    userMessage('Thanks.'),
    assistantMessage('Any time.'),
  ];
}

/**
 * 03 — cumulative totals only, plus an info:null event. Nothing can be
 * attributed to a specific turn, which is the case that separates a
 * turn-aware estimator from a session-level one.
 */
function totalOnly(id) {
  return [
    sessionMeta(id),
    turnContext(),
    userMessage('Summarize the changelog.'),
    tokenCountInfoNull(),
    assistantMessage('Three fixes and one breaking change.'),
    tokenCountTotalOnly(usage(300, 120)),
    userMessage('Which one is breaking?'),
    assistantMessage('The source-path removal.'),
    tokenCountTotalOnly(usage(540, 210)),
  ];
}

/**
 * 04 — official first turn, un-attributed second. The trap: the official
 * input on turn 1 may already cover the preceding user record, so a naive
 * estimator double-counts it.
 */
function mixedCoverage(id) {
  return [
    sessionMeta(id),
    developerPreamble('Fixture preamble.'),
    turnContext(),
    userMessage('Explain the warm-start fast path.'),
    assistantMessage('It compares fingerprints before doing any parse work.'),
    tokenCountLast(usage(200, 75), usage(200, 75)),
    userMessage('And when a fingerprint is missing?'),
    assistantMessage('The file is treated as new and fully re-read.'),
  ];
}

/** 05 — internal records only. No user turn, no assistant turn, no tokens. */
function emptyInternal(id) {
  return [
    sessionMeta(id),
    developerPreamble('You are a coding agent operating in a fixture repository.'),
    turnContext(),
    { timestamp: ts(), type: 'event_msg', payload: { type: 'turn_aborted', reason: 'interrupted' } },
  ];
}

/**
 * 06 — a session that a warm run would grow. Ends mid-turn: a tool call with
 * its output but no assistant reply and no token_count yet, so re-ingesting
 * after growth must not double-count what was already stored.
 */
function liveGrowth(id) {
  const callId = 'call_fixture_0001';
  return [
    sessionMeta(id),
    turnContext(),
    userMessage('Check whether the lockfile changed.'),
    functionCall(callId, 'shell_command', { command: 'git status --short' }),
    functionOutput(callId, ' M pnpm-lock.yaml'),
  ];
}


/**
 * 07 — un-attributed first turn, official count arriving only later.
 *
 * The mirror of 04, and the harder direction: a turn-aware policy has to
 * decide what the later official value covers. If `last_token_usage` covers
 * only its own turn, turn 1 stays uncovered and is estimable; if the estimator
 * instead reads the arrival of *any* official count as "this session is
 * attributed", turn 1 is silently left at zero.
 */
function unattributedThenOfficial(id) {
  return [
    sessionMeta(id),
    turnContext(),
    userMessage('Start the migration.'),
    assistantMessage('Migrated the first two tables.'),
    userMessage('Continue with the rest.'),
    assistantMessage('Done — the remaining four are migrated.'),
    tokenCountLast(usage(150, 60), usage(150, 60)),
  ];
}

/**
 * 08 — several token_count events for one assistant turn.
 *
 * Real Codex emits a token_count per model response, and a single turn can
 * produce more than one. Whether the last wins or they accumulate changes the
 * stored value, so the policy has to say which.
 */
function multipleCountsOneAssistant(id) {
  return [
    sessionMeta(id),
    turnContext(),
    userMessage('Refactor the reader.'),
    assistantMessage('Refactored, and the tests still pass.'),
    tokenCountLast(usage(80, 20), usage(80, 20)),
    tokenCountLast(usage(45, 15), usage(125, 35)),
  ];
}

/**
 * 09 — an assistant-only tail: a turn with no user record before it.
 *
 * Nothing precedes the assistant, so official input tokens on it cannot be
 * covering a user record. A turn-aware policy must not look for one.
 */
function assistantOnlyTail(id) {
  return [
    sessionMeta(id),
    turnContext(),
    assistantMessage('Continuing from the previous session.'),
    tokenCountLast(usage(60, 25), usage(60, 25)),
  ];
}

/**
 * 10 — a user-only tail: a user turn the model never answered.
 *
 * There is no assistant row to attribute onto, so any official count that
 * arrives has nowhere to go. Distinguishing this from "not processed yet" is
 * one of the Phase 3A exit-gate requirements.
 */
function userOnlyTail(id) {
  return [
    sessionMeta(id),
    turnContext(),
    userMessage('Are the migrations reversible?'),
  ];
}

// ─── Emit ──────────────────────────────────────────────────────────────────

const SESSIONS = [
  ['01-official-per-turn', '019c0001-0000-7000-8000-000000000001', officialPerTurn],
  ['02-no-token-count', '019c0002-0000-7000-8000-000000000002', noTokenCount],
  ['03-total-only', '019c0003-0000-7000-8000-000000000003', totalOnly],
  ['04-mixed-coverage', '019c0004-0000-7000-8000-000000000004', mixedCoverage],
  ['05-empty-internal', '019c0005-0000-7000-8000-000000000005', emptyInternal],
  ['06-live-growth', '019c0006-0000-7000-8000-000000000006', liveGrowth],
  ['07-unattributed-then-official', '019c0007-0000-7000-8000-000000000007', unattributedThenOfficial],
  ['08-multiple-counts-one-assistant', '019c0008-0000-7000-8000-000000000008', multipleCountsOneAssistant],
  ['09-assistant-only-tail', '019c0009-0000-7000-8000-000000000009', assistantOnlyTail],
  ['10-user-only-tail', '019c0010-0000-7000-8000-000000000010', userOnlyTail],
];

for (const [label, id, build] of SESSIONS) {
  const file = path.join(OUT, '.codex', 'sessions', '2026', '04', '01', `rollout-2026-04-01T10-00-00-${label}-${id}.jsonl`);
  writePinned(file, build(id));
}

console.log(`wrote ${SESSIONS.length} Codex sessions to ${path.join(OUT, '.codex', 'sessions', '2026', '04', '01')}`);

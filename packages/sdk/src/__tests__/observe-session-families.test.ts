/**
 * The RFC 012C families Chopsticks needs, end to end.
 *
 * `observeSession` has carried all eleven families on the wire since L1, but
 * the Claude decoder only emitted three of them, so a consumer could not see a
 * message, a tool call, or a mode change. This drives a real `.claude`-shaped
 * tree through the real native addon and asserts those arrive as typed events
 * — which is the whole reason `watchSessionTranscript` can be retired.
 *
 * Nothing here parses a payload. `event.value` is the generated
 * `RuntimeSemanticValue` union, so narrowing on `family` is what types it.
 *
 * Skips when the native addon is not loadable, so a host without a rebuilt
 * binary still passes.
 */

import { test, describe, after } from 'node:test';
import assert from 'node:assert/strict';
import os from 'node:os';
import path from 'node:path';
import { mkdtempSync, mkdirSync, writeFileSync, rmSync } from 'node:fs';

import { loadNativeAddon } from '../native.js';
import { isSemanticEvent, observeSession, type ObserverEvent, type SessionObserver } from '../observe-session.js';

const native = loadNativeAddon();
const SESSION = '01234567-89ab-cdef-0123-456789abcdef';
const PROJECT = '-observe-families-fixture';

const roots: string[] = [];
const open: SessionObserver[] = [];

after(async () => {
  for (const observer of open.splice(0)) await observer.close();
  for (const root of roots.splice(0)) rmSync(root, { recursive: true, force: true, maxRetries: 10, retryDelay: 50 });
});

function record(value: Record<string, unknown>): string {
  return `${JSON.stringify({
    parentUuid: null,
    timestamp: '2026-08-11T00:00:00Z',
    sessionId: SESSION,
    cwd: '/fixture',
    version: '1',
    gitBranch: 'main',
    isSidechain: false,
    userType: 'external',
    ...value,
  })}\n`;
}

/**
 * A session that exercises the families a chat consumer renders: an assistant
 * turn with text and a tool call, the user turn carrying that tool's result,
 * and a permission-mode change.
 */
function fixtureTree(): { root: string; transcript: string } {
  const root = mkdtempSync(path.join(os.tmpdir(), 'spaghetti-families-'));
  roots.push(root);
  const project = path.join(root, 'projects', PROJECT);
  mkdirSync(project, { recursive: true });
  const transcript = path.join(project, `${SESSION}.jsonl`);
  writeFileSync(
    transcript,
    record({
      type: 'assistant',
      uuid: 'a-1',
      requestId: 'r-1',
      message: {
        model: 'fixture-model',
        id: 'resp-1',
        type: 'message',
        role: 'assistant',
        content: [
          { type: 'text', text: 'Reading it now.' },
          { type: 'tool_use', id: 'toolu_1', name: 'Read', input: { path: '/fixture/x' } },
        ],
        usage: { input_tokens: 10, output_tokens: 5, cache_creation_input_tokens: 0, cache_read_input_tokens: 0 },
      },
    }) +
      record({
        type: 'user',
        uuid: 'u-2',
        message: {
          role: 'user',
          content: [{ type: 'tool_result', tool_use_id: 'toolu_1', content: 'ok', is_error: false }],
        },
      }) +
      record({ type: 'permission-mode', uuid: 'p-3', permissionMode: 'acceptEdits' }),
  );
  return { root, transcript };
}

function attach(root: string, transcript: string): SessionObserver {
  const observer = observeSession(
    { adapter_id: 'claude-code', agent_root: root, transcript_path: transcript, poll_interval_ms: 15 },
    {},
  );
  open.push(observer);
  return observer;
}

async function collectThroughBootstrap(observer: SessionObserver, timeoutMs = 20_000): Promise<ObserverEvent[]> {
  const deadline = Date.now() + timeoutMs;
  const collected: ObserverEvent[] = [];
  const iterator = observer[Symbol.asyncIterator]();
  while (Date.now() < deadline) {
    const next = await iterator.next();
    if (next.done) break;
    collected.push(next.value);
    if (next.value.type === 'bootstrap_complete') return collected;
  }
  assert.fail(`bootstrap never completed; saw ${collected.map((event) => event.type).join(', ')}`);
}

describe('observeSession runtime families', { skip: !native }, () => {
  test('a message, a tool call and its result, and a mode change arrive as typed events', async () => {
    const { root, transcript } = fixtureTree();
    const events = await collectThroughBootstrap(attach(root, transcript));
    const semantic = events.filter(isSemanticEvent);

    // --- message + content blocks -------------------------------------
    const message = semantic.find((event) => event.family === 'message');
    assert.ok(message, 'the assistant turn should arrive as a message revision');
    assert.ok(message.value && 'MessageRevision' in message.value);
    const revision = message.value.MessageRevision;
    assert.equal(revision.native_message_id, 'a-1');
    assert.equal(revision.role, 'assistant');
    assert.equal(revision.completeness, 'complete');
    assert.deepEqual(
      revision.ordered_content_block_keys,
      ['o:0', 'n:toolu_1'],
      'the message names its blocks in order, so a consumer can render them without guessing',
    );

    const text = semantic
      .filter((event) => event.family === 'content_block')
      .map((event) => (event.value && 'ContentBlockRevision' in event.value ? event.value.ContentBlockRevision : null))
      .find((block) => block?.content.kind === 'text');
    assert.ok(text && text.content.kind === 'text');
    assert.equal(text.content.text, 'Reading it now.');

    // --- tool call and result -----------------------------------------
    const tools = semantic
      .filter((event) => event.family === 'tool')
      .map((event) => (event.value && 'ToolRevision' in event.value ? event.value.ToolRevision : null))
      .filter((tool) => tool !== null);
    const call = tools.find((tool) => tool.kind === 'call');
    const result = tools.find((tool) => tool.kind === 'result');
    assert.ok(call, 'the tool_use block should arrive as a call');
    assert.ok(result, 'its tool_result should arrive as a separate correlated entity');
    assert.equal(call.native_tool_id, 'toolu_1');
    assert.equal(call.tool_name, 'Read');
    assert.equal(result.tool_name, 'Read', 'the result is named by the call it answers');
    assert.equal(result.correlated_native_id, 'toolu_1');
    assert.notEqual(result.native_tool_id, call.native_tool_id, 'a call and its result are two entities');

    // --- effective state ----------------------------------------------
    const state = semantic
      .filter((event) => event.family === 'effective_state')
      .map((event) =>
        event.value && 'EffectiveStateRevision' in event.value ? event.value.EffectiveStateRevision : null,
      )
      .filter((value) => value !== null);
    const mode = state.find((value) => value.dimension === 'permission_mode');
    assert.ok(mode, 'the permission-mode record should arrive as effective state');
    assert.equal(mode.value.value, 'acceptEdits');
    assert.equal(
      mode.evidence_kind,
      'native_transition',
      'a record whose purpose is the mode change is a transition, not an observation',
    );

    const model = state.find((value) => value.dimension === 'model');
    assert.ok(model, 'the response model should arrive as effective state too');
    assert.equal(model.value.value, 'fixture-model');
    assert.equal(model.evidence_kind, 'response_observed');

    // --- every family carries its durable join identity ----------------
    for (const event of semantic) {
      assert.ok(
        event.semantic_revision_ref.fact_revision_id,
        `${event.family} must carry the reference a durable query returns for the same revision`,
      );
      assert.equal(event.actor.native_session_id, SESSION);
    }
  });

  test('the bootstrap barrier reports the families it actually reduced', async () => {
    const { root, transcript } = fixtureTree();
    const events = await collectThroughBootstrap(attach(root, transcript));
    const barrier = events.at(-1);
    assert.equal(barrier?.type, 'bootstrap_complete');
    assert.ok(barrier && 'family_manifest' in barrier);

    const populated = barrier.family_manifest.filter((entry) => entry.entity_count > 0).map((entry) => entry.family);
    for (const family of ['message', 'content_block', 'tool', 'effective_state', 'actor_run', 'usage_v2']) {
      assert.ok(
        populated.includes(family as (typeof populated)[number]),
        `${family} should hold reduced state after bootstrap, saw ${populated.join(', ')}`,
      );
    }
  });
});

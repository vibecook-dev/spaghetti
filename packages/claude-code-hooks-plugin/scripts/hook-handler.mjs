#!/usr/bin/env node

/**
 * Universal hook handler for spaghetti-hooks plugin.
 *
 * Every hook event pipes its JSON stdin through this script.
 * It reads the payload, appends a structured log entry to a JSONL file,
 * and optionally outputs additionalContext for supported hooks.
 *
 * Zero dependencies — pure Node.js.
 */

import { readFileSync, appendFileSync, mkdirSync } from 'node:fs';
import { homedir } from 'node:os';
import { join } from 'node:path';

// ── Read stdin synchronously ────────────────────────────────────────────

let raw = '';
try {
  raw = readFileSync(0, 'utf-8'); // fd 0 = stdin, cross-platform
} catch {
  raw = '{}';
}

let input;
try {
  input = JSON.parse(raw);
} catch {
  input = { _raw: raw };
}

// ── Build log entry ─────────────────────────────────────────────────────

const event = input.hook_event_name || process.env.HOOK_EVENT_NAME || 'unknown';
const timestamp = new Date().toISOString();

const commonKeys = [
  'session_id', 'cwd', 'permission_mode',
  'transcript_path', 'hook_event_name',
  'agent_id', 'agent_type',
];

const logEntry = {
  timestamp,
  event,
  sessionId: input.session_id || null,
  cwd: input.cwd || null,
  permissionMode: input.permission_mode || null,
  transcriptPath: input.transcript_path || null,
  agentId: input.agent_id || null,
  agentType: input.agent_type || null,
  payload: {},
};

// Copy event-specific fields into payload (exclude common fields)
for (const [k, v] of Object.entries(input)) {
  if (!commonKeys.includes(k)) {
    logEntry.payload[k] = v;
  }
}

// ── Append to JSONL ─────────────────────────────────────────────────────

// `homedir()`, not `process.env.HOME` — HOME is not a Windows-guaranteed
// variable (USERPROFILE is), and the `/tmp` fallback resolved
// drive-relative, writing events somewhere the SDK never reads. Both
// sides must agree on `~/.spaghetti/hooks`; see `SPAGHETTI_DIR` in
// packages/cli/src/lib/updater.ts and the hooks reader in the SDK.
const dataDir = join(homedir(), '.spaghetti', 'hooks');
try {
  mkdirSync(dataDir, { recursive: true });
} catch { /* already exists */ }

const logFile = join(dataDir, 'events.jsonl');
try {
  appendFileSync(logFile, JSON.stringify(logEntry) + '\n');
} catch (e) {
  process.stderr.write(`[spaghetti-hooks] Failed to write log: ${e.message}\n`);
}

// ── Output additionalContext for supported hooks ────────────────────────

// Only hooks that accept hookSpecificOutput.additionalContext in their schema
const contextHooks = [
  'UserPromptSubmit', 'PreToolUse', 'PostToolUse',
  'PostToolUseFailure', 'Notification', 'SubagentStart', 'SubagentStop',
];

if (contextHooks.includes(event)) {
  process.stdout.write(JSON.stringify({
    hookSpecificOutput: {
      hookEventName: event,
      additionalContext: `[spaghetti-hooks] ${event} captured at ${timestamp}`,
    },
  }));
}

process.exit(0);

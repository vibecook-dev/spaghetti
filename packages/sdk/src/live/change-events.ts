/**
 * change-events.ts — Live-updates change union + topic types.
 *
 * Foundation for Phase 3 of RFC 005 ("Change events + React hooks").
 * Nothing imports this file yet; the stubs on `AgentDataStore` (see
 * `emit` / `subscribe` / `lastEmittedSeq` in `agent-data-store.ts`)
 * reference these types so the full discriminated union is wired into
 * the type system before any subscriber plumbing lands.
 *
 * The shapes here are a verbatim copy of `docs/LIVE-UPDATES-DESIGN.md`
 * §2.9 — keep them in sync if the design doc changes.
 */

import type {
  SessionMessage,
  SessionIndexEntry,
  SubagentTranscript,
  TodoItem,
  TaskEntry,
  PlanFile,
  SettingsFile,
} from '../types/index.js';

// ═══════════════════════════════════════════════════════════════════════════
// DISPOSE HELPER
// ═══════════════════════════════════════════════════════════════════════════

/**
 * The "tear this subscription down" handle every live-updates API
 * returns. Aligns with `AsyncDisposable`-style cleanup: call it once,
 * further calls are no-ops.
 */
export type Dispose = () => void;

// ═══════════════════════════════════════════════════════════════════════════
// CHANGE UNION
// ═══════════════════════════════════════════════════════════════════════════

/**
 * Discriminated union of every event `LiveUpdates` can emit.
 *
 * `seq` is an in-memory monotonic counter reset on each process start
 * — useful for ordering logs and for subscribers that want to
 * deduplicate; never persisted.  `ts` is `Date.now()` at emit time.
 *
 * Payloads carry enough context that subscribers don't need to re-
 * query SQLite for the common case (append new message, redraw chat).
 */
export type Change = {
  /** Agent source that committed the change. Optional for legacy callers. */
  sourceId?: string;
} & (
  | {
      type: 'session.message.added';
      seq: number;
      ts: number;
      slug: string;
      sessionId: string;
      message: SessionMessage;
      byteOffset: number;
    }
  | {
      type: 'session.created';
      seq: number;
      ts: number;
      slug: string;
      sessionId: string;
      entry: SessionIndexEntry;
    }
  | {
      type: 'session.rewritten';
      seq: number;
      ts: number;
      slug: string;
      sessionId: string;
    }
  | {
      type: 'subagent.updated';
      seq: number;
      ts: number;
      slug: string;
      sessionId: string;
      agentId: string;
      transcript: SubagentTranscript;
    }
  | {
      type: 'tool-result.added';
      seq: number;
      ts: number;
      slug: string;
      sessionId: string;
      toolUseId: string;
    }
  | {
      type: 'file-history.added';
      seq: number;
      ts: number;
      sessionId: string;
      hash: string;
      version: number;
    }
  | {
      type: 'todo.updated';
      seq: number;
      ts: number;
      sessionId: string;
      agentId: string;
      items: TodoItem[];
    }
  | {
      type: 'task.updated';
      seq: number;
      ts: number;
      sessionId: string;
      task: TaskEntry;
    }
  | {
      type: 'plan.upserted';
      seq: number;
      ts: number;
      slug: string;
      plan: PlanFile;
    }
  | {
      type: 'settings.changed';
      seq: number;
      ts: number;
      file: 'settings' | 'settings.local';
      settings: SettingsFile;
    }
);

/**
 * Convenience alias for the literal `Change['type']` tag set —
 * useful for topic-matching code that wants a narrow string type.
 */
export type ChangeType = Change['type'];

// ═══════════════════════════════════════════════════════════════════════════
// TOPIC TYPES
// ═══════════════════════════════════════════════════════════════════════════

/**
 * Topic selector used by `AgentDataStore.subscribe` / `api.live.onChange`.
 *
 * A topic narrows a subscription to a subset of the `Change` union.
 * Omitted qualifiers widen the scope: `{ kind: 'session' }` matches
 * every session event across every project; adding `slug` narrows to
 * one project, adding `sessionId` narrows to one session, etc.
 */
export type ChangeTopic =
  | { kind: 'session'; slug?: string; sessionId?: string }
  | { kind: 'subagent'; slug?: string; sessionId?: string; agentId?: string }
  | { kind: 'tool-result'; slug?: string; sessionId?: string }
  | { kind: 'file-history'; sessionId?: string }
  | { kind: 'todo'; sessionId?: string }
  | { kind: 'task'; sessionId?: string }
  | { kind: 'plan'; slug?: string }
  | { kind: 'settings' };

/**
 * Per-subscription throttling knobs, applied by the subscriber
 * registry when dispatching events to a listener.
 *
 * - `throttleMs`: minimum gap in milliseconds between listener
 *   invocations.
 * - `latest`: when `true` (default when `throttleMs` is set),
 *   intermediate events are dropped and only the most recent is
 *   emitted at each throttle boundary — the listener signature stays
 *   `(e: Change) => void`. When `false`, coalesced events are
 *   delivered as an array and the listener signature widens to
 *   `(e: Change[]) => void`. The two shapes are distinguished at the
 *   type level via the overloaded `subscribe()` / `onChange()`
 *   signatures so consumers never see a listener called with the
 *   "wrong" argument shape.
 */
export type SubscribeOptions = SubscribeOptionsLatest | SubscribeOptionsCoalesced;

/** Default — listener gets one `Change` at a time. */
export interface SubscribeOptionsLatest {
  throttleMs?: number;
  latest?: true;
}

/** Explicit opt-in — listener gets a batch array at each throttle boundary. */
export interface SubscribeOptionsCoalesced {
  throttleMs: number;
  latest: false;
}

/**
 * Listener type selected by the options object. When `options.latest`
 * is literally `false`, the listener argument widens to `Change[]`;
 * otherwise (default, `true`, or no throttle) it stays `Change`.
 */
export type SubscribeListener<O extends SubscribeOptions | undefined> = O extends { latest: false }
  ? (e: Change[]) => void
  : (e: Change) => void;

// ═══════════════════════════════════════════════════════════════════════════
// TYPE GUARDS (one per variant)
// ═══════════════════════════════════════════════════════════════════════════

/**
 * Narrow a `Change` by variant. Used inside hot fan-out loops where
 * `switch(c.type)` is perfectly fine too — these helpers exist for
 * single-case filters like `.filter(isSessionMessageAdded)` and are
 * re-exported through `live/index.ts` when that barrel lands.
 */
export const isSessionMessageAdded = (c: Change): c is Extract<Change, { type: 'session.message.added' }> =>
  c.type === 'session.message.added';

export const isSessionCreated = (c: Change): c is Extract<Change, { type: 'session.created' }> =>
  c.type === 'session.created';

export const isSessionRewritten = (c: Change): c is Extract<Change, { type: 'session.rewritten' }> =>
  c.type === 'session.rewritten';

export const isSubagentUpdated = (c: Change): c is Extract<Change, { type: 'subagent.updated' }> =>
  c.type === 'subagent.updated';

export const isToolResultAdded = (c: Change): c is Extract<Change, { type: 'tool-result.added' }> =>
  c.type === 'tool-result.added';

export const isFileHistoryAdded = (c: Change): c is Extract<Change, { type: 'file-history.added' }> =>
  c.type === 'file-history.added';

export const isTodoUpdated = (c: Change): c is Extract<Change, { type: 'todo.updated' }> => c.type === 'todo.updated';

export const isTaskUpdated = (c: Change): c is Extract<Change, { type: 'task.updated' }> => c.type === 'task.updated';

export const isPlanUpserted = (c: Change): c is Extract<Change, { type: 'plan.upserted' }> =>
  c.type === 'plan.upserted';

export const isSettingsChanged = (c: Change): c is Extract<Change, { type: 'settings.changed' }> =>
  c.type === 'settings.changed';

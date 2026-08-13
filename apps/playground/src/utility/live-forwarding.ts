import type { Change, ChangeTopic, InitProgress, SegmentChangeBatch, SegmentType } from '@vibecook/spaghetti-sdk';
import type { ObservationService } from '@vibecook/spaghetti-sdk/observation';

export interface PlaygroundEventSink {
  progress(progress: InitProgress): void;
  ready(info: { durationMs: number }): void;
  change(batch: SegmentChangeBatch): void;
}

/** Only scopes represented by the playground UI are kept hot. */
export const PLAYGROUND_LIVE_TOPICS: readonly ChangeTopic[] = [
  // projects/** covers messages, subagents, tool results, and memory.
  { kind: 'session' },
  { kind: 'todo' },
  { kind: 'task' },
  { kind: 'plan' },
];

/** Keep active transcripts responsive while still collapsing bursty writes. */
export const LIVE_FORWARD_THROTTLE_MS = 120;

function segmentType(change: Change): SegmentType {
  switch (change.type) {
    case 'session.message.added':
      return 'message';
    case 'session.created':
    case 'session.rewritten':
      return 'session';
    case 'subagent.updated':
      return 'subagent';
    case 'tool-result.added':
      return 'tool_result';
    case 'file-history.added':
      return 'file_history';
    case 'todo.updated':
      return 'todo';
    case 'task.updated':
      return 'task';
    case 'plan.upserted':
      return 'plan';
    case 'settings.changed':
      return 'config_settings';
  }
}

function liveChangeToSegment(change: Change): SegmentChangeBatch['changes'][number] {
  const projectSlug = 'slug' in change ? change.slug : undefined;
  const sessionId = 'sessionId' in change ? change.sessionId : undefined;
  return {
    key: `live:${change.type}:${change.seq}`,
    type: segmentType(change),
    action: 'upsert',
    revision: change.seq,
    ...(change.sourceId ? { sourceId: change.sourceId } : {}),
    ...(projectSlug ? { projectSlug } : {}),
    ...(sessionId ? { sessionId } : {}),
  };
}

export function liveChangesToBatch(changes: readonly Change[]): SegmentChangeBatch {
  return {
    changes: changes.map(liveChangeToSegment),
    timestamp: changes.at(-1)?.ts ?? Date.now(),
  };
}

export function liveChangeToBatch(change: Change): SegmentChangeBatch {
  return liveChangesToBatch([change]);
}

/** Own every SDK subscription and watcher prewarm as one disposable. */
export function attachPlaygroundEventForwarding(sdk: ObservationService, sink: PlaygroundEventSink): () => void {
  const disposers: Array<() => void> = [
    sdk.onProgress(sink.progress),
    sdk.onReady(sink.ready),
    sdk.onChange(sink.change),
  ];

  let disposed = false;
  return () => {
    if (disposed) return;
    disposed = true;
    for (const dispose of disposers.reverse()) {
      try {
        dispose();
      } catch (err) {
        console.error('[sdk-host] live subscription cleanup failed', err);
      }
    }
  };
}

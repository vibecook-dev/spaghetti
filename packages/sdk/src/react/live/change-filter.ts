import type { SegmentChange, SegmentChangeBatch } from '../../data/segment-types.js';
import type { ChangeTopic } from '../../live/change-events.js';

/** Empty compatibility batches are global invalidations by design. */
export function changeBatchMatchesTopic(batch: SegmentChangeBatch, topic?: ChangeTopic): boolean {
  if (topic === undefined || batch.changes.length === 0) return true;
  return batch.changes.some((change) => changeMatchesTopic(change, topic));
}

function changeMatchesTopic(change: SegmentChange, topic: ChangeTopic): boolean {
  switch (topic.kind) {
    case 'session':
      return (
        matchesSegment(change.type, ['project', 'session', 'message', 'project_summary', 'session_summary']) &&
        matchesOptional(change.projectSlug, topic.slug) &&
        matchesOptional(change.sessionId, topic.sessionId)
      );
    case 'subagent':
      return (
        change.type === 'subagent' &&
        matchesOptional(change.projectSlug, topic.slug) &&
        matchesOptional(change.sessionId, topic.sessionId)
      );
    case 'tool-result':
      return (
        change.type === 'tool_result' &&
        matchesOptional(change.projectSlug, topic.slug) &&
        matchesOptional(change.sessionId, topic.sessionId)
      );
    case 'file-history':
      return change.type === 'file_history' && matchesOptional(change.sessionId, topic.sessionId);
    case 'todo':
      return change.type === 'todo' && matchesOptional(change.sessionId, topic.sessionId);
    case 'task':
      return change.type === 'task' && matchesOptional(change.sessionId, topic.sessionId);
    case 'plan':
      return change.type === 'plan' && matchesOptional(change.projectSlug, topic.slug);
    case 'settings':
      return change.type.startsWith('config_');
  }
}

function matchesOptional(actual: string | undefined, expected: string | undefined): boolean {
  return expected === undefined || actual === expected;
}

function matchesSegment(actual: string, expected: readonly string[]): boolean {
  return expected.includes(actual);
}

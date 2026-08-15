import assert from 'node:assert/strict';
import test from 'node:test';

import type { ProjectListItem, SegmentChange } from '@vibecook/spaghetti-sdk';
import { applyProjectLiveActivity } from './project-live-activity.js';

const project: ProjectListItem = {
  projectId: 'path:/workspace/spaghetti',
  members: [{ sourceId: 'codex', slug: '/workspace/spaghetti' }],
  slug: '/workspace/spaghetti',
  sourceIds: ['codex'],
  folderName: 'spaghetti',
  absolutePath: '/workspace/spaghetti',
  sessionCount: 2,
  messageCount: 10,
  tokenUsage: {
    inputTokens: 1,
    outputTokens: 2,
    cacheCreationTokens: 3,
    cacheReadTokens: 4,
    totalTokens: 10,
  },
  tokensEstimated: false,
  firstActiveAt: '2026-08-01T00:00:00.000Z',
  lastActiveAt: '2026-08-02T00:00:00.000Z',
  latestGitBranch: 'main',
  latestPrompt: 'hello',
  hasMemory: false,
};

test('updates only the touched project activity without guessing aggregate deltas', () => {
  const other = {
    ...project,
    projectId: 'path:/workspace/other',
    folderName: 'other',
    members: [{ sourceId: 'codex', slug: '/workspace/other' }],
  };
  const changes: SegmentChange[] = [
    {
      key: 'message:session-1/message-1',
      type: 'message',
      action: 'upsert',
      sourceId: 'codex',
      projectSlug: '/workspace/spaghetti',
      sessionId: 'session-1',
      revision: 4,
    },
  ];

  const projects = [project, other];
  const next = applyProjectLiveActivity(projects, changes, Date.parse('2026-08-14T12:00:00.000Z'));

  assert.notEqual(next, projects);
  assert.equal(next[0]?.lastActiveAt, '2026-08-14T12:00:00.000Z');
  assert.equal(next[0]?.messageCount, 10);
  assert.equal(next[0]?.tokenUsage.totalTokens, 10);
  assert.equal(next[1], other);
});

test('empty and unrelated change batches preserve the project-list identity', () => {
  const projects = [project];
  assert.equal(applyProjectLiveActivity(projects, [], Date.now()), projects);
  assert.equal(
    applyProjectLiveActivity(
      projects,
      [
        {
          key: 'message:session-1/message-2',
          type: 'message',
          action: 'upsert',
          sourceId: 'codex',
          projectSlug: '/workspace/unrelated',
          sessionId: 'session-1',
          revision: 5,
        },
      ],
      Date.now(),
    ),
    projects,
  );
});

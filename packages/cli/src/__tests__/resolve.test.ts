import { test, describe } from 'node:test';
import assert from 'node:assert';
import { resolveProject, resolveSession } from '../lib/resolve.js';

const mockProjects = [
  {
    slug: '-Users-james-spaghetti',
    folderName: 'spaghetti',
    absolutePath: '/Users/james/spaghetti',
    sessionCount: 10,
    messageCount: 100,
    tokenUsage: { inputTokens: 0, outputTokens: 0, cacheCreationTokens: 0, cacheReadTokens: 0 },
    lastActiveAt: '2026-03-21',
    firstActiveAt: '2026-03-01',
    latestGitBranch: 'main',
    hasMemory: true,
  },
  {
    slug: '-Users-james-jabali',
    folderName: 'jabali',
    absolutePath: '/Users/james/jabali',
    sessionCount: 5,
    messageCount: 50,
    tokenUsage: { inputTokens: 0, outputTokens: 0, cacheCreationTokens: 0, cacheReadTokens: 0 },
    lastActiveAt: '2026-03-20',
    firstActiveAt: '2026-02-01',
    latestGitBranch: 'main',
    hasMemory: false,
  },
] as any[];

const mockSessions = [
  {
    sessionId: 'a1b2c3d4-e5f6-7890-abcd-ef1234567890',
    startTime: '2026-03-21',
    lastUpdate: '2026-03-21',
    messageCount: 47,
    lifespanMs: 3600000,
    tokenUsage: { inputTokens: 0, outputTokens: 0, cacheCreationTokens: 0, cacheReadTokens: 0 },
    gitBranch: 'main',
    summary: '',
    firstPrompt: '',
  },
  {
    sessionId: 'b2c3d4e5-f6a7-8901-bcde-f12345678901',
    startTime: '2026-03-20',
    lastUpdate: '2026-03-20',
    messageCount: 23,
    lifespanMs: 1800000,
    tokenUsage: { inputTokens: 0, outputTokens: 0, cacheCreationTokens: 0, cacheReadTokens: 0 },
    gitBranch: 'feature',
    summary: '',
    firstPrompt: '',
  },
] as any[];

describe('resolveProject', () => {
  test('resolves by exact name', () => {
    const r = resolveProject('spaghetti', mockProjects);
    assert.strictEqual(r?.folderName, 'spaghetti');
  });

  test('resolves by numeric index', () => {
    const r = resolveProject('1', mockProjects);
    assert.strictEqual(r?.folderName, 'spaghetti');
  });

  test('resolves by prefix', () => {
    const r = resolveProject('spag', mockProjects);
    assert.strictEqual(r?.folderName, 'spaghetti');
  });

  test('resolves by case-insensitive name', () => {
    const r = resolveProject('SPAGHETTI', mockProjects);
    assert.strictEqual(r?.folderName, 'spaghetti');
  });

  test('resolves second project by index', () => {
    const r = resolveProject('2', mockProjects);
    assert.strictEqual(r?.folderName, 'jabali');
  });

  test('returns null on no match', () => {
    const r = resolveProject('nonexistent', mockProjects);
    assert.strictEqual(r, null);
  });

  test('returns null for out-of-range index', () => {
    const r = resolveProject('99', mockProjects);
    assert.strictEqual(r, null);
  });

  test('returns null for empty project list', () => {
    const r = resolveProject('anything', []);
    assert.strictEqual(r, null);
  });

  test('does not guess when lossy slugs and folder names are ambiguous', () => {
    const collisions = [
      { ...mockProjects[0], projectId: 'path:/tmp/foo/bar', slug: '-tmp-foo-bar', folderName: 'bar' },
      { ...mockProjects[1], projectId: 'path:/tmp/foo-bar', slug: '-tmp-foo-bar', folderName: 'bar' },
    ];
    assert.strictEqual(resolveProject('bar', collisions), null);
    assert.strictEqual(resolveProject('-tmp-foo-bar', collisions), null);
    assert.strictEqual(resolveProject('path:/tmp/foo/bar', collisions)?.projectId, 'path:/tmp/foo/bar');
  });
});

describe('resolveSession', () => {
  test('resolves latest', () => {
    const r = resolveSession('latest', mockSessions);
    assert.strictEqual(r?.sessionId, 'a1b2c3d4-e5f6-7890-abcd-ef1234567890');
  });

  test('resolves "last" alias', () => {
    const r = resolveSession('last', mockSessions);
    assert.strictEqual(r?.sessionId, 'a1b2c3d4-e5f6-7890-abcd-ef1234567890');
  });

  test('resolves by index', () => {
    const r = resolveSession('2', mockSessions);
    assert.strictEqual(r?.sessionId, 'b2c3d4e5-f6a7-8901-bcde-f12345678901');
  });

  test('resolves by partial UUID (6+ chars)', () => {
    const r = resolveSession('a1b2c3', mockSessions);
    assert.strictEqual(r?.sessionId, 'a1b2c3d4-e5f6-7890-abcd-ef1234567890');
  });

  test('resolves by full UUID', () => {
    const r = resolveSession('a1b2c3d4-e5f6-7890-abcd-ef1234567890', mockSessions);
    assert.strictEqual(r?.sessionId, 'a1b2c3d4-e5f6-7890-abcd-ef1234567890');
  });

  test('returns null on no match', () => {
    const r = resolveSession('zzzzz', mockSessions);
    assert.strictEqual(r, null);
  });

  test('returns null for out-of-range index', () => {
    const r = resolveSession('99', mockSessions);
    assert.strictEqual(r, null);
  });

  test('returns null for empty session list', () => {
    const r = resolveSession('latest', []);
    assert.strictEqual(r, null);
  });

  test('rejects partial UUID shorter than 6 chars', () => {
    const r = resolveSession('a1b2c', mockSessions);
    assert.strictEqual(r, null);
  });
});

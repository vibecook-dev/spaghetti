/** Thin IPC broker: renderer requests are forwarded to the SDK UtilityProcess. */

import { ipcMain } from 'electron';
import type { ProjectReference } from '@vibecook/spaghetti-sdk';
import { IPC_CHANNELS } from '../shared/ipc.js';
import type { SdkHostClient } from './sdk-host-client.js';
import { readCanonicalStats } from './canonical-queries.js';
import { listWorktrees } from './worktrees.js';

export function registerIpcHandlers(client: SdkHostClient): void {
  // Lifecycle ---------------------------------------------------------------
  ipcMain.handle(IPC_CHANNELS.isReady, () => client.request('isReady'));
  ipcMain.handle(IPC_CHANNELS.rebuildIndex, () => client.request('rebuildIndex'));
  ipcMain.handle(IPC_CHANNELS.retryInit, () => client.request('retryInit'));
  ipcMain.handle(IPC_CHANNELS.getEngine, () => client.request('getEngine'));
  ipcMain.handle(IPC_CHANNELS.getObservationHostStatus, () => client.request('getObservationHostStatus'));
  ipcMain.handle(IPC_CHANNELS.getObservationOwnerStatus, () => client.request('getObservationOwnerStatus'));
  ipcMain.handle(IPC_CHANNELS.getCanonicalStats, () => readCanonicalStats(client));

  // Projects ----------------------------------------------------------------
  ipcMain.handle(IPC_CHANNELS.getProjectList, () => client.request('getProjectList'));
  ipcMain.handle(IPC_CHANNELS.getProjectTokenActivity, (_event, project: ProjectReference, query) =>
    client.request('getProjectTokenActivity', project, query),
  );
  ipcMain.handle(IPC_CHANNELS.getProjectMemory, (_event, project: ProjectReference, options?: { sourceId?: string }) =>
    client.request('getProjectMemory', project, options),
  );
  // Answered here rather than forwarded: a live `git worktree list` is a
  // question about the workspace right now, not about the session files the
  // SDK derives its database from. See the header of `worktrees.ts`.
  ipcMain.handle(IPC_CHANNELS.getProjectWorktrees, (_event, projectPath: string) => listWorktrees(projectPath));

  // Sessions ----------------------------------------------------------------
  ipcMain.handle(IPC_CHANNELS.getSessionList, (_event, project: ProjectReference, options?: { sourceId?: string }) =>
    client.request('getSessionList', project, options),
  );
  ipcMain.handle(
    IPC_CHANNELS.getSessionMessages,
    (
      _event,
      projectSlug: string,
      sessionId: string,
      limit?: number,
      offset?: number,
      options?: { sourceId?: string },
    ) => client.request('getSessionMessages', projectSlug, sessionId, limit, offset, options),
  );
  ipcMain.handle(
    IPC_CHANNELS.getSessionTimelineFacets,
    (_event, projectSlug: string, sessionId: string, options?: { sourceId?: string }) =>
      client.request('getSessionTimelineFacets', projectSlug, sessionId, options),
  );
  ipcMain.handle(IPC_CHANNELS.getSessionTimeline, (_event, projectSlug: string, sessionId: string, request) =>
    client.request('getSessionTimeline', projectSlug, sessionId, request),
  );
  ipcMain.handle(IPC_CHANNELS.openSessionStream, (_event, projectSlug: string, sessionId: string, request) =>
    client.request('openSessionStream', projectSlug, sessionId, request),
  );
  ipcMain.handle(IPC_CHANNELS.closeSessionStream, (_event, streamId: string) =>
    client.request('closeSessionStream', streamId),
  );
  ipcMain.handle(IPC_CHANNELS.getSessionTodos, (_event, projectSlug: string, sessionId: string) =>
    client.request('getSessionTodos', projectSlug, sessionId),
  );
  ipcMain.handle(IPC_CHANNELS.getSessionPlan, (_event, projectSlug: string, sessionId: string) =>
    client.request('getSessionPlan', projectSlug, sessionId),
  );
  ipcMain.handle(IPC_CHANNELS.getSessionTask, (_event, projectSlug: string, sessionId: string) =>
    client.request('getSessionTask', projectSlug, sessionId),
  );
  ipcMain.handle(IPC_CHANNELS.getToolResult, (_event, projectSlug: string, sessionId: string, toolUseId: string) =>
    client.request('getToolResult', projectSlug, sessionId, toolUseId),
  );

  // Subagents ---------------------------------------------------------------
  ipcMain.handle(IPC_CHANNELS.getSessionSubagents, (_event, projectSlug: string, sessionId: string, options) =>
    client.request('getSessionSubagents', projectSlug, sessionId, options),
  );
  ipcMain.handle(
    IPC_CHANNELS.getSubagentMessages,
    (
      _event,
      projectSlug: string,
      sessionId: string,
      agentId: string,
      limit?: number,
      offset?: number,
      workflowId?: string,
      options?: { sourceId?: string },
    ) => client.request('getSubagentMessages', projectSlug, sessionId, agentId, limit, offset, workflowId, options),
  );
  ipcMain.handle(
    IPC_CHANNELS.getSubagentTimeline,
    (_event, projectSlug: string, sessionId: string, agentId: string, request) =>
      client.request('getSubagentTimeline', projectSlug, sessionId, agentId, request),
  );

  // Search / stats ----------------------------------------------------------
  ipcMain.handle(IPC_CHANNELS.search, (_event, query) => client.request('search', query));
  ipcMain.handle(IPC_CHANNELS.getStats, () => client.request('getStats'));
}

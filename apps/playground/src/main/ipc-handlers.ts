/**
 * Wires the shared IPC contract to the SDK instance.
 */

import { BrowserWindow, ipcMain } from 'electron';
import { EVENT_CHANNELS, IPC_CHANNELS } from '../shared/ipc.js';
import { createSdk, getEngine, getSdk, retrySdkInit, startSdk, type InitSdkOptions } from './sdk.js';

function broadcast(channel: string, payload: unknown): void {
  for (const win of BrowserWindow.getAllWindows()) {
    if (!win.isDestroyed()) {
      win.webContents.send(channel, payload);
    }
  }
}

function attachLifecycleBroadcast(sdk: ReturnType<typeof createSdk>): void {
  sdk.onProgress((progress) => broadcast(EVENT_CHANNELS.progress, progress));
  sdk.onReady((info) => broadcast(EVENT_CHANNELS.ready, info));
  sdk.onChange((batch) => broadcast(EVENT_CHANNELS.change, batch));
}

export function registerIpcHandlers(): void {
  // Lifecycle ---------------------------------------------------------------
  ipcMain.handle(IPC_CHANNELS.isReady, () => {
    try {
      return getSdk().isReady();
    } catch {
      return false;
    }
  });
  ipcMain.handle(IPC_CHANNELS.rebuildIndex, () => getSdk().rebuildIndex());
  ipcMain.handle(IPC_CHANNELS.retryInit, async () => {
    await retrySdkInit((service) => {
      attachLifecycleBroadcast(service);
    });
    return { ok: true as const };
  });
  ipcMain.handle(IPC_CHANNELS.getEngine, () => getEngine());

  // Projects ----------------------------------------------------------------
  ipcMain.handle(IPC_CHANNELS.getProjectList, () => getSdk().getProjectList());
  ipcMain.handle(IPC_CHANNELS.getProjectMemory, (_e, projectSlug: string) => getSdk().getProjectMemory(projectSlug));

  // Sessions ----------------------------------------------------------------
  ipcMain.handle(IPC_CHANNELS.getSessionList, (_e, projectSlug: string, options?: { sourceId?: string }) =>
    getSdk().getSessionList(projectSlug, options),
  );
  ipcMain.handle(
    IPC_CHANNELS.getSessionMessages,
    (_e, projectSlug: string, sessionId: string, limit?: number, offset?: number, options?: { sourceId?: string }) =>
      getSdk().getSessionMessages(projectSlug, sessionId, limit, offset, options),
  );
  ipcMain.handle(IPC_CHANNELS.getSessionTodos, (_e, projectSlug: string, sessionId: string) =>
    getSdk().getSessionTodos(projectSlug, sessionId),
  );
  ipcMain.handle(IPC_CHANNELS.getSessionPlan, (_e, projectSlug: string, sessionId: string) =>
    getSdk().getSessionPlan(projectSlug, sessionId),
  );
  ipcMain.handle(IPC_CHANNELS.getSessionTask, (_e, projectSlug: string, sessionId: string) =>
    getSdk().getSessionTask(projectSlug, sessionId),
  );
  ipcMain.handle(IPC_CHANNELS.getToolResult, (_e, projectSlug: string, sessionId: string, toolUseId: string) =>
    getSdk().getToolResult(projectSlug, sessionId, toolUseId),
  );

  // Subagents ---------------------------------------------------------------
  ipcMain.handle(IPC_CHANNELS.getSessionSubagents, (_e, projectSlug: string, sessionId: string) =>
    getSdk().getSessionSubagents(projectSlug, sessionId),
  );
  ipcMain.handle(
    IPC_CHANNELS.getSubagentMessages,
    (_e, projectSlug: string, sessionId: string, agentId: string, limit?: number, offset?: number) =>
      getSdk().getSubagentMessages(projectSlug, sessionId, agentId, limit, offset),
  );

  // Search / stats ----------------------------------------------------------
  ipcMain.handle(IPC_CHANNELS.search, (_e, query) => getSdk().search(query));
  ipcMain.handle(IPC_CHANNELS.getStats, () => getSdk().getStats());
}

/**
 * Create the SDK, attach progress/ready/change listeners, then initialize.
 */
export async function wireEventForwarding(options: InitSdkOptions): Promise<void> {
  const sdk = createSdk(options);
  attachLifecycleBroadcast(sdk);
  await startSdk();
}

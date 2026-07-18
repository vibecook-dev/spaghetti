/**
 * Preload script — runs in an isolated world with access to Node + DOM.
 *
 * Uses contextBridge to expose a single `window.spaghetti` object that the
 * renderer can call. Every method is a thin `ipcRenderer.invoke` wrapper;
 * every on* method attaches a listener that returns an unsubscribe fn.
 */

import { contextBridge, ipcRenderer, type IpcRendererEvent } from 'electron';
import { EVENT_CHANNELS, IPC_CHANNELS, type SpaghettiBridge } from '../shared/ipc.js';

const bridge: SpaghettiBridge = {
  // Lifecycle ---------------------------------------------------------------
  isReady: () => ipcRenderer.invoke(IPC_CHANNELS.isReady),
  rebuildIndex: () => ipcRenderer.invoke(IPC_CHANNELS.rebuildIndex),
  retryInit: () => ipcRenderer.invoke(IPC_CHANNELS.retryInit),
  getEngine: () => ipcRenderer.invoke(IPC_CHANNELS.getEngine),

  // Projects ----------------------------------------------------------------
  getProjectList: () => ipcRenderer.invoke(IPC_CHANNELS.getProjectList),
  getProjectMemory: (projectSlug, options) => ipcRenderer.invoke(IPC_CHANNELS.getProjectMemory, projectSlug, options),

  // Sessions ----------------------------------------------------------------
  getSessionList: (projectSlug, options) => ipcRenderer.invoke(IPC_CHANNELS.getSessionList, projectSlug, options),
  getSessionMessages: (projectSlug, sessionId, limit, offset, options) =>
    ipcRenderer.invoke(IPC_CHANNELS.getSessionMessages, projectSlug, sessionId, limit, offset, options),
  getSessionTodos: (projectSlug, sessionId) => ipcRenderer.invoke(IPC_CHANNELS.getSessionTodos, projectSlug, sessionId),
  getSessionPlan: (projectSlug, sessionId) => ipcRenderer.invoke(IPC_CHANNELS.getSessionPlan, projectSlug, sessionId),
  getSessionTask: (projectSlug, sessionId) => ipcRenderer.invoke(IPC_CHANNELS.getSessionTask, projectSlug, sessionId),
  getToolResult: (projectSlug, sessionId, toolUseId) =>
    ipcRenderer.invoke(IPC_CHANNELS.getToolResult, projectSlug, sessionId, toolUseId),

  // Subagents ---------------------------------------------------------------
  getSessionSubagents: (projectSlug, sessionId) =>
    ipcRenderer.invoke(IPC_CHANNELS.getSessionSubagents, projectSlug, sessionId),
  getSubagentMessages: (projectSlug, sessionId, agentId, limit, offset) =>
    ipcRenderer.invoke(IPC_CHANNELS.getSubagentMessages, projectSlug, sessionId, agentId, limit, offset),

  // Search / stats ----------------------------------------------------------
  search: (query) => ipcRenderer.invoke(IPC_CHANNELS.search, query),
  getStats: () => ipcRenderer.invoke(IPC_CHANNELS.getStats),

  // Events ------------------------------------------------------------------
  onProgress: (cb) => {
    const handler = (_e: IpcRendererEvent, progress: unknown) => cb(progress as Parameters<typeof cb>[0]);
    ipcRenderer.on(EVENT_CHANNELS.progress, handler);
    return () => ipcRenderer.removeListener(EVENT_CHANNELS.progress, handler);
  },
  onReady: (cb) => {
    const handler = (_e: IpcRendererEvent, info: unknown) => cb(info as Parameters<typeof cb>[0]);
    ipcRenderer.on(EVENT_CHANNELS.ready, handler);
    return () => ipcRenderer.removeListener(EVENT_CHANNELS.ready, handler);
  },
  onChange: (cb) => {
    const handler = (_e: IpcRendererEvent, batch: unknown) => cb(batch as Parameters<typeof cb>[0]);
    ipcRenderer.on(EVENT_CHANNELS.change, handler);
    return () => ipcRenderer.removeListener(EVENT_CHANNELS.change, handler);
  },
  onInitError: (cb) => {
    const handler = (_e: IpcRendererEvent, message: unknown) => cb(String(message));
    ipcRenderer.on(EVENT_CHANNELS.initError, handler);
    return () => ipcRenderer.removeListener(EVENT_CHANNELS.initError, handler);
  },
};

contextBridge.exposeInMainWorld('spaghetti', bridge);

// ── Mille file explorer ────────────────────────────────────────────────────
// MessagePort cannot cross contextBridge — forward via window.postMessage
// with the port in the transfer list. Fires on every workspace open/swap.
// (preload has Node types only; cast for the DOM postMessage surface.)
ipcRenderer.on('fx-port', (event, payload: { workspaceRoot: string }) => {
  const win = globalThis as unknown as {
    postMessage: (message: unknown, targetOrigin: string, transfer?: unknown[]) => void;
  };
  win.postMessage(
    { type: 'fx-port', workspaceRoot: payload?.workspaceRoot ?? '' },
    '*',
    event.ports as unknown as unknown[],
  );
});

contextBridge.exposeInMainWorld('mille', {
  /** Open (or re-attach) the file explorer UtilityProcess against an absolute folder. */
  openWorkspace: (path: string): Promise<{ ok: true; root: string }> =>
    ipcRenderer.invoke('mille:open-workspace', path),
  /** Kill the UtilityProcess (panel closed). */
  closeWorkspace: (): Promise<{ ok: true }> => ipcRenderer.invoke('mille:close-workspace'),
});

contextBridge.exposeInMainWorld('windowControls', {
  minimize: (): Promise<void> => ipcRenderer.invoke('window:minimize'),
  maximize: (): Promise<void> => ipcRenderer.invoke('window:maximize'),
  close: (): Promise<void> => ipcRenderer.invoke('window:close'),
});

// Make the bridge type available globally for the renderer's consumers.
declare global {
  var spaghetti: SpaghettiBridge;
  interface Window {
    spaghetti: SpaghettiBridge;
    mille?: {
      openWorkspace(path: string): Promise<{ ok: true; root: string }>;
      closeWorkspace(): Promise<{ ok: true }>;
    };
    windowControls?: {
      minimize(): Promise<void>;
      maximize(): Promise<void>;
      close(): Promise<void>;
    };
  }
}

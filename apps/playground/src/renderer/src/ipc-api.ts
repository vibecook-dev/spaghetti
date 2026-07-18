/**
 * Adapter — wraps the asynchronous window.spaghetti bridge into an object
 * matching SpaghettiAPI, which the SpaghettiProvider consumes.
 *
 * The native SDK methods are synchronous (they return data directly). Over
 * IPC everything becomes a Promise; the renderer components already handle
 * that — ProjectCard et al treat method results as awaited by the hook layer
 * in AgentDataPlayground, which calls these inside try/catch. We cast the
 * returned shape to SpaghettiAPI so the provider is satisfied. Consumers of
 * the React components must tolerate Promise-returning variants (as they
 * already do via `await`).
 *
 * IMPORTANT: at runtime, each method returns `Promise<T>` rather than `T`.
 * AgentDataPlayground currently calls these synchronously; see README notes.
 * For now the provider surface is preserved; any future component that calls
 * these methods should `await` them.
 */

import type { SpaghettiAPI } from '@vibecook/spaghetti-sdk';
import type { SpaghettiBridge } from '@shared/ipc';

// Renderer-only: assert window.spaghetti exists at runtime.
function getBridge(): SpaghettiBridge {
  const bridge = (window as unknown as { spaghetti?: SpaghettiBridge }).spaghetti;
  if (!bridge) {
    throw new Error('window.spaghetti is not available — preload failed to load');
  }
  return bridge;
}

/**
 * Proxy object exposing the SpaghettiAPI surface. All query methods are
 * async-over-IPC; the types assert synchronous returns to match the
 * provider's contract — callers must `await` the returned values.
 */
export function createIpcApi(): SpaghettiAPI {
  const bridge = getBridge();

  const api = {
    initialize: async () => {
      /* main owns initialization */
    },

    shutdown: () => {
      /* main owns shutdown */
    },

    isReady: () => false as boolean,

    rebuildIndex: () => bridge.rebuildIndex() as unknown,

    getProjectList: () => bridge.getProjectList() as unknown,
    getSessionList: (projectSlug: string, options?: { sourceId?: string }) =>
      bridge.getSessionList(projectSlug, options) as unknown,
    getSessionMessages: (
      projectSlug: string,
      sessionId: string,
      limit?: number,
      offset?: number,
      options?: { sourceId?: string },
    ) => bridge.getSessionMessages(projectSlug, sessionId, limit, offset, options) as unknown,
    getSessionTimelineFacets: (projectSlug: string, sessionId: string, options?: { sourceId?: string }) =>
      bridge.getSessionTimelineFacets(projectSlug, sessionId, options) as unknown,
    getSessionTimeline: (
      projectSlug: string,
      sessionId: string,
      request?: Parameters<SpaghettiAPI['getSessionTimeline']>[2],
    ) => bridge.getSessionTimeline(projectSlug, sessionId, request) as unknown,
    getProjectMemory: (projectSlug: string, options?: { sourceId?: string }) =>
      bridge.getProjectMemory(projectSlug, options) as unknown,
    getSessionTodos: (projectSlug: string, sessionId: string) =>
      bridge.getSessionTodos(projectSlug, sessionId) as unknown,
    getSessionPlan: (projectSlug: string, sessionId: string) =>
      bridge.getSessionPlan(projectSlug, sessionId) as unknown,
    getSessionTask: (projectSlug: string, sessionId: string) =>
      bridge.getSessionTask(projectSlug, sessionId) as unknown,
    getToolResult: (projectSlug: string, sessionId: string, toolUseId: string) =>
      bridge.getToolResult(projectSlug, sessionId, toolUseId) as unknown,
    getSessionSubagents: (projectSlug: string, sessionId: string) =>
      bridge.getSessionSubagents(projectSlug, sessionId) as unknown,
    getSubagentMessages: (projectSlug: string, sessionId: string, agentId: string, limit?: number, offset?: number) =>
      bridge.getSubagentMessages(projectSlug, sessionId, agentId, limit, offset) as unknown,
    search: (query: unknown) => bridge.search(query as never) as unknown,
    getStats: () => bridge.getStats() as unknown,

    onProgress: (cb: Parameters<SpaghettiAPI['onProgress']>[0]) => bridge.onProgress(cb),
    onReady: (cb: Parameters<SpaghettiAPI['onReady']>[0]) => bridge.onReady(cb),
    onChange: (cb: Parameters<SpaghettiAPI['onChange']>[0]) => bridge.onChange(cb),
  };

  // Cast: runtime methods return Promise<T> but the SpaghettiAPI type declares
  // synchronous returns. Consumers built for the IPC adapter should await.
  return api as unknown as SpaghettiAPI;
}

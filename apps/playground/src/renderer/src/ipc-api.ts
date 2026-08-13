/**
 * Typed adapter from the context-isolated Electron bridge to the portable,
 * asynchronous React client. No synchronous compatibility cast is involved.
 */

import type { SpaghettiReactClient } from '@vibecook/spaghetti-sdk/react';
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
 * Proxy object exposing only the methods consumed by the React package.
 */
export function createIpcClient(): SpaghettiReactClient {
  const bridge = getBridge();
  return {
    isReady: () => bridge.isReady(),
    getProjectList: (options) => {
      if (options?.sourceId) {
        return bridge
          .getProjectList()
          .then((projects) => projects.filter((project) => project.sourceIds.includes(options.sourceId!)));
      }
      return bridge.getProjectList();
    },
    getSessionList: (project, options) => bridge.getSessionList(project, options),
    getSessionMessages: (projectSlug, sessionId, limit, offset, options) =>
      bridge.getSessionMessages(projectSlug, sessionId, limit, offset, options),
    getProjectMemory: (project, options) => bridge.getProjectMemory(project, options),
    getToolResult: (projectSlug, sessionId, toolUseId) => bridge.getToolResult(projectSlug, sessionId, toolUseId),
    getSessionSubagents: (projectSlug, sessionId, options) =>
      bridge.getSessionSubagents(projectSlug, sessionId, options),
    getSubagentMessages: (projectSlug, sessionId, agentId, limit, offset, workflowId, options) =>
      bridge.getSubagentMessages(projectSlug, sessionId, agentId, limit, offset, workflowId, options),
    search: (query) => bridge.search(query),
    getStats: () => bridge.getStats(),
    onProgress: (cb) => bridge.onProgress(cb),
    onReady: (cb) => bridge.onReady(cb),
    onChange: (cb) => bridge.onChange(cb),
  };
}

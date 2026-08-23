/** UtilityProcess entrypoint: typed RPC host for the entire Spaghetti SDK. */

import type { MessagePortMain } from 'electron';
import type { SdkHostCommand, SdkHostEvent, SdkHostMessage, SdkRpcRequest } from '../shared/sdk-protocol.js';
import { serializeError } from '../shared/sdk-protocol.js';
import { SdkRuntime, type SdkRuntimeOptions } from './sdk-runtime.js';

const parentPort = process.parentPort;
let shuttingDown = false;

function post(message: SdkHostMessage): void {
  parentPort.postMessage(message);
}

function event(data: SdkHostEvent): void {
  post({ type: 'event', data });
}

function readConfig(): SdkRuntimeOptions {
  const dbPath = process.env.SPAGHETTI_DB_PATH;
  if (!dbPath) throw new Error('SPAGHETTI_DB_PATH is required');
  const rootDir = process.env.SPAGHETTI_ROOT_DIR;
  const rawDetectAdditional = process.env.SPAGHETTI_DETECT_ADDITIONAL_SOURCES;
  if (rawDetectAdditional && !['0', '1', 'false', 'true'].includes(rawDetectAdditional.toLowerCase())) {
    throw new Error('SPAGHETTI_DETECT_ADDITIONAL_SOURCES must be 0, 1, false, or true');
  }
  const detectAdditionalSources =
    rawDetectAdditional === undefined || rawDetectAdditional === '1' || rawDetectAdditional.toLowerCase() === 'true';
  return {
    dbPath,
    ...(rootDir ? { rootDir } : {}),
    ...(!detectAdditionalSources ? { additionalSources: [] } : {}),
  };
}

const runtime = new SdkRuntime(readConfig(), {
  progress: (payload) => event({ event: 'progress', payload }),
  ready: (payload) => event({ event: 'ready', payload }),
  change: (payload) => event({ event: 'change', payload }),
  activeSessionChange: (payload) => event({ event: 'active-session-change', payload }),
  initError: (payload) => event({ event: 'init-error', payload }),
});

async function dispatch(request: SdkRpcRequest): Promise<unknown> {
  switch (request.method) {
    case 'isReady':
      return runtime.isReady();
    case 'rebuildIndex':
      return runtime.rebuildIndex();
    case 'retryInit':
      await runtime.retry();
      return { ok: true as const };
    case 'getEngine':
      return runtime.engine;
    case 'getObservationHostStatus':
      return runtime.getObservationHostStatus();
    case 'getObservationOwnerStatus':
      return runtime.getObservationOwnerStatus();
    case 'getReadiness':
      return runtime.read((sdk) => sdk.getReadiness());
    case 'listCatalogProjects': {
      const [options] = request.args;
      return runtime.read((sdk) => sdk.listCatalogProjects(options));
    }
    case 'getProjectList':
      return runtime.read((sdk) => sdk.getProjectList());
    case 'getProjectTokenActivity': {
      const [project, query] = request.args;
      return runtime.read((sdk) => sdk.getProjectTokenActivity(project, query));
    }
    case 'getProjectMemory': {
      const [project, options] = request.args;
      return runtime.read((sdk) => sdk.getProjectMemory(project, options));
    }
    case 'listCatalogSessions': {
      const [options] = request.args;
      return runtime.read((sdk) => sdk.listCatalogSessions(options));
    }
    case 'getSessionList': {
      const [project, options] = request.args;
      return runtime.read((sdk) => sdk.getSessionList(project, options));
    }
    case 'getSessionMessages': {
      const [projectSlug, sessionId, limit, offset, options] = request.args;
      return runtime.read((sdk) => sdk.getSessionMessages(projectSlug, sessionId, limit, offset, options));
    }
    case 'getSessionTimelineFacets': {
      const [projectSlug, sessionId, options] = request.args;
      return runtime.read((sdk) => sdk.getSessionTimelineFacets(projectSlug, sessionId, options));
    }
    case 'getSessionTimeline': {
      const [projectSlug, sessionId, options] = request.args;
      return runtime.read((sdk) => sdk.getSessionTimeline(projectSlug, sessionId, options));
    }
    case 'openSessionStream': {
      const [projectSlug, sessionId, options] = request.args;
      return runtime.openSessionStream(projectSlug, sessionId, options);
    }
    case 'closeSessionStream': {
      const [streamId] = request.args;
      runtime.closeSessionStream(streamId);
      return { ok: true as const };
    }
    case 'getSessionTodos': {
      const [projectSlug, sessionId] = request.args;
      return runtime.read((sdk) => sdk.getSessionTodos(projectSlug, sessionId));
    }
    case 'getSessionPlan': {
      const [projectSlug, sessionId] = request.args;
      return runtime.read((sdk) => sdk.getSessionPlan(projectSlug, sessionId));
    }
    case 'getSessionTask': {
      const [projectSlug, sessionId] = request.args;
      return runtime.read((sdk) => sdk.getSessionTask(projectSlug, sessionId));
    }
    case 'getToolResult': {
      const [projectSlug, sessionId, toolUseId] = request.args;
      return runtime.read((sdk) => sdk.getToolResult(projectSlug, sessionId, toolUseId));
    }
    case 'getSessionSubagents': {
      const [projectSlug, sessionId, options] = request.args;
      return runtime.read((sdk) => sdk.getSessionSubagents(projectSlug, sessionId, options));
    }
    case 'getSubagentMessages': {
      const [projectSlug, sessionId, agentId, limit, offset, workflowId, options] = request.args;
      return runtime.read((sdk) =>
        sdk.getSubagentMessages(projectSlug, sessionId, agentId, limit, offset, workflowId, options),
      );
    }
    case 'getSubagentTimeline': {
      const [projectSlug, sessionId, agentId, options] = request.args;
      return runtime.read((sdk) => sdk.getSubagentTimeline(projectSlug, sessionId, agentId, options));
    }
    case 'search': {
      const [query] = request.args;
      return runtime.read((sdk) => sdk.search(query));
    }
    case 'getStats':
      return runtime.read((sdk) => sdk.getStats());
  }
  const exhaustive: never = request;
  return exhaustive;
}

async function handleCommand(command: SdkHostCommand, port?: MessagePortMain): Promise<void> {
  if (command.type === 'shutdown') {
    if (shuttingDown) return;
    shuttingDown = true;
    await runtime.dispose();
    post({ type: 'shutdown-complete', id: command.id });
    setImmediate(() => process.exit(0));
    return;
  }

  if (shuttingDown) {
    post({
      type: 'response',
      id: command.id,
      ok: false,
      error: serializeError(new Error('SDK utility is shutting down')),
    });
    return;
  }

  try {
    if (command.type === 'diagnostics') {
      post({
        type: 'response',
        id: command.id,
        ok: true,
        result: {
          pid: process.pid,
          uptimeSeconds: process.uptime(),
          memory: process.memoryUsage(),
        },
      });
      return;
    }
    if (command.type === 'attach-spaghetti-client') {
      if (!port) throw new Error('attach-spaghetti-client requires one transferred MessagePort.');
      await runtime.attachObservationClient(port);
      post({ type: 'response', id: command.id, ok: true, result: { ok: true } });
      return;
    }
    const result = await dispatch(command);
    post({ type: 'response', id: command.id, ok: true, result });
  } catch (error) {
    post({ type: 'response', id: command.id, ok: false, error: serializeError(error) });
  }
}

parentPort.on('message', (message) => {
  void handleCommand(message.data as SdkHostCommand, message.ports[0]);
});

process.on('uncaughtException', (error) => {
  post({ type: 'fatal', error: serializeError(error) });
  process.exit(1);
});

process.on('unhandledRejection', (error) => {
  post({ type: 'fatal', error: serializeError(error) });
  process.exit(1);
});

// Confirm the RPC transport before starting ingestion. Native cold ingest can
// run synchronously for a while; it belongs here, but must not make Electron
// main mistake a busy utility for a failed fork.
post({ type: 'host-ready' });
setImmediate(() => {
  try {
    runtime.start();
  } catch (error) {
    post({ type: 'fatal', error: serializeError(error) });
    process.exit(1);
  }
});

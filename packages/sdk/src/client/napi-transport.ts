import { openSpaghettiEngine, type SpaghettiEngine, type SpaghettiEngineOpenOptions } from '../native.js';
import {
  SPAGHETTI_CLIENT_METHODS,
  SPAGHETTI_CLIENT_PROTOCOL_VERSION,
  SPAGHETTI_MAX_REQUEST_BYTES,
  SPAGHETTI_QUERY_CONTRACT_VERSION,
  type AnySpaghettiProtocolRequest,
  type SpaghettiClientMethod,
  type SpaghettiClientResponseMap,
  type SpaghettiClientTransport,
  type SpaghettiProtocolRequest,
  type SpaghettiProtocolResponse,
  type SpaghettiTransportConnectRequest,
  type SpaghettiTransportConnectResponse,
  type SpaghettiTransportRequestOptions,
} from './protocol.js';
import {
  clientError,
  cancelledProtocolError,
  closedProtocolError,
  normalizeTransportError,
  protocolMismatchError,
} from './errors.js';
import { encodedRequestBytes } from './encoding.js';

export interface NapiTransportOptions {
  engine: SpaghettiEngine;
  /** Dispose the engine with the transport. Defaults to true. */
  ownsEngine?: boolean;
  engineVersion?: string;
}

export interface OpenNapiTransportOptions extends SpaghettiEngineOpenOptions {
  /** Start the native Claude observer before returning the connected transport. */
  observation?: {
    roots: string[];
    reason?: string;
  };
}

/** Embedded transport: one logical request maps to one async N-API call. */
export class NapiTransport implements SpaghettiClientTransport {
  readonly kind = 'napi';
  private readonly engine: SpaghettiEngine;
  private readonly ownsEngine: boolean;
  private readonly engineVersion: string;
  private readonly activeRequestIds = new Set<number>();
  private connected = false;
  private closed = false;
  private disposePromise: Promise<void> | undefined;

  constructor(options: NapiTransportOptions) {
    this.engine = options.engine;
    this.ownsEngine = options.ownsEngine ?? true;
    this.engineVersion = options.engineVersion ?? options.engine.status.owner?.engineVersion ?? 'unknown';
  }

  async connect(
    request: SpaghettiTransportConnectRequest,
    options?: SpaghettiTransportRequestOptions,
  ): Promise<SpaghettiTransportConnectResponse> {
    if (this.closed) throw clientError(closedProtocolError());
    if (options?.signal?.aborted) throw clientError(cancelledProtocolError());
    if (!request.clientName.trim()) {
      throw clientError({ code: 'invalid_request', message: 'clientName must not be blank.', field: 'clientName' });
    }
    if (!request.protocolVersions.includes(SPAGHETTI_CLIENT_PROTOCOL_VERSION)) {
      throw clientError(
        protocolMismatchError(
          `transport supports protocol ${SPAGHETTI_CLIENT_PROTOCOL_VERSION}; client offered ${request.protocolVersions.join(',')}`,
        ),
      );
    }
    if (!request.queryContractVersions.includes(SPAGHETTI_QUERY_CONTRACT_VERSION)) {
      throw clientError(
        protocolMismatchError(
          `transport supports query contract ${SPAGHETTI_QUERY_CONTRACT_VERSION}; client offered ${request.queryContractVersions.join(',')}`,
        ),
      );
    }
    this.connected = true;
    return {
      transportKind: this.kind,
      protocolVersion: SPAGHETTI_CLIENT_PROTOCOL_VERSION,
      queryContractVersion: SPAGHETTI_QUERY_CONTRACT_VERSION,
      engineVersion: this.engineVersion,
      methods: SPAGHETTI_CLIENT_METHODS,
    };
  }

  async request<M extends SpaghettiClientMethod>(
    request: SpaghettiProtocolRequest<M>,
    options?: SpaghettiTransportRequestOptions,
  ): Promise<SpaghettiProtocolResponse<M>> {
    const responseBase = {
      protocolVersion: SPAGHETTI_CLIENT_PROTOCOL_VERSION,
      queryContractVersion: SPAGHETTI_QUERY_CONTRACT_VERSION,
      requestId: request.requestId,
    };
    const failure = (error: ReturnType<typeof normalizeTransportError>): SpaghettiProtocolResponse<M> => ({
      ...responseBase,
      ok: false,
      error,
    });

    if (this.closed) return failure(closedProtocolError());
    if (!this.connected) return failure(protocolMismatchError('connect must succeed before the first request'));
    if (
      request.protocolVersion !== SPAGHETTI_CLIENT_PROTOCOL_VERSION ||
      request.queryContractVersion !== SPAGHETTI_QUERY_CONTRACT_VERSION
    ) {
      return failure(protocolMismatchError('request envelope uses a protocol contract that was not negotiated'));
    }
    if (!Number.isSafeInteger(request.requestId) || request.requestId < 1) {
      return failure({
        code: 'invalid_request',
        message: 'requestId must be a positive safe integer.',
        field: 'requestId',
      });
    }
    if (!SPAGHETTI_CLIENT_METHODS.includes(request.method)) {
      return failure({
        code: 'unsupported_capability',
        message: 'The requested query method is not supported.',
        capability: request.method,
      });
    }
    if (this.activeRequestIds.has(request.requestId)) {
      return failure({
        code: 'invalid_request',
        message: 'requestId is already in flight.',
        field: 'requestId',
        reason: 'duplicate',
      });
    }
    if (encodedRequestBytes(request.payload) > SPAGHETTI_MAX_REQUEST_BYTES) {
      return failure({
        code: 'invalid_request',
        message: `The query request exceeds the ${SPAGHETTI_MAX_REQUEST_BYTES}-byte transport limit.`,
        field: 'payload',
        reason: 'payload_too_large',
      });
    }
    if (options?.signal?.aborted) return failure(cancelledProtocolError());

    this.activeRequestIds.add(request.requestId);
    try {
      const result = await executeEngineRequest(this.engine, request as AnySpaghettiProtocolRequest, options?.signal);
      return { ...responseBase, ok: true, result: result as SpaghettiClientResponseMap[M] };
    } catch (error) {
      if (options?.signal?.aborted) return failure(cancelledProtocolError());
      return failure(normalizeTransportError(error, `napi-${request.requestId}`));
    } finally {
      this.activeRequestIds.delete(request.requestId);
    }
  }

  dispose(): Promise<void> {
    if (this.disposePromise) return this.disposePromise;
    this.closed = true;
    this.connected = false;
    this.disposePromise = (async () => {
      if (this.ownsEngine) {
        this.engine.cancelPendingQueries();
        await this.engine.dispose();
      }
    })();
    return this.disposePromise;
  }
}

/** Open and optionally start the embedded Rust owner before transport negotiation. */
export async function openNapiTransport(options: OpenNapiTransportOptions): Promise<NapiTransport> {
  const { observation, ...engineOptions } = options;
  const engine = await openSpaghettiEngine(engineOptions);
  try {
    if (observation) await engine.startClaudeObservation(observation);
    return new NapiTransport({ engine, ownsEngine: true });
  } catch (error) {
    await engine.dispose();
    throw error;
  }
}

async function executeEngineRequest(
  engine: SpaghettiEngine,
  request: AnySpaghettiProtocolRequest,
  signal?: AbortSignal,
): Promise<unknown> {
  switch (request.method) {
    case 'getHealth':
      return engine.health(signal);
    case 'getOverview':
      return engine.overview(signal);
    case 'replayChanges':
      return engine.replayChanges(request.payload, signal);
    case 'listProjects':
      return engine.listHistoryProjects(request.payload, signal);
    case 'listSessions':
      return engine.listHistorySessions(request.payload, signal);
    case 'getSession':
      return engine.getSession(request.payload.sessionId, signal);
    case 'getMessages':
      return engine.getMessages(request.payload, signal);
    case 'search':
      return engine.search(request.payload, signal);
    case 'getTimeline':
      return engine.getTimeline(request.payload, signal);
    case 'listDelegations':
      return engine.listDelegations(request.payload, signal);
    case 'listWorkflows':
      return engine.listWorkflows(request.payload, signal);
    case 'getWorkflow':
      return engine.getWorkflow(request.payload.workflowId, signal);
    case 'listWorkflowMembers':
      return engine.listWorkflowMembers(request.payload, signal);
    case 'listMemoryDocuments':
      return engine.listMemoryDocuments(request.payload, signal);
    case 'listTaskCollections':
      return engine.listTaskCollections(request.payload, signal);
    case 'listTasks':
      return engine.listTasks(request.payload, signal);
    case 'listPlans':
      return engine.listPlans(request.payload, signal);
    case 'listToolResults':
      return engine.listToolResults(request.payload, signal);
    case 'listArtifacts':
      return engine.listArtifacts(request.payload, signal);
    case 'listSources':
      return engine.listSources(request.payload, signal);
    case 'getStats':
      return engine.getStats(signal);
    case 'getUsage':
      return engine.getUsage(request.payload, signal);
    case 'getUsageActivity':
      return engine.getUsageActivity(request.payload, signal);
    case 'getRuntimeSnapshot':
      return engine.getRuntimeSnapshot(request.payload, signal);
    case 'getRunState':
      return engine.getRunState(request.payload.runId, signal);
    case 'listTeams':
      return engine.listTeams(request.payload, signal);
    case 'getTeam':
      return engine.getTeam(request.payload.teamId, signal);
    case 'listTeamInboxes':
      return engine.listTeamInboxes(request.payload, signal);
    case 'listTeamInboxMessages':
      return engine.listTeamInboxMessages(request.payload, signal);
  }
}

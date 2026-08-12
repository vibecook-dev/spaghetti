/**
 * Transport-neutral RFC 011 query protocol.
 *
 * The current DTO implementations originate in the native engine module, but
 * callers depend on this semantic method map rather than on `SpaghettiEngine`.
 * An IPC transport can therefore reuse the exact same requests and results.
 */

import type { SpaghettiEngine } from '../native.js';

export const SPAGHETTI_CLIENT_PROTOCOL_VERSION = 1 as const;
export const SPAGHETTI_QUERY_CONTRACT_VERSION = 1 as const;
export const SPAGHETTI_MAX_REQUEST_BYTES = 64 * 1024;

type EngineRequest<M extends keyof SpaghettiEngine> = SpaghettiEngine[M] extends (...args: infer A) => unknown
  ? A[0]
  : never;
type EngineResult<M extends keyof SpaghettiEngine> = SpaghettiEngine[M] extends (...args: never[]) => infer R
  ? Awaited<R>
  : never;

export interface GetSessionRequest {
  sessionId: string;
}

export interface GetWorkflowRequest {
  workflowId: string;
}

export interface GetRunStateRequest {
  runId: string;
}

export interface GetTeamRequest {
  teamId: string;
}

/** One request DTO for every canonical read operation. */
export interface SpaghettiClientRequestMap {
  getHealth: undefined;
  getOverview: undefined;
  listProjects: EngineRequest<'listHistoryProjects'>;
  listSessions: EngineRequest<'listHistorySessions'>;
  getSession: GetSessionRequest;
  getMessages: EngineRequest<'getMessages'>;
  search: EngineRequest<'search'>;
  getTimeline: EngineRequest<'getTimeline'>;
  listDelegations: EngineRequest<'listDelegations'>;
  listWorkflows: EngineRequest<'listWorkflows'>;
  getWorkflow: GetWorkflowRequest;
  listWorkflowMembers: EngineRequest<'listWorkflowMembers'>;
  listMemoryDocuments: EngineRequest<'listMemoryDocuments'>;
  listTaskCollections: EngineRequest<'listTaskCollections'>;
  listTasks: EngineRequest<'listTasks'>;
  listPlans: EngineRequest<'listPlans'>;
  listToolResults: EngineRequest<'listToolResults'>;
  listArtifacts: EngineRequest<'listArtifacts'>;
  listSources: EngineRequest<'listSources'>;
  getStats: undefined;
  getUsage: EngineRequest<'getUsage'>;
  getUsageActivity: EngineRequest<'getUsageActivity'>;
  getRuntimeSnapshot: EngineRequest<'getRuntimeSnapshot'>;
  getRunState: GetRunStateRequest;
  listTeams: EngineRequest<'listTeams'>;
  getTeam: GetTeamRequest;
  listTeamInboxes: EngineRequest<'listTeamInboxes'>;
  listTeamInboxMessages: EngineRequest<'listTeamInboxMessages'>;
}

/** One response DTO for every canonical read operation. */
export interface SpaghettiClientResponseMap {
  getHealth: EngineResult<'health'>;
  getOverview: EngineResult<'overview'>;
  listProjects: EngineResult<'listHistoryProjects'>;
  listSessions: EngineResult<'listHistorySessions'>;
  getSession: EngineResult<'getSession'>;
  getMessages: EngineResult<'getMessages'>;
  search: EngineResult<'search'>;
  getTimeline: EngineResult<'getTimeline'>;
  listDelegations: EngineResult<'listDelegations'>;
  listWorkflows: EngineResult<'listWorkflows'>;
  getWorkflow: EngineResult<'getWorkflow'>;
  listWorkflowMembers: EngineResult<'listWorkflowMembers'>;
  listMemoryDocuments: EngineResult<'listMemoryDocuments'>;
  listTaskCollections: EngineResult<'listTaskCollections'>;
  listTasks: EngineResult<'listTasks'>;
  listPlans: EngineResult<'listPlans'>;
  listToolResults: EngineResult<'listToolResults'>;
  listArtifacts: EngineResult<'listArtifacts'>;
  listSources: EngineResult<'listSources'>;
  getStats: EngineResult<'getStats'>;
  getUsage: EngineResult<'getUsage'>;
  getUsageActivity: EngineResult<'getUsageActivity'>;
  getRuntimeSnapshot: EngineResult<'getRuntimeSnapshot'>;
  getRunState: EngineResult<'getRunState'>;
  listTeams: EngineResult<'listTeams'>;
  getTeam: EngineResult<'getTeam'>;
  listTeamInboxes: EngineResult<'listTeamInboxes'>;
  listTeamInboxMessages: EngineResult<'listTeamInboxMessages'>;
}

export type SpaghettiClientMethod = keyof SpaghettiClientRequestMap & keyof SpaghettiClientResponseMap;

export const SPAGHETTI_CLIENT_METHODS = completeMethodList([
  'getHealth',
  'getOverview',
  'listProjects',
  'listSessions',
  'getSession',
  'getMessages',
  'search',
  'getTimeline',
  'listDelegations',
  'listWorkflows',
  'getWorkflow',
  'listWorkflowMembers',
  'listMemoryDocuments',
  'listTaskCollections',
  'listTasks',
  'listPlans',
  'listToolResults',
  'listArtifacts',
  'listSources',
  'getStats',
  'getUsage',
  'getUsageActivity',
  'getRuntimeSnapshot',
  'getRunState',
  'listTeams',
  'getTeam',
  'listTeamInboxes',
  'listTeamInboxMessages',
] as const);

export type SpaghettiClientErrorCode =
  | 'invalid_request'
  | 'unsupported_capability'
  | 'projection_pending'
  | 'cursor_invalid'
  | 'cancelled'
  | 'deadline_exceeded'
  | 'engine_stopping'
  | 'database_busy'
  | 'transport_unavailable'
  | 'protocol_mismatch'
  | 'transport_closed'
  | 'internal';

export interface SpaghettiProtocolError {
  code: SpaghettiClientErrorCode;
  message: string;
  field?: string;
  reason?: string;
  capability?: string;
  projection?: string;
  retryAfterMs?: number;
  diagnosticId?: string;
}

export class SpaghettiClientError extends Error {
  readonly code: SpaghettiClientErrorCode;
  readonly requestId?: number;
  readonly field?: string;
  readonly reason?: string;
  readonly capability?: string;
  readonly projection?: string;
  readonly retryAfterMs?: number;
  readonly diagnosticId?: string;

  constructor(error: SpaghettiProtocolError, requestId?: number) {
    super(error.message);
    this.name = 'SpaghettiClientError';
    this.code = error.code;
    this.requestId = requestId;
    this.field = error.field;
    this.reason = error.reason;
    this.capability = error.capability;
    this.projection = error.projection;
    this.retryAfterMs = error.retryAfterMs;
    this.diagnosticId = error.diagnosticId;
  }
}

export interface SpaghettiTransportConnectRequest {
  clientName: string;
  protocolVersions: readonly number[];
  queryContractVersions: readonly number[];
}

export interface SpaghettiTransportConnectResponse {
  transportKind: string;
  protocolVersion: number;
  queryContractVersion: number;
  engineVersion: string;
  methods: readonly SpaghettiClientMethod[];
}

export interface SpaghettiProtocolRequest<M extends SpaghettiClientMethod = SpaghettiClientMethod> {
  protocolVersion: number;
  queryContractVersion: number;
  requestId: number;
  method: M;
  payload: SpaghettiClientRequestMap[M];
}

export type AnySpaghettiProtocolRequest = {
  [M in SpaghettiClientMethod]: SpaghettiProtocolRequest<M>;
}[SpaghettiClientMethod];

export type SpaghettiProtocolResponse<M extends SpaghettiClientMethod = SpaghettiClientMethod> =
  | {
      protocolVersion: number;
      queryContractVersion: number;
      requestId: number;
      ok: true;
      result: SpaghettiClientResponseMap[M];
    }
  | {
      protocolVersion: number;
      queryContractVersion: number;
      requestId: number;
      ok: false;
      error: SpaghettiProtocolError;
    };

export interface SpaghettiTransportRequestOptions {
  signal?: AbortSignal;
}

/** Implemented by both the embedded N-API path and the future IPC path. */
export interface SpaghettiClientTransport {
  readonly kind: string;
  connect(
    request: SpaghettiTransportConnectRequest,
    options?: SpaghettiTransportRequestOptions,
  ): Promise<SpaghettiTransportConnectResponse>;
  request<M extends SpaghettiClientMethod>(
    request: SpaghettiProtocolRequest<M>,
    options?: SpaghettiTransportRequestOptions,
  ): Promise<SpaghettiProtocolResponse<M>>;
  dispose(): Promise<void>;
}

export type SpaghettiClientInfo = SpaghettiTransportConnectResponse;

export interface SpaghettiQueryOptions {
  signal?: AbortSignal;
  /**
   * Cancels and suppresses the prior in-flight request with the same key.
   * Useful for search-as-you-type and rapidly changing visible scopes.
   */
  supersessionKey?: string;
}

/** The new asynchronous, transport-neutral canonical query surface. */
export interface SpaghettiClient {
  readonly info: SpaghettiClientInfo;
  getHealth(options?: SpaghettiQueryOptions): Promise<SpaghettiClientResponseMap['getHealth']>;
  getOverview(options?: SpaghettiQueryOptions): Promise<SpaghettiClientResponseMap['getOverview']>;
  listProjects(
    request?: Exclude<SpaghettiClientRequestMap['listProjects'], undefined>,
    options?: SpaghettiQueryOptions,
  ): Promise<SpaghettiClientResponseMap['listProjects']>;
  listSessions(
    request: SpaghettiClientRequestMap['listSessions'],
    options?: SpaghettiQueryOptions,
  ): Promise<SpaghettiClientResponseMap['listSessions']>;
  getSession(
    request: SpaghettiClientRequestMap['getSession'],
    options?: SpaghettiQueryOptions,
  ): Promise<SpaghettiClientResponseMap['getSession']>;
  getMessages(
    request: SpaghettiClientRequestMap['getMessages'],
    options?: SpaghettiQueryOptions,
  ): Promise<SpaghettiClientResponseMap['getMessages']>;
  search(
    request: SpaghettiClientRequestMap['search'],
    options?: SpaghettiQueryOptions,
  ): Promise<SpaghettiClientResponseMap['search']>;
  getTimeline(
    request: SpaghettiClientRequestMap['getTimeline'],
    options?: SpaghettiQueryOptions,
  ): Promise<SpaghettiClientResponseMap['getTimeline']>;
  listDelegations(
    request: SpaghettiClientRequestMap['listDelegations'],
    options?: SpaghettiQueryOptions,
  ): Promise<SpaghettiClientResponseMap['listDelegations']>;
  listWorkflows(
    request: SpaghettiClientRequestMap['listWorkflows'],
    options?: SpaghettiQueryOptions,
  ): Promise<SpaghettiClientResponseMap['listWorkflows']>;
  getWorkflow(
    request: SpaghettiClientRequestMap['getWorkflow'],
    options?: SpaghettiQueryOptions,
  ): Promise<SpaghettiClientResponseMap['getWorkflow']>;
  listWorkflowMembers(
    request: SpaghettiClientRequestMap['listWorkflowMembers'],
    options?: SpaghettiQueryOptions,
  ): Promise<SpaghettiClientResponseMap['listWorkflowMembers']>;
  listMemoryDocuments(
    request: SpaghettiClientRequestMap['listMemoryDocuments'],
    options?: SpaghettiQueryOptions,
  ): Promise<SpaghettiClientResponseMap['listMemoryDocuments']>;
  listTaskCollections(
    request?: Exclude<SpaghettiClientRequestMap['listTaskCollections'], undefined>,
    options?: SpaghettiQueryOptions,
  ): Promise<SpaghettiClientResponseMap['listTaskCollections']>;
  listTasks(
    request: SpaghettiClientRequestMap['listTasks'],
    options?: SpaghettiQueryOptions,
  ): Promise<SpaghettiClientResponseMap['listTasks']>;
  listPlans(
    request?: Exclude<SpaghettiClientRequestMap['listPlans'], undefined>,
    options?: SpaghettiQueryOptions,
  ): Promise<SpaghettiClientResponseMap['listPlans']>;
  listToolResults(
    request: SpaghettiClientRequestMap['listToolResults'],
    options?: SpaghettiQueryOptions,
  ): Promise<SpaghettiClientResponseMap['listToolResults']>;
  listArtifacts(
    request: SpaghettiClientRequestMap['listArtifacts'],
    options?: SpaghettiQueryOptions,
  ): Promise<SpaghettiClientResponseMap['listArtifacts']>;
  listSources(
    request?: Exclude<SpaghettiClientRequestMap['listSources'], undefined>,
    options?: SpaghettiQueryOptions,
  ): Promise<SpaghettiClientResponseMap['listSources']>;
  getStats(options?: SpaghettiQueryOptions): Promise<SpaghettiClientResponseMap['getStats']>;
  getUsage(
    request: SpaghettiClientRequestMap['getUsage'],
    options?: SpaghettiQueryOptions,
  ): Promise<SpaghettiClientResponseMap['getUsage']>;
  getUsageActivity(
    request: SpaghettiClientRequestMap['getUsageActivity'],
    options?: SpaghettiQueryOptions,
  ): Promise<SpaghettiClientResponseMap['getUsageActivity']>;
  getRuntimeSnapshot(
    request?: Exclude<SpaghettiClientRequestMap['getRuntimeSnapshot'], undefined>,
    options?: SpaghettiQueryOptions,
  ): Promise<SpaghettiClientResponseMap['getRuntimeSnapshot']>;
  getRunState(
    request: SpaghettiClientRequestMap['getRunState'],
    options?: SpaghettiQueryOptions,
  ): Promise<SpaghettiClientResponseMap['getRunState']>;
  listTeams(
    request?: Exclude<SpaghettiClientRequestMap['listTeams'], undefined>,
    options?: SpaghettiQueryOptions,
  ): Promise<SpaghettiClientResponseMap['listTeams']>;
  getTeam(
    request: SpaghettiClientRequestMap['getTeam'],
    options?: SpaghettiQueryOptions,
  ): Promise<SpaghettiClientResponseMap['getTeam']>;
  listTeamInboxes(
    request: SpaghettiClientRequestMap['listTeamInboxes'],
    options?: SpaghettiQueryOptions,
  ): Promise<SpaghettiClientResponseMap['listTeamInboxes']>;
  listTeamInboxMessages(
    request: SpaghettiClientRequestMap['listTeamInboxMessages'],
    options?: SpaghettiQueryOptions,
  ): Promise<SpaghettiClientResponseMap['listTeamInboxMessages']>;
  dispose(): Promise<void>;
}

function completeMethodList<const T extends readonly SpaghettiClientMethod[]>(
  methods: T & (Exclude<SpaghettiClientMethod, T[number]> extends never ? unknown : never),
): T {
  return methods;
}

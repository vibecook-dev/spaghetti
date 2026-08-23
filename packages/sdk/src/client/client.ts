import {
  SPAGHETTI_CLIENT_METHODS,
  SPAGHETTI_CLIENT_PROTOCOL_VERSION,
  SPAGHETTI_QUERY_CONTRACT_VERSION,
  SpaghettiClientError,
  type SpaghettiClient,
  type SpaghettiClientInfo,
  type SpaghettiClientMethod,
  type SpaghettiClientRequestMap,
  type SpaghettiClientResponseMap,
  type SpaghettiClientTransport,
  type SpaghettiCommittedChangeBatch,
  type SpaghettiQueryOptions,
  type SpaghettiSubscribeOptions,
  type SpaghettiSubscribeRequest,
  type SpaghettiSubscriptionMetrics,
} from './protocol.js';
import {
  cancelledProtocolError,
  clientError,
  closedProtocolError,
  normalizeTransportError,
  protocolMismatchError,
} from './errors.js';

export interface OpenSpaghettiClientOptions {
  transport: SpaghettiClientTransport;
  clientName?: string;
  protocolVersions?: readonly number[];
  queryContractVersions?: readonly number[];
}

interface SupersessionEntry {
  requestId: number;
  controller: AbortController;
}

export const SPAGHETTI_SUBSCRIPTION_WAKE_TIMEOUT_MS = 30_000;
export const SPAGHETTI_SUBSCRIPTION_MAX_WAKE_TIMEOUT_MS = 300_000;

const MAX_CHANGE_ORDINAL = 0xffff_ffff;

class DefaultSpaghettiClient implements SpaghettiClient {
  readonly info: SpaghettiClientInfo;
  private nextRequestId = 1;
  private closed = false;
  private disposePromise: Promise<void> | undefined;
  private readonly supersession = new Map<string, SupersessionEntry>();
  private readonly inFlight = new Map<number, AbortController>();
  private readonly subscriptions = new Set<AbortController>();
  private readonly subscriptionMetrics: SpaghettiSubscriptionMetrics = {
    activeSubscriptions: 0,
    replayRequests: 0,
    replayPayloadBytes: 0,
    deliveredBatches: 0,
    deliveredChanges: 0,
    waitRequests: 0,
    commitWakeups: 0,
    waitTimeouts: 0,
    cancellations: 0,
    maxObservedLagCommits: 0,
  };

  constructor(
    private readonly transport: SpaghettiClientTransport,
    info: SpaghettiClientInfo,
  ) {
    this.info = Object.freeze({ ...info, methods: Object.freeze([...info.methods]) });
  }

  getHealth(options?: SpaghettiQueryOptions): Promise<SpaghettiClientResponseMap['getHealth']> {
    return this.query('getHealth', undefined, options);
  }

  getOverview(options?: SpaghettiQueryOptions): Promise<SpaghettiClientResponseMap['getOverview']> {
    return this.query('getOverview', undefined, options);
  }

  getReadiness(options?: SpaghettiQueryOptions): Promise<SpaghettiClientResponseMap['getReadiness']> {
    return this.query('getReadiness', undefined, options);
  }

  resolveCatalogEntity(
    request: SpaghettiClientRequestMap['resolveCatalogEntity'],
    options?: SpaghettiQueryOptions,
  ): Promise<SpaghettiClientResponseMap['resolveCatalogEntity']> {
    return this.query('resolveCatalogEntity', request, options);
  }

  listCatalogProjects(
    request?: Exclude<SpaghettiClientRequestMap['listCatalogProjects'], undefined>,
    options?: SpaghettiQueryOptions,
  ): Promise<SpaghettiClientResponseMap['listCatalogProjects']> {
    return this.query('listCatalogProjects', request, options);
  }

  listCatalogSessions(
    request?: Exclude<SpaghettiClientRequestMap['listCatalogSessions'], undefined>,
    options?: SpaghettiQueryOptions,
  ): Promise<SpaghettiClientResponseMap['listCatalogSessions']> {
    return this.query('listCatalogSessions', request, options);
  }

  replayChanges(
    request?: Exclude<SpaghettiClientRequestMap['replayChanges'], undefined>,
    options?: SpaghettiQueryOptions,
  ): Promise<SpaghettiClientResponseMap['replayChanges']> {
    return this.query('replayChanges', request, options);
  }

  waitForCommit(
    request: SpaghettiClientRequestMap['waitForCommit'],
    options?: SpaghettiQueryOptions,
  ): Promise<SpaghettiClientResponseMap['waitForCommit']> {
    return this.query('waitForCommit', request, options);
  }

  listProjects(
    request?: Exclude<SpaghettiClientRequestMap['listProjects'], undefined>,
    options?: SpaghettiQueryOptions,
  ): Promise<SpaghettiClientResponseMap['listProjects']> {
    return this.query('listProjects', request, options);
  }

  listSessions(
    request: SpaghettiClientRequestMap['listSessions'],
    options?: SpaghettiQueryOptions,
  ): Promise<SpaghettiClientResponseMap['listSessions']> {
    return this.query('listSessions', request, options);
  }

  getSession(
    request: SpaghettiClientRequestMap['getSession'],
    options?: SpaghettiQueryOptions,
  ): Promise<SpaghettiClientResponseMap['getSession']> {
    return this.query('getSession', request, options);
  }

  getMessages(
    request: SpaghettiClientRequestMap['getMessages'],
    options?: SpaghettiQueryOptions,
  ): Promise<SpaghettiClientResponseMap['getMessages']> {
    return this.query('getMessages', request, options);
  }

  search(
    request: SpaghettiClientRequestMap['search'],
    options?: SpaghettiQueryOptions,
  ): Promise<SpaghettiClientResponseMap['search']> {
    return this.query('search', request, options);
  }

  getTimeline(
    request: SpaghettiClientRequestMap['getTimeline'],
    options?: SpaghettiQueryOptions,
  ): Promise<SpaghettiClientResponseMap['getTimeline']> {
    return this.query('getTimeline', request, options);
  }

  listDelegations(
    request: SpaghettiClientRequestMap['listDelegations'],
    options?: SpaghettiQueryOptions,
  ): Promise<SpaghettiClientResponseMap['listDelegations']> {
    return this.query('listDelegations', request, options);
  }

  listWorkflows(
    request: SpaghettiClientRequestMap['listWorkflows'],
    options?: SpaghettiQueryOptions,
  ): Promise<SpaghettiClientResponseMap['listWorkflows']> {
    return this.query('listWorkflows', request, options);
  }

  getWorkflow(
    request: SpaghettiClientRequestMap['getWorkflow'],
    options?: SpaghettiQueryOptions,
  ): Promise<SpaghettiClientResponseMap['getWorkflow']> {
    return this.query('getWorkflow', request, options);
  }

  listWorkflowMembers(
    request: SpaghettiClientRequestMap['listWorkflowMembers'],
    options?: SpaghettiQueryOptions,
  ): Promise<SpaghettiClientResponseMap['listWorkflowMembers']> {
    return this.query('listWorkflowMembers', request, options);
  }

  listMemoryDocuments(
    request: SpaghettiClientRequestMap['listMemoryDocuments'],
    options?: SpaghettiQueryOptions,
  ): Promise<SpaghettiClientResponseMap['listMemoryDocuments']> {
    return this.query('listMemoryDocuments', request, options);
  }

  listTaskCollections(
    request?: Exclude<SpaghettiClientRequestMap['listTaskCollections'], undefined>,
    options?: SpaghettiQueryOptions,
  ): Promise<SpaghettiClientResponseMap['listTaskCollections']> {
    return this.query('listTaskCollections', request, options);
  }

  listTasks(
    request: SpaghettiClientRequestMap['listTasks'],
    options?: SpaghettiQueryOptions,
  ): Promise<SpaghettiClientResponseMap['listTasks']> {
    return this.query('listTasks', request, options);
  }

  listPlans(
    request?: Exclude<SpaghettiClientRequestMap['listPlans'], undefined>,
    options?: SpaghettiQueryOptions,
  ): Promise<SpaghettiClientResponseMap['listPlans']> {
    return this.query('listPlans', request, options);
  }

  listToolResults(
    request: SpaghettiClientRequestMap['listToolResults'],
    options?: SpaghettiQueryOptions,
  ): Promise<SpaghettiClientResponseMap['listToolResults']> {
    return this.query('listToolResults', request, options);
  }

  listArtifacts(
    request: SpaghettiClientRequestMap['listArtifacts'],
    options?: SpaghettiQueryOptions,
  ): Promise<SpaghettiClientResponseMap['listArtifacts']> {
    return this.query('listArtifacts', request, options);
  }

  listSources(
    request?: Exclude<SpaghettiClientRequestMap['listSources'], undefined>,
    options?: SpaghettiQueryOptions,
  ): Promise<SpaghettiClientResponseMap['listSources']> {
    return this.query('listSources', request, options);
  }

  getStats(options?: SpaghettiQueryOptions): Promise<SpaghettiClientResponseMap['getStats']> {
    return this.query('getStats', undefined, options);
  }

  getUsage(
    request: SpaghettiClientRequestMap['getUsage'],
    options?: SpaghettiQueryOptions,
  ): Promise<SpaghettiClientResponseMap['getUsage']> {
    return this.query('getUsage', request, options);
  }

  getUsageActivity(
    request: SpaghettiClientRequestMap['getUsageActivity'],
    options?: SpaghettiQueryOptions,
  ): Promise<SpaghettiClientResponseMap['getUsageActivity']> {
    return this.query('getUsageActivity', request, options);
  }

  getRuntimeUsageV2(
    request: SpaghettiClientRequestMap['getRuntimeUsageV2'],
    options?: SpaghettiQueryOptions,
  ): Promise<SpaghettiClientResponseMap['getRuntimeUsageV2']> {
    return this.query('getRuntimeUsageV2', request, options);
  }

  getRuntimeUsageTotals(
    request: SpaghettiClientRequestMap['getRuntimeUsageTotals'],
    options?: SpaghettiQueryOptions,
  ): Promise<SpaghettiClientResponseMap['getRuntimeUsageTotals']> {
    return this.query('getRuntimeUsageTotals', request, options);
  }

  getRuntimeUsageCompatibility(
    request: SpaghettiClientRequestMap['getRuntimeUsageCompatibility'],
    options?: SpaghettiQueryOptions,
  ): Promise<SpaghettiClientResponseMap['getRuntimeUsageCompatibility']> {
    return this.query('getRuntimeUsageCompatibility', request, options);
  }

  getFactFamilyCoverage(
    request: SpaghettiClientRequestMap['getFactFamilyCoverage'],
    options?: SpaghettiQueryOptions,
  ): Promise<SpaghettiClientResponseMap['getFactFamilyCoverage']> {
    return this.query('getFactFamilyCoverage', request, options);
  }

  getRuntimeSnapshot(
    request?: Exclude<SpaghettiClientRequestMap['getRuntimeSnapshot'], undefined>,
    options?: SpaghettiQueryOptions,
  ): Promise<SpaghettiClientResponseMap['getRuntimeSnapshot']> {
    return this.query('getRuntimeSnapshot', request, options);
  }

  getRunState(
    request: SpaghettiClientRequestMap['getRunState'],
    options?: SpaghettiQueryOptions,
  ): Promise<SpaghettiClientResponseMap['getRunState']> {
    return this.query('getRunState', request, options);
  }

  listTeams(
    request?: Exclude<SpaghettiClientRequestMap['listTeams'], undefined>,
    options?: SpaghettiQueryOptions,
  ): Promise<SpaghettiClientResponseMap['listTeams']> {
    return this.query('listTeams', request, options);
  }

  getTeam(
    request: SpaghettiClientRequestMap['getTeam'],
    options?: SpaghettiQueryOptions,
  ): Promise<SpaghettiClientResponseMap['getTeam']> {
    return this.query('getTeam', request, options);
  }

  listTeamInboxes(
    request: SpaghettiClientRequestMap['listTeamInboxes'],
    options?: SpaghettiQueryOptions,
  ): Promise<SpaghettiClientResponseMap['listTeamInboxes']> {
    return this.query('listTeamInboxes', request, options);
  }

  listTeamInboxMessages(
    request: SpaghettiClientRequestMap['listTeamInboxMessages'],
    options?: SpaghettiQueryOptions,
  ): Promise<SpaghettiClientResponseMap['listTeamInboxMessages']> {
    return this.query('listTeamInboxMessages', request, options);
  }

  subscribe(
    request: SpaghettiSubscribeRequest = {},
    options: SpaghettiSubscribeOptions = {},
  ): AsyncIterable<SpaghettiCommittedChangeBatch> {
    if (this.closed) throw clientError(closedProtocolError());
    if (options.signal?.aborted) throw clientError(cancelledProtocolError());

    const wakeTimeoutMs = options.wakeTimeoutMs ?? SPAGHETTI_SUBSCRIPTION_WAKE_TIMEOUT_MS;
    if (
      !Number.isSafeInteger(wakeTimeoutMs) ||
      wakeTimeoutMs < 1 ||
      wakeTimeoutMs > SPAGHETTI_SUBSCRIPTION_MAX_WAKE_TIMEOUT_MS
    ) {
      throw clientError({
        code: 'invalid_request',
        message: `wakeTimeoutMs must be an integer between 1 and ${SPAGHETTI_SUBSCRIPTION_MAX_WAKE_TIMEOUT_MS}.`,
        field: 'wakeTimeoutMs',
      });
    }
    if (
      request.batchSize !== undefined &&
      (!Number.isSafeInteger(request.batchSize) || request.batchSize < 1 || request.batchSize > 1_000)
    ) {
      throw clientError({
        code: 'invalid_request',
        message: 'batchSize must be an integer between 1 and 1000.',
        field: 'batchSize',
      });
    }

    const initialCursor = request.from ? { ...request.from } : undefined;
    const topics = request.topics ? [...request.topics] : undefined;
    const batchSize = request.batchSize;
    return this.subscription(initialCursor, topics, batchSize, wakeTimeoutMs, options.signal);
  }

  getSubscriptionMetrics(): Readonly<SpaghettiSubscriptionMetrics> {
    return Object.freeze({ ...this.subscriptionMetrics });
  }

  dispose(): Promise<void> {
    if (this.disposePromise) return this.disposePromise;
    this.closed = true;
    for (const controller of this.subscriptions) controller.abort(cancelledProtocolError());
    this.subscriptions.clear();
    for (const controller of this.inFlight.values()) controller.abort(cancelledProtocolError());
    this.inFlight.clear();
    this.supersession.clear();
    this.disposePromise = Promise.resolve().then(() => this.transport.dispose());
    return this.disposePromise;
  }

  private async *subscription(
    initialCursor: SpaghettiSubscribeRequest['from'],
    topics: string[] | undefined,
    batchSize: number | undefined,
    wakeTimeoutMs: number,
    callerSignal: AbortSignal | undefined,
  ): AsyncGenerator<SpaghettiCommittedChangeBatch> {
    const controller = new AbortController();
    const signal = combineSignals(callerSignal, controller.signal);
    let cursor = initialCursor;
    let cancellationCounted = false;
    this.subscriptions.add(controller);
    this.subscriptionMetrics.activeSubscriptions += 1;

    try {
      while (!signal?.aborted) {
        const replayAfter = cursor;
        this.subscriptionMetrics.replayRequests += 1;
        const page = await this.replayChanges(
          {
            ...(cursor ? { after: cursor } : {}),
            ...(topics ? { topics } : {}),
            ...(batchSize !== undefined ? { limit: batchSize } : {}),
          },
          signal ? { signal } : undefined,
        );
        if (signal?.aborted) return;
        assertReplayProgress(page, cursor);
        this.subscriptionMetrics.replayPayloadBytes += page.payloadBytes;
        const lagStart = replayAfter?.commitSeq ?? page.oldestAvailable?.commitSeq ?? page.atCommitSeq;
        this.subscriptionMetrics.maxObservedLagCommits = Math.max(
          this.subscriptionMetrics.maxObservedLagCommits,
          page.atCommitSeq - lagStart,
        );

        if (page.changes.length > 0) {
          this.subscriptionMetrics.deliveredBatches += 1;
          this.subscriptionMetrics.deliveredChanges += page.changes.length;
          yield page;
        }

        if (page.hasMore) {
          cursor = page.nextCursor;
          continue;
        }

        cursor = { commitSeq: page.atCommitSeq, ordinal: MAX_CHANGE_ORDINAL };
        this.subscriptionMetrics.waitRequests += 1;
        const wake = await this.waitForCommit(
          { afterCommitSeq: page.atCommitSeq, timeoutMs: wakeTimeoutMs },
          signal ? { signal } : undefined,
        );
        if (signal?.aborted) return;
        assertCommitWait(wake, page.atCommitSeq);
        if (wake.reason === 'commit') this.subscriptionMetrics.commitWakeups += 1;
        else this.subscriptionMetrics.waitTimeouts += 1;
      }
    } catch (error) {
      if (signal?.aborted && error instanceof SpaghettiClientError && error.code === 'cancelled') {
        this.subscriptionMetrics.cancellations += 1;
        cancellationCounted = true;
        return;
      }
      throw error;
    } finally {
      if (signal?.aborted && !cancellationCounted) this.subscriptionMetrics.cancellations += 1;
      controller.abort(cancelledProtocolError());
      this.subscriptions.delete(controller);
      this.subscriptionMetrics.activeSubscriptions -= 1;
    }
  }

  private async query<M extends SpaghettiClientMethod>(
    method: M,
    payload: SpaghettiClientRequestMap[M],
    options?: SpaghettiQueryOptions,
  ): Promise<SpaghettiClientResponseMap[M]> {
    if (this.closed) throw clientError(closedProtocolError());
    if (options?.signal?.aborted) throw clientError(cancelledProtocolError());
    if (!this.info.methods.includes(method)) {
      throw clientError({
        code: 'unsupported_capability',
        message: 'The connected transport does not support this query.',
        capability: method,
      });
    }

    const requestId = this.allocateRequestId();
    const supersessionKey = options?.supersessionKey;
    const controller = new AbortController();
    this.inFlight.set(requestId, controller);
    if (supersessionKey) {
      this.supersession.get(supersessionKey)?.controller.abort(cancelledProtocolError());
      const supersession = { requestId, controller };
      this.supersession.set(supersessionKey, supersession);
    }
    const signal = combineSignals(options?.signal, controller.signal);

    try {
      const response = await abortableRequest(
        this.transport.request(
          {
            protocolVersion: this.info.protocolVersion,
            queryContractVersion: this.info.queryContractVersion,
            requestId,
            method,
            payload,
          },
          signal ? { signal } : undefined,
        ),
        signal,
        requestId,
      );

      if (supersessionKey && this.supersession.get(supersessionKey)?.requestId !== requestId) {
        throw clientError(cancelledProtocolError(), requestId);
      }
      if (
        response.requestId !== requestId ||
        response.protocolVersion !== this.info.protocolVersion ||
        response.queryContractVersion !== this.info.queryContractVersion
      ) {
        throw clientError(protocolMismatchError('response envelope does not match the negotiated request'), requestId);
      }
      if ('error' in response) throw clientError(response.error, requestId);
      assertResultContract(response.result, this.info.queryContractVersion, requestId);
      return response.result;
    } catch (error) {
      if (error instanceof SpaghettiClientError) throw error;
      throw clientError(normalizeTransportError(error, `${this.transport.kind}-${requestId}`), requestId);
    } finally {
      this.inFlight.delete(requestId);
      if (supersessionKey && this.supersession.get(supersessionKey)?.requestId === requestId) {
        this.supersession.delete(supersessionKey);
      }
    }
  }

  private allocateRequestId(): number {
    if (!Number.isSafeInteger(this.nextRequestId)) {
      throw clientError({ code: 'internal', message: 'The client request ID space is exhausted.' });
    }
    return this.nextRequestId++;
  }
}

export async function openSpaghettiClient(options: OpenSpaghettiClientOptions): Promise<SpaghettiClient> {
  const clientName = options.clientName?.trim() || '@vibecook/spaghetti-sdk';
  const protocolVersions = options.protocolVersions ?? [SPAGHETTI_CLIENT_PROTOCOL_VERSION];
  const queryContractVersions = options.queryContractVersions ?? [SPAGHETTI_QUERY_CONTRACT_VERSION];
  try {
    const info = await options.transport.connect({ clientName, protocolVersions, queryContractVersions });
    assertNegotiated(info, protocolVersions, queryContractVersions);
    return new DefaultSpaghettiClient(options.transport, info);
  } catch (error) {
    await options.transport.dispose().catch(() => undefined);
    if (error instanceof SpaghettiClientError) throw error;
    throw clientError(normalizeTransportError(error, `${options.transport.kind}-connect`));
  }
}

function assertNegotiated(
  info: SpaghettiClientInfo,
  protocolVersions: readonly number[],
  queryContractVersions: readonly number[],
): void {
  if (!protocolVersions.includes(info.protocolVersion)) {
    throw clientError(protocolMismatchError(`transport selected unoffered protocol ${info.protocolVersion}`));
  }
  if (!queryContractVersions.includes(info.queryContractVersion)) {
    throw clientError(
      protocolMismatchError(`transport selected unoffered query contract ${info.queryContractVersion}`),
    );
  }
  if (new Set(info.methods).size !== info.methods.length) {
    throw clientError(protocolMismatchError('transport advertised duplicate query methods'));
  }
  const unknown = info.methods.filter((method) => !SPAGHETTI_CLIENT_METHODS.includes(method));
  if (unknown.length > 0) {
    throw clientError(protocolMismatchError(`transport advertised unknown query methods: ${unknown.join(', ')}`));
  }
}

function assertResultContract(result: unknown, expected: number, requestId: number): void {
  if (!result || typeof result !== 'object' || !('contractVersion' in result)) return;
  if ((result as { contractVersion?: unknown }).contractVersion !== expected) {
    throw clientError(protocolMismatchError('result uses a query contract that was not negotiated'), requestId);
  }
}

function assertReplayProgress(page: SpaghettiCommittedChangeBatch, after: SpaghettiSubscribeRequest['from']): void {
  if (!Number.isSafeInteger(page.atCommitSeq) || page.atCommitSeq < 0) {
    throw clientError(protocolMismatchError('change replay returned an invalid snapshot watermark'));
  }
  if (!Number.isSafeInteger(page.payloadBytes) || page.payloadBytes < 0) {
    throw clientError(protocolMismatchError('change replay returned an invalid payload byte count'));
  }
  if (after && page.atCommitSeq < after.commitSeq) {
    throw clientError({
      code: 'cursor_invalid',
      message: 'The change cursor is ahead of the current durable watermark.',
      reason: 'ahead_of_watermark',
    });
  }
  if (page.hasMore && page.changes.length === 0) {
    throw clientError(protocolMismatchError('change replay reported more pages without making cursor progress'));
  }
  if (page.changes.length === 0) {
    if (page.nextCursor !== undefined) {
      throw clientError(protocolMismatchError('empty change replay returned a next cursor'));
    }
    return;
  }

  let previous = after;
  for (const change of page.changes) {
    assertChangeCursor(change.cursor);
    if (previous && compareChangeCursors(change.cursor, previous) <= 0) {
      throw clientError(protocolMismatchError('change replay cursors are not strictly increasing'));
    }
    if (change.cursor.commitSeq > page.atCommitSeq) {
      throw clientError(protocolMismatchError('change replay cursor exceeds its snapshot watermark'));
    }
    previous = change.cursor;
  }
  if (!page.nextCursor || compareChangeCursors(page.nextCursor, page.changes.at(-1)!.cursor) !== 0) {
    throw clientError(protocolMismatchError('change replay next cursor does not match its final change'));
  }
}

function assertCommitWait(result: SpaghettiClientResponseMap['waitForCommit'], afterCommitSeq: number): void {
  if (
    !Number.isSafeInteger(result.observedCommitSeq) ||
    result.observedCommitSeq < 0 ||
    !Number.isSafeInteger(result.waitedMs) ||
    result.waitedMs < 0 ||
    (result.reason !== 'commit' && result.reason !== 'timeout')
  ) {
    throw clientError(protocolMismatchError('commit wait returned an invalid result'));
  }
  if (result.reason === 'commit' && result.observedCommitSeq <= afterCommitSeq) {
    throw clientError(protocolMismatchError('commit wake did not advance the durable watermark'));
  }
  if (result.reason === 'timeout' && result.observedCommitSeq > afterCommitSeq) {
    throw clientError(protocolMismatchError('commit wait timed out despite a newer durable watermark'));
  }
}

function assertChangeCursor(cursor: SpaghettiSubscribeRequest['from']): void {
  if (
    !cursor ||
    !Number.isSafeInteger(cursor.commitSeq) ||
    cursor.commitSeq < 0 ||
    !Number.isInteger(cursor.ordinal) ||
    cursor.ordinal < 0 ||
    cursor.ordinal > MAX_CHANGE_ORDINAL
  ) {
    throw clientError(protocolMismatchError('change replay returned an invalid durable cursor'));
  }
}

function compareChangeCursors(
  left: NonNullable<SpaghettiSubscribeRequest['from']>,
  right: NonNullable<SpaghettiSubscribeRequest['from']>,
): number {
  return left.commitSeq === right.commitSeq ? left.ordinal - right.ordinal : left.commitSeq - right.commitSeq;
}

function combineSignals(...candidates: Array<AbortSignal | undefined>): AbortSignal | undefined {
  const signals = candidates.filter((signal): signal is AbortSignal => signal !== undefined);
  if (signals.length === 0) return undefined;
  if (signals.length === 1) return signals[0];
  return AbortSignal.any(signals);
}

function abortableRequest<T>(request: Promise<T>, signal: AbortSignal | undefined, requestId: number): Promise<T> {
  if (!signal) return request;
  if (signal.aborted) return Promise.reject(clientError(cancelledProtocolError(), requestId));
  return new Promise<T>((resolve, reject) => {
    let settled = false;
    const onAbort = (): void => {
      if (settled) return;
      settled = true;
      reject(clientError(cancelledProtocolError(), requestId));
    };
    signal.addEventListener('abort', onAbort, { once: true });
    request.then(
      (result) => {
        if (settled) return;
        settled = true;
        signal.removeEventListener('abort', onAbort);
        resolve(result);
      },
      (error: unknown) => {
        if (settled) return;
        settled = true;
        signal.removeEventListener('abort', onAbort);
        reject(error);
      },
    );
  });
}

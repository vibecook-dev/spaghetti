import { decodeSpaghettiIpcFrame, encodeSpaghettiIpcFrame, type SpaghettiIpcFrame } from './ipc-framing.js';
import type { SpaghettiIpcChannel } from './ipc-channel.js';
import {
  SPAGHETTI_MAX_REQUEST_BYTES,
  type AnySpaghettiProtocolRequest,
  type SpaghettiClientMethod,
  type SpaghettiClientTransport,
  type SpaghettiProtocolError,
  type SpaghettiProtocolRequest,
  type SpaghettiProtocolResponse,
  type SpaghettiTransportConnectRequest,
  type SpaghettiTransportConnectResponse,
  type SpaghettiTransportRequestOptions,
} from './protocol.js';
import { cancelledProtocolError, clientError, closedProtocolError, protocolMismatchError } from './errors.js';
import { encodedRequestBytes } from './encoding.js';

export const SPAGHETTI_IPC_CONNECT_TIMEOUT_MS = 10_000;

export interface SpaghettiIpcFrameObservation {
  direction: 'sent' | 'received';
  byteLength: number;
}

export interface IpcTransportOptions {
  channel: SpaghettiIpcChannel;
  /** Maximum handshake wait. Defaults to 10 seconds. */
  connectTimeoutMs?: number;
  /** Optional encoded-frame telemetry for diagnostics and topology benchmarks. */
  onFrame?(observation: SpaghettiIpcFrameObservation): void;
}

interface PendingConnect {
  resolve(info: SpaghettiTransportConnectResponse): void;
  reject(error: unknown): void;
  cleanup(): void;
}

interface PendingRequest {
  request: AnySpaghettiProtocolRequest;
  resolve(response: SpaghettiProtocolResponse): void;
  cleanup(): void;
}

type IpcTransportState = 'idle' | 'connecting' | 'connected' | 'closed';

/** Client-side framed IPC transport over a bounded binary channel. */
export class IpcTransport implements SpaghettiClientTransport {
  readonly kind = 'ipc';
  private readonly channel: SpaghettiIpcChannel;
  private readonly connectTimeoutMs: number;
  private readonly onFrame: IpcTransportOptions['onFrame'];
  private readonly pendingRequests = new Map<number, PendingRequest>();
  private readonly unsubscribeMessage: () => void;
  private readonly unsubscribeClose: () => void;
  private state: IpcTransportState = 'idle';
  private connectPending: PendingConnect | undefined;
  private connectedInfo: SpaghettiTransportConnectResponse | undefined;
  private disposePromise: Promise<void> | undefined;

  constructor(options: IpcTransportOptions) {
    const timeout = options.connectTimeoutMs ?? SPAGHETTI_IPC_CONNECT_TIMEOUT_MS;
    if (!Number.isSafeInteger(timeout) || timeout < 1) {
      throw new TypeError('connectTimeoutMs must be a positive safe integer.');
    }
    this.channel = options.channel;
    this.connectTimeoutMs = timeout;
    this.onFrame = options.onFrame;
    this.unsubscribeMessage = this.channel.onMessage(this.handleMessage);
    this.unsubscribeClose = this.channel.onClose(this.handleChannelClose);
  }

  async connect(
    request: SpaghettiTransportConnectRequest,
    options?: SpaghettiTransportRequestOptions,
  ): Promise<SpaghettiTransportConnectResponse> {
    if (this.state === 'closed') throw clientError(closedProtocolError());
    if (this.state === 'connected') return this.connectedInfo!;
    if (this.state === 'connecting') {
      throw clientError({
        code: 'invalid_request',
        message: 'An IPC connection handshake is already in progress.',
        field: 'connect',
        reason: 'duplicate',
      });
    }
    if (options?.signal?.aborted) throw clientError(cancelledProtocolError());

    this.state = 'connecting';
    const response = new Promise<SpaghettiTransportConnectResponse>((resolve, reject) => {
      const timeout = setTimeout(() => {
        this.closeWith(
          {
            code: 'transport_unavailable',
            message: 'The IPC host did not complete its handshake in time.',
            reason: 'connect_timeout',
          },
          true,
        );
      }, this.connectTimeoutMs);
      (timeout as ReturnType<typeof setTimeout> & { unref?: () => void }).unref?.();

      const onAbort = (): void => this.closeWith(cancelledProtocolError(), true);
      this.connectPending = {
        resolve,
        reject,
        cleanup: () => {
          clearTimeout(timeout);
          options?.signal?.removeEventListener('abort', onAbort);
        },
      };
      options?.signal?.addEventListener('abort', onAbort, { once: true });
      if (options?.signal?.aborted) onAbort();
    });

    if (this.state === 'connecting') {
      try {
        await this.sendFrame({ type: 'connect', request });
      } catch {
        this.closeWith(
          {
            code: 'transport_unavailable',
            message: 'The IPC host is unavailable.',
            reason: 'connect_send_failed',
          },
          true,
        );
      }
    }
    return response;
  }

  async request<M extends SpaghettiClientMethod>(
    request: SpaghettiProtocolRequest<M>,
    options?: SpaghettiTransportRequestOptions,
  ): Promise<SpaghettiProtocolResponse<M>> {
    if (this.state === 'closed') return failureResponse(request, closedProtocolError());
    if (this.state !== 'connected') {
      return failureResponse(request, protocolMismatchError('connect must succeed before the first request'));
    }
    if (!Number.isSafeInteger(request.requestId) || request.requestId < 1) {
      return failureResponse(request, {
        code: 'invalid_request',
        message: 'requestId must be a positive safe integer.',
        field: 'requestId',
      });
    }
    if (encodedRequestBytes(request.payload) > SPAGHETTI_MAX_REQUEST_BYTES) {
      return failureResponse(request, {
        code: 'invalid_request',
        message: `The query request exceeds the ${SPAGHETTI_MAX_REQUEST_BYTES}-byte transport limit.`,
        field: 'payload',
        reason: 'payload_too_large',
      });
    }
    if (options?.signal?.aborted) return failureResponse(request, cancelledProtocolError());
    if (this.pendingRequests.has(request.requestId)) {
      return failureResponse(request, {
        code: 'invalid_request',
        message: 'requestId is already in flight.',
        field: 'requestId',
        reason: 'duplicate',
      });
    }

    const response = new Promise<SpaghettiProtocolResponse<M>>((resolve) => {
      const onAbort = (): void => {
        const pending = this.pendingRequests.get(request.requestId);
        if (pending !== entry) return;
        this.pendingRequests.delete(request.requestId);
        pending.cleanup();
        pending.resolve(failureResponse(request, cancelledProtocolError()));
        void this.sendFrame({ type: 'cancel', requestId: request.requestId }).catch(() => undefined);
      };
      const entry: PendingRequest = {
        request: request as AnySpaghettiProtocolRequest,
        resolve: (value) => resolve(value as SpaghettiProtocolResponse<M>),
        cleanup: () => options?.signal?.removeEventListener('abort', onAbort),
      };
      this.pendingRequests.set(request.requestId, entry);
      options?.signal?.addEventListener('abort', onAbort, { once: true });
      if (options?.signal?.aborted) onAbort();
    });

    if (this.pendingRequests.has(request.requestId)) {
      try {
        await this.sendFrame({ type: 'request', request: request as AnySpaghettiProtocolRequest });
      } catch {
        this.closeWith(closedProtocolError(), true);
      }
    }
    return response;
  }

  dispose(): Promise<void> {
    if (this.disposePromise) return this.disposePromise;
    const sendClose = this.state !== 'closed';
    this.transitionClosed(closedProtocolError());
    this.disposePromise = (async () => {
      if (sendClose) await this.sendFrame({ type: 'close' }).catch(() => undefined);
      await this.channel.close().catch(() => undefined);
    })();
    return this.disposePromise;
  }

  private readonly handleMessage = (encoded: Uint8Array): void => {
    if (this.state === 'closed') return;
    this.observeFrame('received', encoded.byteLength);
    let frame: SpaghettiIpcFrame;
    try {
      frame = decodeSpaghettiIpcFrame(encoded);
    } catch {
      this.closeWith(protocolMismatchError('the IPC host sent an invalid frame'), true);
      return;
    }

    switch (frame.type) {
      case 'connect-result':
        this.handleConnectResult(frame);
        return;
      case 'response': {
        if (this.state !== 'connected') {
          this.closeWith(protocolMismatchError('the IPC host sent a response before negotiation'), true);
          return;
        }
        const pending = this.pendingRequests.get(frame.response.requestId);
        if (!pending) return; // Late response for a locally cancelled request.
        this.pendingRequests.delete(frame.response.requestId);
        pending.cleanup();
        pending.resolve(frame.response);
        return;
      }
      case 'close':
        this.closeWith(closedProtocolError(), true);
        return;
      case 'connect':
      case 'request':
      case 'cancel':
        this.closeWith(protocolMismatchError('the IPC host sent a client-only frame'), true);
    }
  };

  private readonly handleChannelClose = (): void => {
    this.closeWith(closedProtocolError(), false);
  };

  private handleConnectResult(frame: Extract<SpaghettiIpcFrame, { type: 'connect-result' }>): void {
    if (this.state !== 'connecting' || !this.connectPending) {
      this.closeWith(protocolMismatchError('the IPC host sent an unexpected handshake result'), true);
      return;
    }
    if ('error' in frame) {
      this.closeWith(frame.error, true);
      return;
    }

    const info: SpaghettiTransportConnectResponse = {
      transportKind: frame.transportKind,
      protocolVersion: frame.protocolVersion,
      queryContractVersion: frame.queryContractVersion,
      engineVersion: frame.engineVersion,
      methods: frame.methods,
    };
    const pending = this.connectPending;
    this.connectPending = undefined;
    pending.cleanup();
    this.connectedInfo = info;
    this.state = 'connected';
    pending.resolve(info);
  }

  private closeWith(error: SpaghettiProtocolError, closeChannel: boolean): void {
    if (!this.transitionClosed(error)) return;
    this.disposePromise = closeChannel ? this.channel.close().catch(() => undefined) : Promise.resolve();
  }

  private transitionClosed(error: SpaghettiProtocolError): boolean {
    if (this.state === 'closed') return false;
    this.state = 'closed';
    this.connectedInfo = undefined;
    this.unsubscribeMessage();
    this.unsubscribeClose();

    const connect = this.connectPending;
    this.connectPending = undefined;
    if (connect) {
      connect.cleanup();
      connect.reject(clientError(error));
    }
    for (const pending of this.pendingRequests.values()) {
      pending.cleanup();
      pending.resolve(failureResponse(pending.request, error));
    }
    this.pendingRequests.clear();
    return true;
  }

  private sendFrame(frame: SpaghettiIpcFrame): Promise<void> {
    const encoded = encodeSpaghettiIpcFrame(frame);
    this.observeFrame('sent', encoded.byteLength);
    return this.channel.send(encoded);
  }

  private observeFrame(direction: SpaghettiIpcFrameObservation['direction'], byteLength: number): void {
    try {
      this.onFrame?.({ direction, byteLength });
    } catch {
      // Diagnostic observers cannot alter transport behavior.
    }
  }
}

function failureResponse<M extends SpaghettiClientMethod>(
  request: SpaghettiProtocolRequest<M>,
  error: SpaghettiProtocolError,
): SpaghettiProtocolResponse<M> {
  return {
    protocolVersion: request.protocolVersion,
    queryContractVersion: request.queryContractVersion,
    requestId: request.requestId,
    ok: false,
    error,
  };
}

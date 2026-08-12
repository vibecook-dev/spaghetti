import type { SpaghettiIpcChannel } from './ipc-channel.js';
import { decodeSpaghettiIpcFrame, encodeSpaghettiIpcFrame, type SpaghettiIpcFrame } from './ipc-framing.js';
import {
  SPAGHETTI_MAX_REQUEST_BYTES,
  type AnySpaghettiProtocolRequest,
  type SpaghettiClientTransport,
  type SpaghettiProtocolError,
  type SpaghettiProtocolResponse,
  type SpaghettiTransportConnectRequest,
} from './protocol.js';
import { cancelledProtocolError, normalizeTransportError, protocolMismatchError } from './errors.js';
import { encodedRequestBytes } from './encoding.js';

export interface SpaghettiIpcHostOptions {
  channel: SpaghettiIpcChannel;
  transport: SpaghettiClientTransport;
  /** Dispose the backing transport when the IPC host closes. Defaults to true. */
  ownsTransport?: boolean;
  /** Advertised topology name. Defaults to `ipc`. */
  transportKind?: string;
}

type IpcHostState = 'idle' | 'connecting' | 'connected' | 'failed' | 'closed';

/**
 * Serve one negotiated Spaghetti client over a framed binary channel.
 * A host owns one channel and never exposes the backing transport directly.
 */
export class SpaghettiIpcHost {
  private readonly channel: SpaghettiIpcChannel;
  private readonly transport: SpaghettiClientTransport;
  private readonly ownsTransport: boolean;
  private readonly transportKind: string;
  private readonly activeRequests = new Map<number, AbortController>();
  private readonly unsubscribeMessage: () => void;
  private readonly unsubscribeClose: () => void;
  private state: IpcHostState = 'idle';
  private connectTask: Promise<void> | undefined;
  private disposePromise: Promise<void> | undefined;

  constructor(options: SpaghettiIpcHostOptions) {
    const transportKind = options.transportKind?.trim() || 'ipc';
    this.channel = options.channel;
    this.transport = options.transport;
    this.ownsTransport = options.ownsTransport ?? true;
    this.transportKind = transportKind;
    this.unsubscribeMessage = this.channel.onMessage(this.handleMessage);
    this.unsubscribeClose = this.channel.onClose(this.handleChannelClose);
  }

  dispose(): Promise<void> {
    return this.shutdown(true);
  }

  private readonly handleMessage = (encoded: Uint8Array): void => {
    if (this.state === 'closed') return;
    let frame: SpaghettiIpcFrame;
    try {
      frame = decodeSpaghettiIpcFrame(encoded);
    } catch {
      void this.shutdown(false).catch(() => undefined);
      return;
    }

    switch (frame.type) {
      case 'connect':
        if (this.state !== 'idle') {
          void this.shutdown(false).catch(() => undefined);
          return;
        }
        this.state = 'connecting';
        this.connectTask = this.handleConnect(frame.request);
        return;
      case 'request':
        void this.handleRequestWhenReady(frame.request).catch(() => this.shutdown(false).catch(() => undefined));
        return;
      case 'cancel':
        this.activeRequests.get(frame.requestId)?.abort(cancelledProtocolError());
        return;
      case 'close':
        void this.shutdown(false).catch(() => undefined);
        return;
      case 'connect-result':
      case 'response':
        void this.shutdown(false).catch(() => undefined);
    }
  };

  private readonly handleChannelClose = (): void => {
    void this.shutdown(false).catch(() => undefined);
  };

  private async handleConnect(request: SpaghettiTransportConnectRequest): Promise<void> {
    try {
      const info = await this.transport.connect(request);
      if (this.state === 'closed') return;
      this.state = 'connected';
      await this.sendFrame({
        type: 'connect-result',
        ok: true,
        ...info,
        transportKind: this.transportKind,
      });
    } catch (error) {
      if (this.state === 'closed') return;
      this.state = 'failed';
      const protocolError = normalizeTransportError(error, 'ipc-host-connect');
      await this.sendFrame({ type: 'connect-result', ok: false, error: protocolError }).catch(() => undefined);
    }
  }

  private async handleRequestWhenReady(request: AnySpaghettiProtocolRequest): Promise<void> {
    const connectTask = this.connectTask;
    if (connectTask) await connectTask.catch(() => undefined);
    if (this.state === 'closed') return;
    if (this.state !== 'connected') {
      await this.sendFrame(
        failureFrame(request, protocolMismatchError('connect must succeed before the first request')),
      ).catch(() => {
        void this.shutdown(false).catch(() => undefined);
      });
      return;
    }
    if (encodedRequestBytes(request.payload) > SPAGHETTI_MAX_REQUEST_BYTES) {
      await this.sendFrame(
        failureFrame(request, {
          code: 'invalid_request',
          message: `The query request exceeds the ${SPAGHETTI_MAX_REQUEST_BYTES}-byte transport limit.`,
          field: 'payload',
          reason: 'payload_too_large',
        }),
      ).catch(() => {
        void this.shutdown(false).catch(() => undefined);
      });
      return;
    }
    if (this.activeRequests.has(request.requestId)) {
      await this.sendFrame(
        failureFrame(request, {
          code: 'invalid_request',
          message: 'requestId is already in flight.',
          field: 'requestId',
          reason: 'duplicate',
        }),
      ).catch(() => {
        void this.shutdown(false).catch(() => undefined);
      });
      return;
    }

    const controller = new AbortController();
    this.activeRequests.set(request.requestId, controller);
    let response: SpaghettiProtocolResponse;
    try {
      response = await this.transport.request(request, { signal: controller.signal });
    } catch (error) {
      response = failureResponse(request, normalizeTransportError(error, `ipc-host-${request.requestId}`));
    } finally {
      this.activeRequests.delete(request.requestId);
    }

    if (this.state !== 'connected' || controller.signal.aborted) return;
    await this.sendFrame({ type: 'response', response }).catch(() => {
      void this.shutdown(false).catch(() => undefined);
    });
  }

  private shutdown(sendClose: boolean): Promise<void> {
    if (this.disposePromise) return this.disposePromise;
    this.state = 'closed';
    this.connectTask = undefined;
    this.unsubscribeMessage();
    this.unsubscribeClose();
    for (const controller of this.activeRequests.values()) controller.abort(cancelledProtocolError());
    this.activeRequests.clear();

    this.disposePromise = (async () => {
      if (sendClose) await this.sendFrame({ type: 'close' }).catch(() => undefined);
      await this.channel.close().catch(() => undefined);
      if (this.ownsTransport) await this.transport.dispose();
    })();
    return this.disposePromise;
  }

  private sendFrame(frame: SpaghettiIpcFrame): Promise<void> {
    return this.channel.send(encodeSpaghettiIpcFrame(frame));
  }
}

export function serveSpaghettiIpc(options: SpaghettiIpcHostOptions): SpaghettiIpcHost {
  return new SpaghettiIpcHost(options);
}

function failureFrame(
  request: AnySpaghettiProtocolRequest,
  error: SpaghettiProtocolError,
): Extract<SpaghettiIpcFrame, { type: 'response' }> {
  return { type: 'response', response: failureResponse(request, error) };
}

function failureResponse(
  request: AnySpaghettiProtocolRequest,
  error: SpaghettiProtocolError,
): SpaghettiProtocolResponse {
  return {
    protocolVersion: request.protocolVersion,
    queryContractVersion: request.queryContractVersion,
    requestId: request.requestId,
    ok: false,
    error,
  };
}

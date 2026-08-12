/** Minimal binary channel used by the framed IPC transport and host. */
export interface SpaghettiIpcChannel {
  readonly kind: string;
  send(frame: Uint8Array): Promise<void>;
  onMessage(listener: (frame: Uint8Array) => void): () => void;
  onClose(listener: () => void): () => void;
  close(): Promise<void>;
}

/**
 * The common surface shared by Node MessagePort, browser MessagePort, and
 * Electron MessagePortMain. Listener methods are detected at runtime.
 */
export interface SpaghettiMessagePort {
  postMessage(value: Uint8Array): void;
  start?(): void;
  close?(): void;
}

interface PortInternals extends SpaghettiMessagePort {
  on?(event: string, listener: (...args: unknown[]) => void): unknown;
  off?(event: string, listener: (...args: unknown[]) => void): unknown;
  removeListener?(event: string, listener: (...args: unknown[]) => void): unknown;
  addEventListener?(event: string, listener: (...args: unknown[]) => void): unknown;
  removeEventListener?(event: string, listener: (...args: unknown[]) => void): unknown;
}

export class SpaghettiIpcChannelClosedError extends Error {
  constructor() {
    super('The IPC channel is closed.');
    this.name = 'SpaghettiIpcChannelClosedError';
  }
}

/** Binary channel adapter for Node, Electron, and DOM-style message ports. */
export class MessagePortIpcChannel implements SpaghettiIpcChannel {
  readonly kind: string;
  private readonly port: PortInternals;
  private readonly messageListeners = new Set<(frame: Uint8Array) => void>();
  private readonly closeListeners = new Set<() => void>();
  private listenerStyle: 'emitter' | 'event-target';
  private closed = false;
  private closePromise: Promise<void> | undefined;

  constructor(port: SpaghettiMessagePort, kind = 'message-port') {
    this.port = port as PortInternals;
    this.kind = kind;

    if (typeof this.port.on === 'function') {
      this.listenerStyle = 'emitter';
      this.port.on('message', this.handlePortMessage);
      this.port.on('close', this.handlePortClose);
    } else if (typeof this.port.addEventListener === 'function') {
      this.listenerStyle = 'event-target';
      this.port.addEventListener('message', this.handlePortMessage);
      this.port.addEventListener('close', this.handlePortClose);
    } else {
      throw new TypeError('The message port does not expose an event listener API.');
    }

    this.port.start?.();
  }

  async send(frame: Uint8Array): Promise<void> {
    if (this.closed) throw new SpaghettiIpcChannelClosedError();
    this.port.postMessage(frame.slice());
  }

  onMessage(listener: (frame: Uint8Array) => void): () => void {
    if (this.closed) return () => undefined;
    this.messageListeners.add(listener);
    return () => this.messageListeners.delete(listener);
  }

  onClose(listener: () => void): () => void {
    if (this.closed) {
      queueMicrotask(listener);
      return () => undefined;
    }
    this.closeListeners.add(listener);
    return () => this.closeListeners.delete(listener);
  }

  close(): Promise<void> {
    if (this.closePromise) return this.closePromise;
    this.closePromise = this.finishClose(true);
    return this.closePromise;
  }

  private readonly handlePortMessage = (...args: unknown[]): void => {
    if (this.closed) return;
    const frame = messageBytes(args[0]);
    for (const listener of [...this.messageListeners]) listener(frame);
  };

  private readonly handlePortClose = (): void => {
    if (this.closePromise) return;
    this.closePromise = this.finishClose(false);
    void this.closePromise.catch(() => undefined);
  };

  private async finishClose(closePort: boolean): Promise<void> {
    if (this.closed) return;
    this.closed = true;
    this.detachPortListeners();
    try {
      if (closePort) this.port.close?.();
    } finally {
      const listeners = [...this.closeListeners];
      this.messageListeners.clear();
      this.closeListeners.clear();
      for (const listener of listeners) listener();
    }
  }

  private detachPortListeners(): void {
    if (this.listenerStyle === 'emitter') {
      const remove = this.port.off ?? this.port.removeListener;
      remove?.call(this.port, 'message', this.handlePortMessage);
      remove?.call(this.port, 'close', this.handlePortClose);
      return;
    }
    this.port.removeEventListener?.('message', this.handlePortMessage);
    this.port.removeEventListener?.('close', this.handlePortClose);
  }
}

function messageBytes(message: unknown): Uint8Array {
  const value = isMessageEvent(message) ? message.data : message;
  if (value instanceof Uint8Array) return value.slice();
  if (value instanceof ArrayBuffer) return new Uint8Array(value.slice(0));
  if (ArrayBuffer.isView(value)) {
    return new Uint8Array(value.buffer, value.byteOffset, value.byteLength).slice();
  }
  // Let the frame decoder classify non-binary messages as malformed input.
  return new Uint8Array();
}

function isMessageEvent(value: unknown): value is { data: unknown } {
  return !!value && typeof value === 'object' && 'data' in value && !ArrayBuffer.isView(value);
}

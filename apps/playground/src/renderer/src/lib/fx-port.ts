/**
 * Listens for `fx-port` window messages forwarded from the preload.
 * Every workspace open produces a fresh MessagePort; consumers swap
 * the PortFileExplorer connection when a new one lands.
 */

export interface FxPortMessage {
  port: MessagePort;
  workspaceRoot: string;
}

type Listener = (msg: FxPortMessage) => void;
const listeners = new Set<Listener>();

let resolveInitial: ((msg: FxPortMessage) => void) | null = null;
let initialDelivered = false;
let pendingInitial: FxPortMessage | null = null;

export const fxPortReady: Promise<FxPortMessage> = new Promise((res) => {
  resolveInitial = res;
  if (pendingInitial) {
    res(pendingInitial);
    pendingInitial = null;
  }
});

function deliver(msg: FxPortMessage): void {
  if (!initialDelivered) {
    initialDelivered = true;
    if (resolveInitial) {
      resolveInitial(msg);
    } else {
      pendingInitial = msg;
    }
    // Also notify swap listeners so late subscribers still get the first port
    // if they only used onFxPort (panel opens after main already forked).
    for (const cb of listeners) {
      try {
        cb(msg);
      } catch (err) {
        console.error('[fx-port] listener threw', err);
      }
    }
    return;
  }
  for (const cb of listeners) {
    try {
      cb(msg);
    } catch (err) {
      console.error('[fx-port] listener threw', err);
    }
  }
}

if (typeof window !== 'undefined') {
  window.addEventListener('message', (evt: MessageEvent) => {
    const data = evt.data as { type?: string; workspaceRoot?: string } | undefined;
    if (data?.type !== 'fx-port' || evt.ports.length === 0) return;
    deliver({
      port: evt.ports[0]!,
      workspaceRoot: data.workspaceRoot ?? '',
    });
  });
}

/**
 * Subscribe to every `fx-port` arrival (including the first if it already
 * landed before subscribe — use a local latch if you only want subsequent).
 */
export function onFxPort(cb: Listener): () => void {
  listeners.add(cb);
  return () => {
    listeners.delete(cb);
  };
}

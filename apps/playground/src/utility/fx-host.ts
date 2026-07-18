/**
 * UtilityProcess host for @vibecook/mille.
 *
 * Loads the native `.node` binding out of the renderer/main process so
 * walkers and watchers run in their own thread pool. Main spawns this
 * script with WORKSPACE_ROOT set, waits for `{ type: 'ready' }`, then
 * attaches a MessagePort via `{ type: 'attach' }`.
 *
 * Ported from the mille Electron playground (without git decorations).
 */

import { createFileExplorerHost } from '@vibecook/mille/host';
import type { FileExplorerHost, MessagePortLike } from '@vibecook/mille/host';
import type { MessagePortMain } from 'electron';

/**
 * Electron MessagePortMain vs node:worker_threads MessagePort:
 * 1. Events arrive as MessageEvent `{ data, ports }` — unwrap to raw payload.
 * 2. `postMessage(msg, undefined)` drops the message — omit transfer list when empty.
 */
function wrapMessagePortMain(port: MessagePortMain): MessagePortLike {
  return {
    addEventListener: (_type, listener) => {
      port.on('message', (evt) => listener({ data: evt.data }));
    },
    removeEventListener: () => {
      /* host attaches a single listener per port */
    },
    postMessage: (msg, transfer) => {
      if (transfer && (transfer as unknown[]).length > 0) {
        port.postMessage(msg, transfer as MessagePortMain[]);
      } else {
        port.postMessage(msg);
      }
    },
    start: () => port.start(),
    close: () => port.close(),
  };
}

let host: FileExplorerHost | null = null;

async function bootstrap(): Promise<void> {
  const root = process.env.WORKSPACE_ROOT;
  if (!root) throw new Error('WORKSPACE_ROOT env var not set');

  host = await createFileExplorerHost({
    roots: [root],
    respectIgnore: true,
    followSymlinks: 'smart',
    watchDebounceMs: 75,
    // Seed only the root entry; children load on expand (good for monorepos).
    initialWalk: 'roots-only',
  });

  // Attach listener before ready so main can transfer the port immediately.
  process.parentPort.on('message', (evt) => {
    const msg = evt.data as { type?: string } | undefined;
    const port = evt.ports[0];
    if (msg?.type === 'attach' && port) {
      host!.attachPort(wrapMessagePortMain(port));
      console.log('[fx-host] attachPort done');
    }
  });

  process.parentPort.postMessage({ type: 'ready' });
  console.log(`[fx-host] ready for ${root}`);
}

bootstrap().catch((err) => {
  console.error('[fx-host] bootstrap failed:', err);
  process.exit(1);
});

process.on('exit', () => {
  if (host) void host.dispose();
});

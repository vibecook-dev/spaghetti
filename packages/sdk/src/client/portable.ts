/**
 * Runtime-portable Spaghetti client entrypoint.
 *
 * This module deliberately excludes embedded N-API and engine-opening code so
 * Electron main, renderers, and remote clients do not load storage/watchers.
 */
export * from './protocol.js';
export { normalizeTransportError } from './errors.js';
export {
  openSpaghettiClient,
  SPAGHETTI_SUBSCRIPTION_POLL_INTERVAL_MS,
  type OpenSpaghettiClientOptions,
} from './client.js';
export * from './ipc-channel.js';
export * from './ipc-framing.js';
export { IpcTransport, SPAGHETTI_IPC_CONNECT_TIMEOUT_MS, type IpcTransportOptions } from './ipc-transport.js';

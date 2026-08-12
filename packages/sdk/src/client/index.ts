export * from './protocol.js';
export { normalizeTransportError } from './errors.js';
export {
  NapiTransport,
  openNapiTransport,
  type NapiTransportOptions,
  type OpenNapiTransportOptions,
} from './napi-transport.js';
export {
  openSpaghettiClient,
  SPAGHETTI_SUBSCRIPTION_POLL_INTERVAL_MS,
  type OpenSpaghettiClientOptions,
} from './client.js';
export { openEmbeddedSpaghettiClient, type OpenEmbeddedSpaghettiClientOptions } from './embedded-client.js';
export * from './ipc-channel.js';
export * from './ipc-framing.js';
export {
  IpcTransport,
  SPAGHETTI_IPC_CONNECT_TIMEOUT_MS,
  type IpcTransportOptions,
  type SpaghettiIpcFrameObservation,
} from './ipc-transport.js';
export { SpaghettiIpcHost, serveSpaghettiIpc, type SpaghettiIpcHostOptions } from './ipc-host.js';

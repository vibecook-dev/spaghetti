/** Runtime entry for the sole-owner Rust observation/product service. */

export {
  createObservationService,
  type ObservationService,
  type ObservationServiceOptions,
} from './observation-service.js';
export {
  openObservationHost,
  type ObservationHost,
  type ObservationHostFactFamilyReplayRequest,
  type ObservationHostOptions,
  type ObservationHostProgress,
  type ObservationHostSnapshot,
  type ObservationHostSource,
} from './observation-host.js';
export { MessagePortIpcChannel, type SpaghettiMessagePort } from './client/index.js';
export type {
  SpaghettiCatalogPageOptions,
  SpaghettiCatalogProject,
  SpaghettiCatalogProjectPage,
  SpaghettiCatalogResolution,
  SpaghettiCatalogSession,
  SpaghettiCatalogSessionPage,
  SpaghettiCatalogSessionPageOptions,
  SpaghettiCatalogStartup,
  SpaghettiCatalogState,
  SpaghettiReadiness,
  SpaghettiReadinessField,
  SpaghettiReadinessState,
} from './native.js';

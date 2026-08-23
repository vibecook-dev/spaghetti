/**
 * @vibecook/spaghetti-sdk — canonical Rust observation/query API plus
 * transport-neutral DTOs and presentation helpers.
 *
 * The retired TypeScript ingestion/query implementation is deliberately not
 * reachable from this package entry. Repository-only differential tests use
 * `legacy-oracle.ts` directly.
 */

// Public domain and compatibility DTOs (types erase from runtime bundles).
export * from './types/index.js';
export * from './api.js';
export * from './data/segment-types.js';
export * from './data/summary-types.js';
export * from './data/timeline-query.js';

// RFC 012A topology-independent identity, qualification, and coverage wire
// contracts. Rust derives references; portable consumers validate and compare.
export * from './contracts/rfc012a.js';

// RFC 012C portable semantic values plus the bounded unknown-evidence
// aggregate/sample snapshot. The latter is not itself query or observer
// authority; enclosing RFC 012B/012D contracts bind scope and lifecycle.
export * from './contracts/rfc012c.js';
export * from './contracts/rfc012c-unknown-evidence.js';
export {
  mergeDurableAndScopedUsage,
  type DurableLiveUsageMerge,
  type DurableUsageContribution,
  type ScopedUsageObserverEvent,
} from './runtime/usage-v2-live-merge.js';

// RFC 012D exact-version negotiation, contextual envelopes, and the first
// store-free exact-known-object native observer owner. Artifact reads and
// dynamic descendant composition remain gated on their full scoped contracts.
export * from './contracts/rfc012d.js';
export * from './contracts/rfc012d-actor-envelope.js';
export * from './contracts/rfc012d-artifact.js';
export * from './contracts/rfc012d-artifact-availability.js';
export * from './contracts/rfc012d-artifact-availability-envelope.js';
export * from './contracts/rfc012d-capability-snapshot.js';
export * from './contracts/rfc012d-close.js';
export * from './contracts/rfc012d-completion-envelope.js';
export * from './contracts/rfc012d-continuity-envelope.js';
export * from './contracts/rfc012d-event-envelope.js';
export * from './contracts/rfc012d-known-envelope.js';
export * from './contracts/rfc012d-replacement-manifest.js';
export * from './contracts/rfc012d-scope-coverage.js';
export * from './contracts/rfc012d-source-envelope.js';
export * from './contracts/rfc012d-unknown-wire.js';
export * from './contracts/rfc012d-usage-envelope.js';
export * from './contracts/rfc012d-watermark.js';
export {
  SCOPED_OBSERVATION_REQUEST_CONTRACT_VERSION,
  ScopedObservationRequestError,
  ScopedObservationTransportError,
  observeSession,
  type SessionObservationApply,
  type SessionObservationRequest,
  type SessionObservationRootIdentity,
  type SessionObserver,
} from './scoped-observation.js';

// Transport-neutral async client and the sole-owner production service.
export * from './client/index.js';
export * from './observation.js';

// Low-level native engine lifecycle and its typed canonical query contract.
export * from './native.js';

// Presentation-only source metadata. These helpers never inspect source data.
export { sourceReportsPerMessageTokens, sourceDisplayName, sourceDisplayRoot } from './sources/capabilities.js';

// Transitional settings remain readable for upgrade diagnostics. Production
// observation always uses Rust; no root export constructs a TypeScript owner.
export {
  type IngestEngine,
  type SpaghettiSettings,
  readSettings,
  writeSettings,
  resolveEngine,
  defaultDbPathForEngine,
  settingsPath,
} from './settings.js';

// Compatibility invalidation types used by existing presentation clients.
export type { Change, ChangeType, ChangeTopic, SubscribeOptions, Dispose } from './live/change-events.js';
export {
  isSessionMessageAdded,
  isSessionCreated,
  isSessionRewritten,
  isSubagentUpdated,
  isToolResultAdded,
  isFileHistoryAdded,
  isTodoUpdated,
  isTaskUpdated,
  isPlanUpserted,
  isSettingsChanged,
} from './live/change-events.js';

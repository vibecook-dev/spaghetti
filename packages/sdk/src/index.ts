/**
 * `@vibecook/spaghetti-sdk` — the public API.
 *
 * ## The allowlist
 *
 * This file is an allowlist, not a re-export of whatever happens to be in
 * `src/`. RFC 012 landing plan §4 makes it one because the alternative already
 * happened: `export *` of 25 hand-written contract modules put 580 symbols on
 * the public API that no consumer ever imported, and every one of them then had
 * to be kept working. The rule that replaced it:
 *
 * > Every export below belongs to a named consumer. If you cannot name the
 * > consumer, it does not go here — put it in the module and import it
 * > directly from repository code.
 *
 * `scripts/code_shape/check_code_shape.py` counts the export statements in this
 * file and fails when the count grows. That is a ratchet, not a budget: adding
 * a group is a deliberate act that lowers something else or updates the
 * baseline on purpose.
 *
 * `export *` is allowed only for the barrels named below, each of which is
 * itself curated. It is never allowed for a `contracts/` module — those are
 * internal wire validators, and the lanes replacing their callers delete them.
 *
 * ## Consumers
 *
 * | Group | Consumer | Notes |
 * | --- | --- | --- |
 * | Domain and API DTOs | playground, CLI | `types/`, `api`, data query shapes |
 * | Transport-neutral client | playground, CLI, IPC hosts | `client/`, `observation` |
 * | Native engine surface | playground, CLI | `native` |
 * | Legacy live tail | **Chopsticks** (pinned) | see below |
 * | Session observer | **Chopsticks** | `observeSession` + generated events |
 * | VibeField Phase A | **VibeField** | generated identity + durable watermark |
 * | Presentation helpers | playground | source display metadata |
 * | Settings | CLI (`spag doctor`) | engine selection, db path |
 * | Change events | playground | live invalidation |
 *
 * Chopsticks pins this package and imports exactly `watchSessionTranscript`,
 * `SessionMessage`, and `SessionTranscriptTail`. Those three are load-bearing
 * for a downstream release and do not change until `observeSession` ships and
 * Chopsticks has migrated — the landing plan gives it one release of overlap.
 *
 * The retired TypeScript ingestion/query implementation is deliberately not
 * reachable from this entry point. Repository-only differential tests use
 * `legacy-oracle.ts` directly.
 */

// ── Domain and API DTOs (types erase from runtime bundles) ─────────────────
export * from './types/index.js';
export * from './api.js';
export * from './data/segment-types.js';
export * from './data/summary-types.js';
export * from './data/timeline-query.js';

// ── Transport-neutral async client and the sole-owner production service ───
export * from './client/index.js';
export * from './observation.js';

// ── Low-level native engine lifecycle and its typed query contract ─────────
export * from './native.js';

// ── Legacy live tail — Chopsticks pins these three names ───────────────────
// Superseded by `observeSession` below. Kept working for one release after
// Chopsticks migrates, then removed together with its allowlist entry in
// `scripts/architecture/rfc011-legacy-boundaries.json`.
// `SessionMessage` reaches consumers through `./types/index.js` above.
export { watchSessionTranscript } from './live/session-tail.js';
export type {
  SessionTranscriptEvent,
  SessionTranscriptTail,
  WatchSessionTranscriptOptions,
} from './live/session-tail.js';

// ── Store-free session observer (RFC 012 landing plan §3.1) ────────────────
// The replacement for `watchSessionTranscript`: one attachment to one session
// tree, all eleven semantic families plus the control events, and no database.
export {
  isSemanticEvent,
  observeSession,
  type ObserveSessionOptions,
  type ObserveSessionRequest,
  type ObserverEvent,
  type SemanticObserverEvent,
  type SessionObserver,
  type SessionObserverStatus,
} from './observe-session.js';
// The members of that union, so a consumer can name the shape a handler takes,
// and the per-family value types `SemanticEvent.value` carries — a handler
// signature should be able to say `ToolRevisionFact`, not re-derive it with
// `Extract<NonNullable<SemanticEvent['value']>, …>`.
// Generated from Rust by `pnpm generate:types`; never edited by hand.
export type {
  ActorAttribution,
  ActorRef,
  ClosedEvent,
  FamilyManifestEntry,
  ObjectCoverage,
  ObserverBarrier,
  ObserverErrorEvent,
  ObserverEventId,
  ObserverFamily,
  ObserverPhase,
  OverflowEvent,
  OverflowReason,
  ResetEvent,
  SemanticEvent,
  SemanticOperation,
  SourceErrorEvent,
  SourcePosition,
  UnknownEvidenceEvent,
  ActorAffiliationDimension,
  ActorAffiliationRevisionFact,
  ActorAffiliationState,
  ActorRunRevisionFact,
  ActorRunRole,
  CanonicalEntityKey,
  CanonicalFactId,
  ContentBlockRevisionFact,
  ContentBlockRevisionValue,
  ContractCompleteness,
  EffectiveStateDimension,
  EffectiveStateEvidenceKind,
  EffectiveStateRevisionFact,
  EffectiveStateValueAuthority,
  EffectiveStateValueProvenance,
  FactRevisionId,
  MessageRevisionFact,
  MessageRevisionRole,
  NativeCompactionPhase,
  NativeProgressState,
  NativeQueueOperation,
  NativeRuntimeMarkerProvenance,
  NativeRuntimeMarkerRevisionFact,
  NativeRuntimeMarkerValue,
  PlanRevisionFact,
  QualifiedTimestamp,
  QualifiedUnknownReason,
  QualifiedValue,
  QualifiedValueQuality,
  RuntimeSemanticValue,
  TaskLifecycleState,
  TaskRevisionFact,
  TimestampQuality,
  ToolRevisionFact,
  ToolRevisionKind,
  UsageBucketsV2,
  UsageResponseIdentity,
  UsageRevisionV2Fact,
  UsageValueAuthority,
  UsageValueProvenance,
  UserInputKind,
  UserInputLifecycleState,
  UserInputOperation,
  UserInputOption,
  UserInputQuestion,
  UserInputRequestRevisionFact,
} from './generated/index.js';

// ── VibeField Phase A — generated identity and durable watermark ───────────
export {
  isSameEntity,
  isSameNativeIdentity,
  isSameRevision,
  isSameSnapshot,
  queryWatermark,
  type DurableQueryWatermark,
  type ExternalEntityRef,
  type NativeIdentity,
  type ProjectRef,
  type SemanticRevisionRef,
  type SessionRef,
} from './vibefield.js';

// ── Presentation-only source metadata. These never inspect source data. ────
export { sourceReportsPerMessageTokens, sourceDisplayName, sourceDisplayRoot } from './sources/capabilities.js';

// ── Settings: engine selection, readable for upgrade diagnostics ───────────
// Production observation always uses Rust; no root export constructs a
// TypeScript owner.
export {
  type IngestEngine,
  type SpaghettiSettings,
  readSettings,
  writeSettings,
  resolveEngine,
  defaultDbPathForEngine,
  settingsPath,
} from './settings.js';

// ── Change events: compatibility invalidation for presentation clients ─────
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

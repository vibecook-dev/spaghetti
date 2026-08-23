/**
 * VibeField Phase A surface (RFC 012 landing plan §3.2).
 *
 * Nothing here is a hand-written mirror of native output:
 *
 * - the identity contracts are re-exported straight from `./generated/`,
 *   which `pnpm generate:types` produces from the Rust definitions in
 *   `crates/spaghetti-napi/src/adapter/semantic.rs`;
 * - the durable watermark is derived structurally from the napi-generated
 *   engine result types, so renaming the Rust field breaks this file at
 *   compile time rather than at runtime in VibeField.
 *
 * The only original code is comparison: references are structural values, so
 * `===` is always false between two decodings of the same reference and every
 * consumer would otherwise write the same field-by-field check.
 */

import type {
  ExternalEntityRef,
  NativeIdentity,
  SemanticRevisionRef,
  TimelinePage as EngineTimelinePage,
} from './generated/index.js';

export type { ExternalEntityRef, NativeIdentity, SemanticRevisionRef };

/**
 * A session's topology-independent identity. Identical to
 * {@link ExternalEntityRef}; the alias names the role, and RFC 012A is
 * explicit that a session reference is not a different *kind* of reference.
 */
export type SessionRef = ExternalEntityRef;

/** A project's topology-independent identity. See {@link SessionRef}. */
export type ProjectRef = ExternalEntityRef;

/**
 * The durable commit watermark every snapshot-consistent query result carries.
 *
 * Derived from the napi-generated `EngineTimelinePage` rather than declared, so
 * it cannot drift from the engine. All 32 durable query results share this
 * field; the timeline page is simply the one this type points at.
 */
export type DurableQueryWatermark = Pick<EngineTimelinePage, 'atCommitSeq'>;

/**
 * Reads the commit watermark a query result was computed at.
 *
 * Two results with equal watermarks describe the same durable snapshot, so
 * pages joined across calls are consistent exactly when this value matches.
 */
export function queryWatermark(result: DurableQueryWatermark): number {
  return result.atCommitSeq;
}

/**
 * Whether two query results describe the same durable snapshot, and may
 * therefore be joined without re-reading either.
 */
export function isSameSnapshot(left: DurableQueryWatermark, right: DurableQueryWatermark): boolean {
  return left.atCommitSeq === right.atCommitSeq;
}

/**
 * Whether two entity references name the same entity.
 *
 * Compares the contract version too: references minted under different
 * versions are not comparable, and silently treating them as equal is the
 * identity conflict RFC 012A §3.2 requires to stay explicit.
 */
export function isSameEntity(left: ExternalEntityRef, right: ExternalEntityRef): boolean {
  return (
    left.external_entity_reference_version === right.external_entity_reference_version &&
    left.entity_key === right.entity_key
  );
}

/** Whether two semantic revision references name the same revision. */
export function isSameRevision(left: SemanticRevisionRef, right: SemanticRevisionRef): boolean {
  return (
    left.semantic_reference_contract_version === right.semantic_reference_contract_version &&
    left.fact_revision_id === right.fact_revision_id
  );
}

/**
 * Whether two native identities are the same claim.
 *
 * Namespace is part of the identity: the same `native_id` issued by two agent
 * products is two identities, never one.
 */
export function isSameNativeIdentity(left: NativeIdentity, right: NativeIdentity): boolean {
  return left.native_namespace === right.native_namespace && left.native_id === right.native_id;
}

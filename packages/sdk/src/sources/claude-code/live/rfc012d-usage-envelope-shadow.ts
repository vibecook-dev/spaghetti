/**
 * Crate-private RFC 012D usage-envelope comparison helper.
 *
 * Consumes the Rust-produced scoped usage envelope fixture and records
 * occurrence-scoped event_id plus SemanticRevisionRef. It does not call the
 * N-API observer or parse native JSON payloads.
 */

import { readFileSync } from 'node:fs';

import type { SemanticRevisionRef } from '../../../contracts/rfc012a.js';
import {
  parseScopedUsageEnvelope,
  parseScopedUsageEnvelopeContext,
  type ScopedUsageEnvelope,
} from '../../../contracts/rfc012d-usage-envelope.js';

export interface ScopedUsageShadowRecord {
  eventId: string;
  semanticRevisionRef: SemanticRevisionRef;
  factId: string;
  operation: 'upsert' | 'retract';
}

interface UsageEnvelopeFixture {
  context: unknown;
  upsert: unknown;
  reset_retraction: unknown;
}

const fixtureUrl = new URL(
  '../../../../../../crates/spaghetti-napi/fixtures/contracts/rfc012d-scoped-usage-envelope-v1.json',
  import.meta.url,
);

export function shadowRecordFromUsageEnvelope(envelope: ScopedUsageEnvelope): ScopedUsageShadowRecord {
  return {
    eventId: envelope.event_id,
    semanticRevisionRef: envelope.semantic_revision_ref,
    factId: envelope.event.fact_id,
    operation: envelope.event.operation,
  };
}

export function rfc012dUsageEnvelopeShadowRecords(): readonly ScopedUsageShadowRecord[] {
  const fixture = JSON.parse(readFileSync(fixtureUrl, 'utf8')) as UsageEnvelopeFixture;
  const context = parseScopedUsageEnvelopeContext(fixture.context);
  return [
    shadowRecordFromUsageEnvelope(parseScopedUsageEnvelope(fixture.upsert, context)),
    shadowRecordFromUsageEnvelope(parseScopedUsageEnvelope(fixture.reset_retraction, context)),
  ];
}

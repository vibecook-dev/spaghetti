/** Complete portable outer union for RFC 012D event families.
 *
 * Known families route through their strict specialist parsers. The additive
 * branch is accepted only with a caller-held negotiated unknown-wire
 * selection and remains a bounded uninterpreted value.
 */

import { ContractValidationError } from './rfc012a.js';
import {
  SCOPED_KNOWN_ENVELOPE_CONTRACT_VERSION,
  parseScopedObservationKnownEnvelope,
  type ScopedObservationKnownEnvelope,
} from './rfc012d-known-envelope.js';
import {
  parseObservationUnknownWireContractSelectionForExpected,
  parseObservationUnknownWireEvent,
  type ObservationUnknownWireContractSelection,
  type ObservationUnknownWireEvent,
} from './rfc012d-unknown-wire.js';
import { parseObservationContractSelectionForExpected, type ObservationContractSelection } from './rfc012d.js';
import { parseScopedUsageEnvelopeContext, type ScopedUsageEnvelopeContext } from './rfc012d-usage-envelope.js';

export const SCOPED_OBSERVATION_EVENT_UNION_CONTRACT_VERSION = 1 as const;

type KnownAsEventUnion<T> = T extends {
  family: infer Family;
  context: infer Context;
  event: infer Event;
}
  ? {
      scoped_observation_event_union_contract_version: typeof SCOPED_OBSERVATION_EVENT_UNION_CONTRACT_VERSION;
      family: Family;
      context: Context;
      event: Event;
    }
  : never;

export type ScopedObservationEventEnvelope =
  | KnownAsEventUnion<ScopedObservationKnownEnvelope>
  | {
      scoped_observation_event_union_contract_version: typeof SCOPED_OBSERVATION_EVENT_UNION_CONTRACT_VERSION;
      family: 'unknown_wire_event';
      context: ScopedUsageEnvelopeContext;
      event: ObservationUnknownWireEvent;
    };

type UnknownRecord = Record<string, unknown>;

function exactRecord(value: unknown, fields: readonly string[], label: string): UnknownRecord {
  if (value === null || typeof value !== 'object' || Array.isArray(value)) {
    throw new ContractValidationError(`${label} must be an object`);
  }
  const prototype = Object.getPrototypeOf(value);
  if (prototype !== Object.prototype && prototype !== null) {
    throw new ContractValidationError(`${label} must be a plain JSON object`);
  }
  const input = value as UnknownRecord;
  let count = 0;
  for (const field in input) {
    if (!Object.prototype.hasOwnProperty.call(input, field)) continue;
    count += 1;
    if (!fields.includes(field)) throw new ContractValidationError(`${label} contains unknown field ${field}`);
  }
  for (const field of fields) {
    if (!Object.prototype.hasOwnProperty.call(input, field)) {
      throw new ContractValidationError(`${label} is missing field ${field}`);
    }
  }
  if (count !== fields.length) throw new ContractValidationError(`${label} has an invalid field count`);
  return input;
}

function sameSelection(left: ObservationContractSelection, right: ObservationContractSelection): void {
  parseObservationContractSelectionForExpected(left, right);
}

/**
 * Parses the complete outer union. `unknownSelectionInput` is ignored for a
 * known family, but is mandatory and exact for `unknown_wire_event`.
 */
export function parseScopedObservationEventEnvelope(
  value: unknown,
  unknownSelectionInput?: ObservationUnknownWireContractSelection,
): ScopedObservationEventEnvelope {
  const input = exactRecord(
    value,
    ['scoped_observation_event_union_contract_version', 'family', 'context', 'event'],
    'scoped observation event envelope',
  );
  if (input.scoped_observation_event_union_contract_version !== SCOPED_OBSERVATION_EVENT_UNION_CONTRACT_VERSION) {
    throw new ContractValidationError('unsupported scoped observation event-union contract version');
  }

  if (input.family === 'unknown_wire_event') {
    if (unknownSelectionInput === undefined) {
      throw new ContractValidationError('unknown-wire event preservation was not negotiated');
    }
    const selection = parseObservationUnknownWireContractSelectionForExpected(
      unknownSelectionInput,
      unknownSelectionInput,
    );
    const context = parseScopedUsageEnvelopeContext(input.context);
    sameSelection(context.contract_selection, selection.observation_selection);
    const event = parseObservationUnknownWireEvent(input.event, selection);
    const source = event.envelope_provenance.source;
    if (
      !context.authorized_sources.some(
        (candidate) =>
          candidate.instance_key === source.instance_key &&
          candidate.stream_key === source.stream_key &&
          candidate.object_key === source.object_key,
      )
    ) {
      throw new ContractValidationError('unknown-wire provenance source is outside caller-held context');
    }
    return {
      scoped_observation_event_union_contract_version: SCOPED_OBSERVATION_EVENT_UNION_CONTRACT_VERSION,
      family: 'unknown_wire_event',
      context,
      event,
    };
  }

  const known = parseScopedObservationKnownEnvelope({
    scoped_known_envelope_contract_version: SCOPED_KNOWN_ENVELOPE_CONTRACT_VERSION,
    family: input.family,
    context: input.context,
    event: input.event,
  });
  return {
    scoped_observation_event_union_contract_version: SCOPED_OBSERVATION_EVENT_UNION_CONTRACT_VERSION,
    family: known.family,
    context: known.context,
    event: known.event,
  } as ScopedObservationEventEnvelope;
}

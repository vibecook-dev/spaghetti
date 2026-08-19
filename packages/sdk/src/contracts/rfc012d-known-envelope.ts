/** Strict dispatcher for the RFC 012D event families implemented today.
 *
 * This is intentionally not the complete additive event union. Unknown family
 * preservation requires a separately negotiated bounded `unknown_wire_event`
 * carrier; until then this parser rejects unknown discriminators.
 */

import { ContractValidationError } from './rfc012a.js';
import {
  parseScopedActorEnvelope,
  parseScopedActorEnvelopeContext,
  type ScopedActorEnvelope,
  type ScopedActorEnvelopeContext,
} from './rfc012d-actor-envelope.js';
import {
  parseScopedArtifactAvailabilityEnvelope,
  parseScopedArtifactAvailabilityEnvelopeContext,
  type ScopedArtifactAvailabilityEnvelope,
  type ScopedArtifactAvailabilityEnvelopeContext,
} from './rfc012d-artifact-availability-envelope.js';
import {
  parseScopedCompletionEnvelope,
  parseScopedCompletionEnvelopeContext,
  type ScopedCompletionEnvelope,
  type ScopedCompletionEnvelopeContext,
} from './rfc012d-completion-envelope.js';
import {
  parseScopedContinuityEnvelope,
  parseScopedContinuityEnvelopeContext,
  type ScopedContinuityEnvelope,
  type ScopedContinuityEnvelopeContext,
} from './rfc012d-continuity-envelope.js';
import {
  parseScopedSourceEnvelope,
  type ScopedSourceEnvelope,
  type ScopedSourceEnvelopeContext,
} from './rfc012d-source-envelope.js';
import {
  parseScopedUsageEnvelope,
  parseScopedUsageEnvelopeContext,
  type ScopedUsageEnvelope,
  type ScopedUsageEnvelopeContext,
} from './rfc012d-usage-envelope.js';

export const SCOPED_KNOWN_ENVELOPE_CONTRACT_VERSION = 1 as const;

export type ScopedObservationKnownEnvelope =
  | {
      scoped_known_envelope_contract_version: typeof SCOPED_KNOWN_ENVELOPE_CONTRACT_VERSION;
      family: 'usage';
      context: ScopedUsageEnvelopeContext;
      event: ScopedUsageEnvelope;
    }
  | {
      scoped_known_envelope_contract_version: typeof SCOPED_KNOWN_ENVELOPE_CONTRACT_VERSION;
      family: 'actor';
      context: ScopedActorEnvelopeContext;
      event: ScopedActorEnvelope;
    }
  | {
      scoped_known_envelope_contract_version: typeof SCOPED_KNOWN_ENVELOPE_CONTRACT_VERSION;
      family: 'source';
      context: ScopedSourceEnvelopeContext;
      event: ScopedSourceEnvelope;
    }
  | {
      scoped_known_envelope_contract_version: typeof SCOPED_KNOWN_ENVELOPE_CONTRACT_VERSION;
      family: 'artifact_availability';
      context: ScopedArtifactAvailabilityEnvelopeContext;
      event: ScopedArtifactAvailabilityEnvelope;
    }
  | {
      scoped_known_envelope_contract_version: typeof SCOPED_KNOWN_ENVELOPE_CONTRACT_VERSION;
      family: 'completion';
      context: ScopedCompletionEnvelopeContext;
      event: ScopedCompletionEnvelope;
    }
  | {
      scoped_known_envelope_contract_version: typeof SCOPED_KNOWN_ENVELOPE_CONTRACT_VERSION;
      family: 'continuity';
      context: ScopedContinuityEnvelopeContext;
      event: ScopedContinuityEnvelope;
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
  const unknown = Object.keys(input).find((field) => !fields.includes(field));
  if (unknown !== undefined) {
    throw new ContractValidationError(`${label} contains unknown field ${unknown}`);
  }
  const missing = fields.find((field) => !Object.prototype.hasOwnProperty.call(input, field));
  if (missing !== undefined) {
    throw new ContractValidationError(`${label} is missing field ${missing}`);
  }
  return input;
}

/**
 * Dispatches only the six frozen specialist families. Every branch parses the
 * caller-held context first and then consumes the event against that exact
 * context; the repeated selection/root/source authority cannot be learned
 * from the received event itself.
 */
export function parseScopedObservationKnownEnvelope(value: unknown): ScopedObservationKnownEnvelope {
  const input = exactRecord(
    value,
    ['scoped_known_envelope_contract_version', 'family', 'context', 'event'],
    'scoped known envelope',
  );
  if (input.scoped_known_envelope_contract_version !== SCOPED_KNOWN_ENVELOPE_CONTRACT_VERSION) {
    throw new ContractValidationError('unsupported scoped known-envelope contract version');
  }

  switch (input.family) {
    case 'usage': {
      const context = parseScopedUsageEnvelopeContext(input.context);
      return {
        scoped_known_envelope_contract_version: SCOPED_KNOWN_ENVELOPE_CONTRACT_VERSION,
        family: 'usage',
        context,
        event: parseScopedUsageEnvelope(input.event, context),
      };
    }
    case 'actor': {
      const context = parseScopedActorEnvelopeContext(input.context);
      return {
        scoped_known_envelope_contract_version: SCOPED_KNOWN_ENVELOPE_CONTRACT_VERSION,
        family: 'actor',
        context,
        event: parseScopedActorEnvelope(input.event, context),
      };
    }
    case 'source': {
      const context = parseScopedUsageEnvelopeContext(input.context);
      return {
        scoped_known_envelope_contract_version: SCOPED_KNOWN_ENVELOPE_CONTRACT_VERSION,
        family: 'source',
        context,
        event: parseScopedSourceEnvelope(input.event, context),
      };
    }
    case 'artifact_availability': {
      const context = parseScopedArtifactAvailabilityEnvelopeContext(input.context);
      return {
        scoped_known_envelope_contract_version: SCOPED_KNOWN_ENVELOPE_CONTRACT_VERSION,
        family: 'artifact_availability',
        context,
        event: parseScopedArtifactAvailabilityEnvelope(input.event, context),
      };
    }
    case 'completion': {
      const context = parseScopedCompletionEnvelopeContext(input.context);
      return {
        scoped_known_envelope_contract_version: SCOPED_KNOWN_ENVELOPE_CONTRACT_VERSION,
        family: 'completion',
        context,
        event: parseScopedCompletionEnvelope(input.event, context),
      };
    }
    case 'continuity': {
      const context = parseScopedContinuityEnvelopeContext(input.context);
      return {
        scoped_known_envelope_contract_version: SCOPED_KNOWN_ENVELOPE_CONTRACT_VERSION,
        family: 'continuity',
        context,
        event: parseScopedContinuityEnvelope(input.event, context),
      };
    }
    default:
      throw new ContractValidationError(
        'unsupported scoped known-envelope family; bounded unknown-wire preservation is not negotiated',
      );
  }
}

/** Attachment-bound RFC 012D close acknowledgement.
 *
 * The public observer method remains `close(): Promise<void>`. This DTO is the
 * internal portable completion proof behind that promise. It exposes no native
 * lifecycle counters or applied-state claims, and it is consumed only against
 * caller-held attachment context issued by the trusted native observer handle.
 */

import { ContractValidationError, parseOpaqueContractReference, type OpaqueContractReference } from './rfc012a.js';
import { parseObservationContractSelectionForExpected, type ObservationContractSelection } from './rfc012d.js';
import { parseScopedUsageRoot, type ScopedUsageRoot } from './rfc012d-usage-envelope.js';

export const SCOPED_CLOSE_RECEIPT_CONTRACT_VERSION = 1 as const;

const MAX_ADAPTER_ID_BYTES = 128;
const textEncoder = new TextEncoder();
type UnknownRecord = Record<string, unknown>;

export interface ScopedCloseRoot extends ScopedUsageRoot {
  adapter_id: string;
  source_instance_key: OpaqueContractReference;
}

export interface ScopedCloseContext {
  contract_selection: ObservationContractSelection;
  root: ScopedCloseRoot;
  attachment_ref: OpaqueContractReference;
  close_request_id: OpaqueContractReference;
}

export interface ScopedCloseReceipt {
  scoped_close_receipt_contract_version: typeof SCOPED_CLOSE_RECEIPT_CONTRACT_VERSION;
  contract_selection: ObservationContractSelection;
  root: ScopedCloseRoot;
  attachment_ref: OpaqueContractReference;
  close_request_id: OpaqueContractReference;
  outcome: 'closed';
}

function record(value: unknown, label: string): UnknownRecord {
  if (value === null || typeof value !== 'object' || Array.isArray(value)) {
    throw new ContractValidationError(`${label} must be an object`);
  }
  const prototype = Object.getPrototypeOf(value);
  if (prototype !== Object.prototype && prototype !== null) {
    throw new ContractValidationError(`${label} must be a plain JSON object`);
  }
  return value as UnknownRecord;
}

function exactRecord(value: unknown, fields: readonly string[], label: string): UnknownRecord {
  const input = record(value, label);
  const known = new Set(fields);
  for (const key of Object.keys(input)) {
    if (!known.has(key)) throw new ContractValidationError(`${label} contains unknown field ${key}`);
  }
  for (const field of fields) {
    if (!Object.hasOwn(input, field)) throw new ContractValidationError(`${label} is missing field ${field}`);
  }
  return input;
}

function adapterId(value: unknown): string {
  if (
    typeof value !== 'string' ||
    value.length === 0 ||
    value.length > MAX_ADAPTER_ID_BYTES ||
    value.trim() !== value ||
    textEncoder.encode(value).byteLength > MAX_ADAPTER_ID_BYTES
  ) {
    throw new ContractValidationError('close root adapter_id is not a bounded canonical identifier');
  }
  return value;
}

function fixedOpaque(value: unknown, label: string): OpaqueContractReference {
  const parsed = parseOpaqueContractReference(value, label);
  const encoded = parsed.slice(3);
  const standard = encoded.replace(/-/g, '+').replace(/_/g, '/');
  const padded = standard.padEnd(Math.ceil(standard.length / 4) * 4, '=');
  let binary: string;
  try {
    binary = atob(padded);
  } catch {
    throw new ContractValidationError(`${label} is not canonical base64url`);
  }
  let roundTrip = '';
  for (let index = 0; index < binary.length; index += 1) {
    roundTrip += String.fromCharCode(binary.charCodeAt(index));
  }
  const canonical = btoa(roundTrip).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/g, '');
  if (binary.length !== 32 || canonical !== encoded) {
    throw new ContractValidationError(`${label} must contain exactly 32 canonical bytes`);
  }
  return parsed;
}

function parseRoot(value: unknown): ScopedCloseRoot {
  const fields = [
    'adapter_id',
    'source_instance_key',
    'session_ref',
    'session_key',
    'root_actor_run_key',
    'native_session_claim',
  ];
  const input = exactRecord(value, fields, 'scoped close root');
  const common = parseScopedUsageRoot({
    session_ref: input.session_ref,
    session_key: input.session_key,
    root_actor_run_key: input.root_actor_run_key,
    native_session_claim: input.native_session_claim,
  });
  fixedOpaque(common.session_key, 'close root session key');
  fixedOpaque(common.session_ref.entity_key, 'close root external session key');
  fixedOpaque(common.root_actor_run_key, 'close root actor run key');
  if (common.native_session_claim !== null) {
    fixedOpaque(common.native_session_claim.entity_ref.entity_key, 'close root native claim key');
  }
  return {
    adapter_id: adapterId(input.adapter_id),
    source_instance_key: fixedOpaque(input.source_instance_key, 'close root source instance key'),
    ...common,
  };
}

function canonicalEqual(left: unknown, right: unknown): boolean {
  return JSON.stringify(left) === JSON.stringify(right);
}

export function parseScopedCloseContext(value: unknown): ScopedCloseContext {
  const input = exactRecord(
    value,
    ['contract_selection', 'root', 'attachment_ref', 'close_request_id'],
    'scoped close context',
  );
  return {
    contract_selection: parseObservationContractSelectionForExpected(
      input.contract_selection,
      input.contract_selection,
    ),
    root: parseRoot(input.root),
    attachment_ref: fixedOpaque(input.attachment_ref, 'close attachment reference'),
    close_request_id: fixedOpaque(input.close_request_id, 'close request id'),
  };
}

export function parseScopedCloseReceipt(value: unknown, expectedContextInput: unknown): ScopedCloseReceipt {
  const expected = parseScopedCloseContext(expectedContextInput);
  const input = exactRecord(
    value,
    [
      'scoped_close_receipt_contract_version',
      'contract_selection',
      'root',
      'attachment_ref',
      'close_request_id',
      'outcome',
    ],
    'scoped close receipt',
  );
  if (input.scoped_close_receipt_contract_version !== SCOPED_CLOSE_RECEIPT_CONTRACT_VERSION) {
    throw new ContractValidationError('unsupported scoped close receipt contract version');
  }
  if (input.outcome !== 'closed') {
    throw new ContractValidationError('scoped close receipt is not a completed close acknowledgement');
  }
  const contractSelection = parseObservationContractSelectionForExpected(
    input.contract_selection,
    expected.contract_selection,
  );
  const root = parseRoot(input.root);
  const attachmentRef = fixedOpaque(input.attachment_ref, 'close attachment reference');
  const closeRequestId = fixedOpaque(input.close_request_id, 'close request id');
  if (
    !canonicalEqual(root, expected.root) ||
    attachmentRef !== expected.attachment_ref ||
    closeRequestId !== expected.close_request_id
  ) {
    throw new ContractValidationError('scoped close receipt does not match caller-held attachment context');
  }
  return {
    scoped_close_receipt_contract_version: SCOPED_CLOSE_RECEIPT_CONTRACT_VERSION,
    contract_selection: contractSelection,
    root,
    attachment_ref: attachmentRef,
    close_request_id: closeRequestId,
    outcome: 'closed',
  };
}

/** Typed owner for the strict, store-free RFC 012D native transport. */

import { Buffer } from 'node:buffer';

import { ContractValidationError } from './contracts/rfc012a.js';
import { parseScopedCapabilitySnapshot } from './contracts/rfc012d-capability-snapshot.js';
import {
  type ScopedBootstrapCompletionBarrier,
  type ScopedResyncCompletionBarrier,
} from './contracts/rfc012d-completion-envelope.js';
import {
  parseScopedObservationEventEnvelope,
  type ScopedObservationEventEnvelope,
} from './contracts/rfc012d-event-envelope.js';
import { parseScopedObservationWatermark, type ScopedObservationWatermark } from './contracts/rfc012d-watermark.js';
import {
  parseObservationContractRequest,
  type ObservationCapabilities,
  type ObservationContractRequest,
} from './contracts/rfc012d.js';
import { openNativeScopedObservationJson, type NativeScopedObservation } from './native.js';

export const SCOPED_OBSERVATION_REQUEST_CONTRACT_VERSION = 1 as const;

const MAX_REQUEST_JSON_BYTES = 256 * 1024;
const MAX_NATIVE_RESPONSE_JSON_BYTES = 8 * 1024 * 1024;
const MAX_CONFIGURED_ROOTS = 16;
const MAX_KNOWN_OBJECTS = 64;
const MAX_RELATION_IDENTITY_INPUTS = 32;
const MAX_IDENTITY_BYTES = 64 * 1024;
const MAX_ROOT_BYTES = 32 * 1024;
const MAX_ROOT_BYTES_TOTAL = 256 * 1024;
const identifierPattern = /^[a-z0-9][a-z0-9._-]{0,127}$/;
const portableFactFamilies = new Set(['runtime.actor-affiliation', 'runtime.actor-run', 'runtime.usage-v2']);
const textEncoder = new TextEncoder();

type UnknownRecord = Record<string, unknown>;

export interface SessionObservationRootIdentity {
  sessionIdentityKey: Uint8Array;
  rootRunIdentityKey?: Uint8Array | null;
  relationIdentityInputs: Readonly<Record<string, Uint8Array>>;
}

/**
 * First public request profile: exact known append objects, no persistence,
 * and only fact families frozen by the native transport offer.
 */
export interface SessionObservationRequest {
  adapterId: string;
  configuredRoots: readonly string[];
  programId: string;
  knownObjectRelativePaths: Readonly<Record<string, string>>;
  rootIdentity: SessionObservationRootIdentity;
  contractRequest: ObservationContractRequest;
}

export type SessionObservationApply = (event: ScopedObservationEventEnvelope) => void | Promise<void>;

export interface SessionObserver {
  capabilities(): ObservationCapabilities;
  events(): AsyncIterable<ScopedObservationEventEnvelope>;
  consume(apply: SessionObservationApply): Promise<void>;
  poll(): Promise<ScopedObservationWatermark>;
  /** Consumer-ready boundary: the bootstrap completion envelope was applied. */
  ready(): Promise<ScopedBootstrapCompletionBarrier>;
  /** Consumer-applied full-snapshot replacement boundary. */
  resync(): Promise<ScopedResyncCompletionBarrier>;
  close(): Promise<void>;
}

export class ScopedObservationRequestError extends ContractValidationError {
  constructor() {
    super('invalid scoped observation request');
    this.name = 'ScopedObservationRequestError';
  }
}

export class ScopedObservationTransportError extends Error {
  constructor(message = 'native scoped observation violated its transport contract') {
    super(message);
    this.name = 'ScopedObservationTransportError';
  }
}

interface Deferred<T> {
  readonly promise: Promise<T>;
  resolve(value: T): void;
  reject(error: unknown): void;
  settled(): boolean;
}

function deferred<T>(): Deferred<T> {
  let resolvePromise!: (value: T) => void;
  let rejectPromise!: (error: unknown) => void;
  let isSettled = false;
  const promise = new Promise<T>((resolve, reject) => {
    resolvePromise = resolve;
    rejectPromise = reject;
  });
  // Terminal close must be able to settle an optional waiter even when the
  // application never called ready()/resync(). Keep the original promise
  // observable while marking that retained rejection as handled.
  void promise.catch(() => undefined);
  return {
    promise,
    resolve(value) {
      if (isSettled) return;
      isSettled = true;
      resolvePromise(value);
    },
    reject(error) {
      if (isSettled) return;
      isSettled = true;
      rejectPromise(error);
    },
    settled: () => isSettled,
  };
}

class NativeSessionObserver implements SessionObserver {
  readonly #native: NativeScopedObservation;
  readonly #capabilities: ObservationCapabilities;
  readonly #bootstrapApplied = deferred<ScopedBootstrapCompletionBarrier>();
  #resyncApplied: Deferred<ScopedResyncCompletionBarrier> | undefined;
  #readyPromise: Promise<ScopedBootstrapCompletionBarrier> | undefined;
  #resyncPromise: Promise<ScopedResyncCompletionBarrier> | undefined;
  #closePromise: Promise<void> | undefined;
  #eventsClaimed = false;
  #closed = false;
  #deliveryPending = false;

  constructor(native: NativeScopedObservation) {
    this.#native = native;
    this.#capabilities = deepFreeze(parseCapabilitiesResponse(native.capabilitiesJson()));
  }

  capabilities(): ObservationCapabilities {
    return this.#capabilities;
  }

  async *events(): AsyncIterableIterator<ScopedObservationEventEnvelope> {
    this.#assertOpen();
    if (this.#eventsClaimed) {
      throw new ScopedObservationTransportError('scoped observation events already have a consumer');
    }
    this.#eventsClaimed = true;
    try {
      while (!this.#closed) {
        const json = await this.#native.nextEventJson();
        if (json === null) {
          if (this.#closePromise !== undefined) return;
          throw new ScopedObservationTransportError('native scoped observation ended unexpectedly');
        }
        const event = deepFreeze(parseEventResponse(json));
        const completion = completionBarrier(event);
        this.#deliveryPending = true;
        yield event;
        await this.#native.acknowledgeApplied();
        this.#deliveryPending = false;
        if (completion?.kind === 'bootstrap') {
          this.#bootstrapApplied.resolve(completion.barrier);
        } else if (completion?.kind === 'resync') {
          this.#resyncApplied?.resolve(completion.barrier);
        }
      }
    } catch (error) {
      this.#rejectPending(error);
      throw error;
    } finally {
      if (!this.#closed || this.#deliveryPending) {
        await this.close();
      }
    }
  }

  async consume(apply: SessionObservationApply): Promise<void> {
    for await (const event of this.events()) {
      await apply(event);
    }
  }

  async poll(): Promise<ScopedObservationWatermark> {
    this.#assertOpen();
    return deepFreeze(parsePollResponse(await this.#native.pollJson()));
  }

  ready(): Promise<ScopedBootstrapCompletionBarrier> {
    if (this.#bootstrapApplied.settled()) return this.#bootstrapApplied.promise;
    this.#assertOpen();
    this.#readyPromise ??= Promise.all([this.#native.readyOffered(), this.#bootstrapApplied.promise]).then(
      ([, barrier]) => barrier,
    );
    return this.#readyPromise;
  }

  resync(): Promise<ScopedResyncCompletionBarrier> {
    this.#assertOpen();
    if (this.#resyncPromise !== undefined) return this.#resyncPromise;
    const applied = deferred<ScopedResyncCompletionBarrier>();
    this.#resyncApplied = applied;
    this.#resyncPromise = Promise.all([this.#native.resyncOffered(), applied.promise])
      .then(([, barrier]) => barrier)
      .finally(() => {
        if (this.#resyncApplied === applied) {
          this.#resyncApplied = undefined;
          this.#resyncPromise = undefined;
        }
      });
    return this.#resyncPromise;
  }

  close(): Promise<void> {
    if (this.#closePromise !== undefined) return this.#closePromise;
    this.#closePromise = this.#native.close().then(
      () => {
        this.#closed = true;
        this.#deliveryPending = false;
        this.#rejectPending(new ScopedObservationTransportError('scoped observation closed before completion'));
      },
      (error: unknown) => {
        this.#closed = true;
        this.#rejectPending(error);
        throw error;
      },
    );
    return this.#closePromise;
  }

  #assertOpen(): void {
    if (this.#closed || this.#closePromise !== undefined) {
      throw new ScopedObservationTransportError('scoped observation is closed');
    }
  }

  #rejectPending(error: unknown): void {
    this.#bootstrapApplied.reject(error);
    this.#resyncApplied?.reject(error);
  }
}

/** Open one typed RFC 012D observer over the strict native JSON owner. */
export async function observeSession(request: SessionObservationRequest): Promise<SessionObserver> {
  const requestJson = encodeScopedObservationRequestForTransport(request);
  const native = await openNativeScopedObservationJson(requestJson);
  try {
    return createSessionObserverForTransport(native);
  } catch (error) {
    await native.close().catch(() => undefined);
    throw error;
  }
}

/** @internal Test seam; not re-exported from the package root. */
export function createSessionObserverForTransport(native: NativeScopedObservation): SessionObserver {
  return new NativeSessionObserver(native);
}

/** @internal Test seam; not re-exported from the package root. */
export function encodeScopedObservationRequestForTransport(request: SessionObservationRequest): string {
  try {
    const adapterId = identifier(request.adapterId);
    const programId = identifier(request.programId);
    const configuredRoots = stringArray(request.configuredRoots, MAX_CONFIGURED_ROOTS, true);
    let rootBytes = 0;
    for (const root of configuredRoots) {
      const bytes = boundedUtf8(root, MAX_ROOT_BYTES);
      rootBytes += bytes;
      if (!Number.isSafeInteger(rootBytes) || rootBytes > MAX_ROOT_BYTES_TOTAL) failRequest();
    }
    const knownObjectRelativePaths = stringMap(request.knownObjectRelativePaths, MAX_KNOWN_OBJECTS, (key) =>
      identifier(key),
    );
    const relationIdentityInputs = binaryMap(request.rootIdentity.relationIdentityInputs, MAX_RELATION_IDENTITY_INPUTS);
    const contractRequest = parseObservationContractRequest(request.contractRequest);
    for (const [family, versions] of Object.entries(contractRequest.contract_versions.fact_family_versions)) {
      if (!portableFactFamilies.has(family) || !versions.includes(1)) failRequest();
    }
    const wire = {
      scoped_observation_request_contract_version: SCOPED_OBSERVATION_REQUEST_CONTRACT_VERSION,
      adapter_id: adapterId,
      persistence: 'none',
      scope_mode: 'exact_known_objects',
      configured_roots: configuredRoots,
      program_id: programId,
      known_object_relative_paths: knownObjectRelativePaths,
      root_identity: {
        session_identity_key: encodedIdentity(request.rootIdentity.sessionIdentityKey),
        root_run_identity_key:
          request.rootIdentity.rootRunIdentityKey == null
            ? null
            : encodedIdentity(request.rootIdentity.rootRunIdentityKey),
        relation_identity_inputs: relationIdentityInputs,
      },
      contract_request: contractRequest,
    };
    const json = JSON.stringify(wire);
    if (json.length > MAX_REQUEST_JSON_BYTES || textEncoder.encode(json).byteLength > MAX_REQUEST_JSON_BYTES) {
      failRequest();
    }
    return json;
  } catch (error) {
    if (error instanceof ScopedObservationRequestError) throw error;
    throw new ScopedObservationRequestError();
  }
}

function parseCapabilitiesResponse(json: string): ObservationCapabilities {
  const input = exactNativeRecord(parseNativeJson(json), ['context', 'snapshot']);
  try {
    return parseScopedCapabilitySnapshot(input.snapshot, input.context).observation_capabilities;
  } catch {
    throw new ScopedObservationTransportError();
  }
}

function parseEventResponse(json: string): ScopedObservationEventEnvelope {
  try {
    return parseScopedObservationEventEnvelope(parseNativeJson(json));
  } catch {
    throw new ScopedObservationTransportError();
  }
}

function parsePollResponse(json: string): ScopedObservationWatermark {
  const input = exactNativeRecord(parseNativeJson(json), ['context', 'watermark']);
  try {
    return parseScopedObservationWatermark(input.watermark, input.context);
  } catch {
    throw new ScopedObservationTransportError();
  }
}

function completionBarrier(
  envelope: ScopedObservationEventEnvelope,
):
  | { kind: 'bootstrap'; barrier: ScopedBootstrapCompletionBarrier }
  | { kind: 'resync'; barrier: ScopedResyncCompletionBarrier }
  | undefined {
  if (envelope.family !== 'completion') return undefined;
  const completion = envelope.event.event;
  return completion.kind === 'observer_bootstrap_complete'
    ? { kind: 'bootstrap', barrier: completion.barrier }
    : { kind: 'resync', barrier: completion.barrier };
}

function parseNativeJson(json: string): unknown {
  if (
    typeof json !== 'string' ||
    json.length === 0 ||
    json.length > MAX_NATIVE_RESPONSE_JSON_BYTES ||
    textEncoder.encode(json).byteLength > MAX_NATIVE_RESPONSE_JSON_BYTES
  ) {
    throw new ScopedObservationTransportError();
  }
  try {
    return JSON.parse(json) as unknown;
  } catch {
    throw new ScopedObservationTransportError();
  }
}

function exactNativeRecord(value: unknown, fields: readonly string[]): UnknownRecord {
  if (value === null || typeof value !== 'object' || Array.isArray(value)) {
    throw new ScopedObservationTransportError();
  }
  const input = value as UnknownRecord;
  const keys = Object.keys(input);
  if (keys.length !== fields.length || keys.some((key) => !fields.includes(key))) {
    throw new ScopedObservationTransportError();
  }
  for (const field of fields) {
    if (!Object.hasOwn(input, field)) throw new ScopedObservationTransportError();
  }
  return input;
}

function identifier(value: unknown): string {
  if (typeof value !== 'string' || !identifierPattern.test(value)) failRequest();
  return value;
}

function boundedUtf8(value: unknown, maxBytes = MAX_IDENTITY_BYTES): number {
  if (typeof value !== 'string' || value.length === 0 || value.length > maxBytes) failRequest();
  for (let index = 0; index < value.length; index += 1) {
    const code = value.charCodeAt(index);
    if (code === 0 || code < 0x20 || code === 0x7f) failRequest();
  }
  const bytes = textEncoder.encode(value).byteLength;
  if (bytes === 0 || bytes > maxBytes) failRequest();
  return bytes;
}

function stringArray(value: readonly string[], maxItems: number, unique: boolean): string[] {
  if (!Array.isArray(value) || value.length === 0 || value.length > maxItems) failRequest();
  const result = value.map((item) => {
    boundedUtf8(item, MAX_ROOT_BYTES);
    return item;
  });
  if (unique && new Set(result).size !== result.length) failRequest();
  return result;
}

function stringMap(
  value: Readonly<Record<string, string>>,
  maxItems: number,
  validateKey: (key: string) => string,
): Record<string, string> {
  plainRequestRecord(value);
  const result: Record<string, string> = Object.create(null) as Record<string, string>;
  let count = 0;
  let totalBytes = 0;
  for (const key in value) {
    if (!Object.hasOwn(value, key)) continue;
    count += 1;
    if (count > maxItems) failRequest();
    const item = value[key];
    const canonicalKey = validateKey(key);
    totalBytes += boundedUtf8(canonicalKey, 128) + boundedUtf8(item);
    if (!Number.isSafeInteger(totalBytes) || totalBytes > MAX_REQUEST_JSON_BYTES) failRequest();
    result[canonicalKey] = item;
  }
  if (count === 0) failRequest();
  return result;
}

function binaryMap(value: Readonly<Record<string, Uint8Array>>, maxItems: number): Record<string, string> {
  plainRequestRecord(value);
  const result: Record<string, string> = Object.create(null) as Record<string, string>;
  let count = 0;
  let totalEncodedBytes = 0;
  for (const key in value) {
    if (!Object.hasOwn(value, key)) continue;
    count += 1;
    if (count > maxItems) failRequest();
    const canonicalKey = identifier(key);
    const encoded = encodedIdentity(value[key]);
    totalEncodedBytes += canonicalKey.length + encoded.length;
    if (!Number.isSafeInteger(totalEncodedBytes) || totalEncodedBytes > MAX_REQUEST_JSON_BYTES) failRequest();
    result[canonicalKey] = encoded;
  }
  if (count === 0) failRequest();
  return result;
}

function plainRequestRecord(value: unknown): asserts value is UnknownRecord {
  if (value === null || typeof value !== 'object' || Array.isArray(value)) failRequest();
  const prototype = Object.getPrototypeOf(value);
  if (prototype !== Object.prototype && prototype !== null) failRequest();
}

function encodedIdentity(value: unknown): string {
  if (!(value instanceof Uint8Array) || value.byteLength === 0 || value.byteLength > MAX_IDENTITY_BYTES) {
    failRequest();
  }
  return Buffer.from(value.buffer, value.byteOffset, value.byteLength).toString('base64url');
}

function deepFreeze<T>(value: T): T {
  if (value !== null && typeof value === 'object' && !Object.isFrozen(value)) {
    for (const key of Object.keys(value)) {
      deepFreeze((value as UnknownRecord)[key]);
    }
    Object.freeze(value);
  }
  return value;
}

function failRequest(): never {
  throw new ScopedObservationRequestError();
}

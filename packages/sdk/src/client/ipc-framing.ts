import {
  isSpaghettiClientErrorCode,
  isSpaghettiClientMethod,
  type AnySpaghettiProtocolRequest,
  type SpaghettiProtocolError,
  type SpaghettiProtocolResponse,
  type SpaghettiTransportConnectRequest,
  type SpaghettiTransportConnectResponse,
} from './protocol.js';

export const SPAGHETTI_IPC_WIRE_VERSION = 1 as const;
export const SPAGHETTI_IPC_MAX_FRAME_BYTES = 24 * 1024 * 1024;

const MAGIC = new Uint8Array([0x53, 0x50, 0x41, 0x47]); // SPAG
const HEADER_BYTES = 10;
const textEncoder = new TextEncoder();
const textDecoder = new TextDecoder('utf-8', { fatal: true });

enum FrameKind {
  Connect = 1,
  ConnectResult = 2,
  Request = 3,
  Response = 4,
  Cancel = 5,
  Close = 6,
}

export type SpaghettiIpcFrame =
  | { type: 'connect'; request: SpaghettiTransportConnectRequest }
  | ({ type: 'connect-result'; ok: true } & SpaghettiTransportConnectResponse)
  | { type: 'connect-result'; ok: false; error: SpaghettiProtocolError }
  | { type: 'request'; request: AnySpaghettiProtocolRequest }
  | { type: 'response'; response: SpaghettiProtocolResponse }
  | { type: 'cancel'; requestId: number }
  | { type: 'close' };

export class SpaghettiIpcFrameError extends Error {
  constructor(message: string) {
    super(message);
    this.name = 'SpaghettiIpcFrameError';
  }
}

export function encodeSpaghettiIpcFrame(frame: SpaghettiIpcFrame): Uint8Array {
  const { kind, body } = frameBody(frame);
  let json: string;
  try {
    json = JSON.stringify(body);
  } catch {
    throw new SpaghettiIpcFrameError('IPC frame body is not JSON serializable.');
  }
  const payload = textEncoder.encode(json);
  const totalBytes = HEADER_BYTES + payload.byteLength;
  if (totalBytes > SPAGHETTI_IPC_MAX_FRAME_BYTES) {
    throw new SpaghettiIpcFrameError(
      `IPC frame is ${totalBytes} bytes; limit is ${SPAGHETTI_IPC_MAX_FRAME_BYTES} bytes.`,
    );
  }

  const encoded = new Uint8Array(totalBytes);
  encoded.set(MAGIC, 0);
  encoded[4] = SPAGHETTI_IPC_WIRE_VERSION;
  encoded[5] = kind;
  new DataView(encoded.buffer).setUint32(6, payload.byteLength, false);
  encoded.set(payload, HEADER_BYTES);
  return encoded;
}

export function decodeSpaghettiIpcFrame(encoded: Uint8Array): SpaghettiIpcFrame {
  if (encoded.byteLength < HEADER_BYTES) throw new SpaghettiIpcFrameError('IPC frame is shorter than its header.');
  if (encoded.byteLength > SPAGHETTI_IPC_MAX_FRAME_BYTES) {
    throw new SpaghettiIpcFrameError('IPC frame exceeds the configured byte limit.');
  }
  for (let index = 0; index < MAGIC.length; index += 1) {
    if (encoded[index] !== MAGIC[index]) throw new SpaghettiIpcFrameError('IPC frame magic is invalid.');
  }
  if (encoded[4] !== SPAGHETTI_IPC_WIRE_VERSION) {
    throw new SpaghettiIpcFrameError(`Unsupported IPC wire version: ${encoded[4]}.`);
  }
  const kind = encoded[5] as FrameKind;
  const declaredBytes = new DataView(encoded.buffer, encoded.byteOffset, encoded.byteLength).getUint32(6, false);
  if (declaredBytes !== encoded.byteLength - HEADER_BYTES) {
    throw new SpaghettiIpcFrameError('IPC frame payload length does not match its header.');
  }

  let body: unknown;
  try {
    body = JSON.parse(textDecoder.decode(encoded.subarray(HEADER_BYTES)));
  } catch {
    throw new SpaghettiIpcFrameError('IPC frame payload is not valid UTF-8 JSON.');
  }
  return parseFrame(kind, body);
}

function frameBody(frame: SpaghettiIpcFrame): { kind: FrameKind; body: unknown } {
  switch (frame.type) {
    case 'connect':
      return { kind: FrameKind.Connect, body: frame.request };
    case 'connect-result': {
      if ('error' in frame) {
        return { kind: FrameKind.ConnectResult, body: { ok: false, error: frame.error } };
      }
      const result: SpaghettiTransportConnectResponse = {
        transportKind: frame.transportKind,
        protocolVersion: frame.protocolVersion,
        queryContractVersion: frame.queryContractVersion,
        engineVersion: frame.engineVersion,
        methods: frame.methods,
      };
      return { kind: FrameKind.ConnectResult, body: { ok: true, result } };
    }
    case 'request':
      return { kind: FrameKind.Request, body: frame.request };
    case 'response':
      return { kind: FrameKind.Response, body: frame.response };
    case 'cancel':
      return { kind: FrameKind.Cancel, body: { requestId: frame.requestId } };
    case 'close':
      return { kind: FrameKind.Close, body: {} };
  }
}

function parseFrame(kind: FrameKind, body: unknown): SpaghettiIpcFrame {
  switch (kind) {
    case FrameKind.Connect:
      return { type: 'connect', request: parseConnectRequest(body) };
    case FrameKind.ConnectResult:
      return parseConnectResult(body);
    case FrameKind.Request:
      return { type: 'request', request: parseRequest(body) };
    case FrameKind.Response:
      return { type: 'response', response: parseResponse(body) };
    case FrameKind.Cancel:
      return { type: 'cancel', requestId: requiredPositiveInteger(record(body).requestId, 'cancel.requestId') };
    case FrameKind.Close:
      record(body);
      return { type: 'close' };
    default:
      throw new SpaghettiIpcFrameError(`Unknown IPC frame kind: ${kind}.`);
  }
}

function parseConnectRequest(value: unknown): SpaghettiTransportConnectRequest {
  const candidate = record(value);
  return {
    clientName: requiredString(candidate.clientName, 'connect.clientName'),
    protocolVersions: numberArray(candidate.protocolVersions, 'connect.protocolVersions'),
    queryContractVersions: numberArray(candidate.queryContractVersions, 'connect.queryContractVersions'),
  };
}

function parseConnectResult(value: unknown): SpaghettiIpcFrame {
  const candidate = record(value);
  if (candidate.ok === false) {
    return { type: 'connect-result', ok: false, error: parseProtocolError(candidate.error) };
  }
  if (candidate.ok !== true) throw new SpaghettiIpcFrameError('connect-result.ok must be boolean.');
  const result = record(candidate.result);
  const methods = array(result.methods, 'connect-result.methods').map((method) => {
    if (!isSpaghettiClientMethod(method))
      throw new SpaghettiIpcFrameError(`Unknown advertised method: ${String(method)}.`);
    return method;
  });
  return {
    type: 'connect-result',
    ok: true,
    transportKind: requiredString(result.transportKind, 'connect-result.transportKind'),
    protocolVersion: requiredPositiveInteger(result.protocolVersion, 'connect-result.protocolVersion'),
    queryContractVersion: requiredPositiveInteger(result.queryContractVersion, 'connect-result.queryContractVersion'),
    engineVersion: requiredString(result.engineVersion, 'connect-result.engineVersion'),
    methods,
  };
}

function parseRequest(value: unknown): AnySpaghettiProtocolRequest {
  const candidate = record(value);
  if (!isSpaghettiClientMethod(candidate.method)) {
    throw new SpaghettiIpcFrameError(`Unknown request method: ${String(candidate.method)}.`);
  }
  return {
    protocolVersion: requiredPositiveInteger(candidate.protocolVersion, 'request.protocolVersion'),
    queryContractVersion: requiredPositiveInteger(candidate.queryContractVersion, 'request.queryContractVersion'),
    requestId: requiredPositiveInteger(candidate.requestId, 'request.requestId'),
    method: candidate.method,
    payload: candidate.payload as never,
  } as AnySpaghettiProtocolRequest;
}

function parseResponse(value: unknown): SpaghettiProtocolResponse {
  const candidate = record(value);
  const base = {
    protocolVersion: requiredPositiveInteger(candidate.protocolVersion, 'response.protocolVersion'),
    queryContractVersion: requiredPositiveInteger(candidate.queryContractVersion, 'response.queryContractVersion'),
    requestId: requiredPositiveInteger(candidate.requestId, 'response.requestId'),
  };
  if (candidate.ok === true) return { ...base, ok: true, result: candidate.result as never };
  if (candidate.ok === false) return { ...base, ok: false, error: parseProtocolError(candidate.error) };
  throw new SpaghettiIpcFrameError('response.ok must be boolean.');
}

function parseProtocolError(value: unknown): SpaghettiProtocolError {
  const candidate = record(value);
  const code = requiredString(candidate.code, 'error.code');
  if (!isSpaghettiClientErrorCode(code)) {
    throw new SpaghettiIpcFrameError(`Unknown protocol error code: ${code}.`);
  }
  return {
    code,
    message: requiredString(candidate.message, 'error.message'),
    ...optionalString(candidate, 'field'),
    ...optionalString(candidate, 'reason'),
    ...optionalString(candidate, 'capability'),
    ...optionalString(candidate, 'projection'),
    ...optionalNumber(candidate, 'retryAfterMs'),
    ...optionalString(candidate, 'diagnosticId'),
    ...optionalNumber(candidate, 'currentCommitSeq'),
    ...optionalChangeCursor(candidate, 'oldestAvailable'),
  };
}

function optionalChangeCursor(
  candidate: Record<string, unknown>,
  field: string,
): { oldestAvailable?: { commitSeq: number; ordinal: number } } {
  const value = candidate[field];
  if (value === undefined) return {};
  const cursor = record(value);
  const commitSeq = requiredNonNegativeInteger(cursor.commitSeq, `${field}.commitSeq`);
  const ordinal = requiredNonNegativeInteger(cursor.ordinal, `${field}.ordinal`);
  if (ordinal > 0xffff_ffff) {
    throw new SpaghettiIpcFrameError(`${field}.ordinal exceeds uint32.`);
  }
  return { oldestAvailable: { commitSeq, ordinal } };
}

function record(value: unknown): Record<string, unknown> {
  if (!value || typeof value !== 'object' || Array.isArray(value)) {
    throw new SpaghettiIpcFrameError('IPC frame body must be an object.');
  }
  return value as Record<string, unknown>;
}

function array(value: unknown, field: string): unknown[] {
  if (!Array.isArray(value)) throw new SpaghettiIpcFrameError(`${field} must be an array.`);
  return value;
}

function numberArray(value: unknown, field: string): number[] {
  const result = array(value, field).map((item) => requiredPositiveInteger(item, field));
  if (result.length === 0) throw new SpaghettiIpcFrameError(`${field} must not be empty.`);
  return result;
}

function requiredPositiveInteger(value: unknown, field: string): number {
  if (!Number.isSafeInteger(value) || (value as number) < 1) {
    throw new SpaghettiIpcFrameError(`${field} must be a positive safe integer.`);
  }
  return value as number;
}

function requiredNonNegativeInteger(value: unknown, field: string): number {
  if (!Number.isSafeInteger(value) || (value as number) < 0) {
    throw new SpaghettiIpcFrameError(`${field} must be a non-negative safe integer.`);
  }
  return value as number;
}

function requiredString(value: unknown, field: string): string {
  if (typeof value !== 'string' || value.length === 0) {
    throw new SpaghettiIpcFrameError(`${field} must be a non-empty string.`);
  }
  return value;
}

function optionalString(candidate: Record<string, unknown>, field: string): Record<string, string> {
  const value = candidate[field];
  if (value === undefined) return {};
  if (typeof value !== 'string') throw new SpaghettiIpcFrameError(`error.${field} must be a string.`);
  return { [field]: value };
}

function optionalNumber(candidate: Record<string, unknown>, field: string): Record<string, number> {
  const value = candidate[field];
  if (value === undefined) return {};
  if (typeof value !== 'number' || !Number.isFinite(value)) {
    throw new SpaghettiIpcFrameError(`error.${field} must be a finite number.`);
  }
  return { [field]: value };
}

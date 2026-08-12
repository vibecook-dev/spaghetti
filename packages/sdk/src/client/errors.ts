import {
  SpaghettiClientError,
  isSpaghettiClientErrorCode,
  type SpaghettiClientErrorCode,
  type SpaghettiProtocolError,
} from './protocol.js';
import { EngineUnavailableError } from '../native.js';

const CURSOR_SCOPE = /scope|filter|project|session|workflow|inbox/i;

export function cancelledProtocolError(): SpaghettiProtocolError {
  return { code: 'cancelled', message: 'The query was cancelled.' };
}

export function closedProtocolError(): SpaghettiProtocolError {
  return { code: 'transport_closed', message: 'The Spaghetti transport is closed.' };
}

export function protocolMismatchError(reason: string): SpaghettiProtocolError {
  return {
    code: 'protocol_mismatch',
    message: 'The client and transport do not share a supported protocol contract.',
    reason,
  };
}

/** Convert transport-specific failures without exposing raw internal details. */
export function normalizeTransportError(error: unknown, diagnosticId: string): SpaghettiProtocolError {
  if (error instanceof SpaghettiClientError) return protocolErrorFromClient(error);
  if (error instanceof EngineUnavailableError) {
    return {
      code: 'transport_unavailable',
      message: 'The embedded Spaghetti engine is unavailable on this host.',
      reason: 'native_addon_unavailable',
    };
  }
  if (isProtocolErrorLike(error)) {
    return {
      code: error.code,
      message: typeof error.message === 'string' ? error.message : 'The query failed.',
      ...(typeof error.field === 'string' ? { field: error.field } : {}),
      ...(typeof error.reason === 'string' ? { reason: error.reason } : {}),
      ...(typeof error.capability === 'string' ? { capability: error.capability } : {}),
      ...(typeof error.projection === 'string' ? { projection: error.projection } : {}),
      ...(typeof error.retryAfterMs === 'number' ? { retryAfterMs: error.retryAfterMs } : {}),
      ...(typeof error.diagnosticId === 'string' ? { diagnosticId: error.diagnosticId } : {}),
    };
  }

  const name = error instanceof Error ? error.name : '';
  const raw = error instanceof Error ? error.message : String(error);
  const lower = raw.toLowerCase();

  if (name === 'AbortError' || /\babort|\bcancel/.test(lower)) return cancelledProtocolError();
  if (/cursor/.test(lower)) {
    const reason = /expired/.test(lower)
      ? 'expired'
      : CURSOR_SCOPE.test(raw)
        ? 'scope_mismatch'
        : /decode|base64|malformed|invalid/.test(lower)
          ? 'malformed'
          : 'invalid';
    return { code: 'cursor_invalid', message: 'The query cursor is invalid or expired.', reason };
  }
  if (/unsupported/.test(lower)) {
    return { code: 'unsupported_capability', message: 'The requested capability is not supported.' };
  }
  if (/projection/.test(lower) && /pending|not ready|stale/.test(lower)) {
    return { code: 'projection_pending', message: 'The requested projection is not ready.' };
  }
  if (/deadline|timed? out|timeout/.test(lower)) {
    return { code: 'deadline_exceeded', message: 'The query deadline was exceeded.' };
  }
  if (/shutting down|stopping|stopped|not accepting quer|(?:query|worker pool|engine).*closed/.test(lower)) {
    return { code: 'engine_stopping', message: 'The Spaghetti engine is stopping.' };
  }
  if (/database.*(?:busy|locked|already owned)|already owned by|sqlite_busy|sqlite_locked/.test(lower)) {
    return { code: 'database_busy', message: 'The Spaghetti database is busy.' };
  }
  if (/invalid|required|must be|must not|out of range|unknown .* id|not-a-/.test(lower)) {
    return { code: 'invalid_request', message: 'The query request is invalid.', reason: 'validation_failed' };
  }
  return {
    code: 'internal',
    message: `The query failed internally. Diagnostic: ${diagnosticId}.`,
    diagnosticId,
  };
}

export function clientError(error: SpaghettiProtocolError, requestId?: number): SpaghettiClientError {
  return new SpaghettiClientError(error, requestId);
}

export function isClientErrorCode(value: unknown): value is SpaghettiClientErrorCode {
  return isSpaghettiClientErrorCode(value);
}

function protocolErrorFromClient(error: SpaghettiClientError): SpaghettiProtocolError {
  return {
    code: error.code,
    message: error.message,
    ...(error.field ? { field: error.field } : {}),
    ...(error.reason ? { reason: error.reason } : {}),
    ...(error.capability ? { capability: error.capability } : {}),
    ...(error.projection ? { projection: error.projection } : {}),
    ...(error.retryAfterMs !== undefined ? { retryAfterMs: error.retryAfterMs } : {}),
    ...(error.diagnosticId ? { diagnosticId: error.diagnosticId } : {}),
  };
}

function isProtocolErrorLike(error: unknown): error is Record<string, unknown> & { code: SpaghettiClientErrorCode } {
  return !!error && typeof error === 'object' && isClientErrorCode((error as { code?: unknown }).code);
}

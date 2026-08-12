const requestEncoder = new TextEncoder();

/** JSON byte count shared by every transport-side request bound. */
export function encodedRequestBytes(payload: unknown): number {
  try {
    return requestEncoder.encode(JSON.stringify(payload ?? null)).byteLength;
  } catch {
    return Number.POSITIVE_INFINITY;
  }
}

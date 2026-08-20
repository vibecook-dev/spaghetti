/** Internal RFC 012A/012C JSON preflight and canonical-string helpers.
 *
 * Not a package-root export. Fixture parsers re-export only ContractValidationError
 * and keep these implementation helpers unbarreled.
 */

export class ContractValidationError extends Error {
  constructor(message: string) {
    super(message);
    this.name = 'ContractValidationError';
  }
}

const UTF8_ENCODER = new TextEncoder();

export const MAX_SEMANTIC_FIXTURE_JSON_BYTES = 1024 * 1024;
export const MAX_SEMANTIC_FIXTURE_DEPTH = 16;
export const MAX_SEMANTIC_FIXTURE_NODES = 4_096;

/** Unicode White_Space, matching Rust `char::is_whitespace`. U+FEFF is excluded. */
export function isRustWhitespaceCodePoint(code: number): boolean {
  switch (code) {
    case 0x09:
    case 0x0a:
    case 0x0b:
    case 0x0c:
    case 0x0d:
    case 0x20:
    case 0x85:
    case 0xa0:
    case 0x1680:
    case 0x2028:
    case 0x2029:
    case 0x202f:
    case 0x205f:
    case 0x3000:
      return true;
    default:
      return code >= 0x2000 && code <= 0x200a;
  }
}

export function hasSurroundingRustWhitespace(value: string): boolean {
  if (value.length === 0) return false;
  return (
    isRustWhitespaceCodePoint(value.charCodeAt(0)) || isRustWhitespaceCodePoint(value.charCodeAt(value.length - 1))
  );
}

export function assertNoUnpairedUtf16Surrogates(value: string, label: string): void {
  for (let index = 0; index < value.length; index += 1) {
    const code = value.charCodeAt(index);
    if (code >= 0xd800 && code <= 0xdbff) {
      const next = index + 1 < value.length ? value.charCodeAt(index + 1) : -1;
      if (next < 0xdc00 || next > 0xdfff) {
        throw new ContractValidationError(`${label} contains an unpaired UTF-16 surrogate`);
      }
      index += 1;
    } else if (code >= 0xdc00 && code <= 0xdfff) {
      throw new ContractValidationError(`${label} contains an unpaired UTF-16 surrogate`);
    }
  }
}

function accountSemanticFixtureGraph(value: unknown, depth: number, nodes: { count: number }): void {
  if (depth > MAX_SEMANTIC_FIXTURE_DEPTH) {
    throw new ContractValidationError(`semantic fixture JSON exceeds depth ${MAX_SEMANTIC_FIXTURE_DEPTH}`);
  }
  nodes.count += 1;
  if (nodes.count > MAX_SEMANTIC_FIXTURE_NODES) {
    throw new ContractValidationError(`semantic fixture JSON exceeds ${MAX_SEMANTIC_FIXTURE_NODES} nodes`);
  }
  if (Array.isArray(value)) {
    for (let index = 0; index < value.length; index += 1) {
      accountSemanticFixtureGraph(value[index], depth + 1, nodes);
    }
    return;
  }
  if (value !== null && typeof value === 'object') {
    const record = value as Record<string, unknown>;
    for (const key in record) {
      if (!Object.hasOwn(record, key)) continue;
      accountSemanticFixtureGraph(record[key], depth + 1, nodes);
    }
  }
}

export function assertSemanticFixtureGraph(value: unknown): void {
  accountSemanticFixtureGraph(value, 1, { count: 0 });
}

function isDigit(code: number): boolean {
  return code >= 0x30 && code <= 0x39;
}

function isHexDigit(code: number): boolean {
  return (code >= 0x30 && code <= 0x39) || (code >= 0x41 && code <= 0x46) || (code >= 0x61 && code <= 0x66);
}

function hexDigitValue(code: number): number {
  if (code >= 0x30 && code <= 0x39) return code - 0x30;
  if (code >= 0x41 && code <= 0x46) return code - 0x41 + 10;
  return code - 0x61 + 10;
}

function parseUnicodeEscape(json: string, hexStart: number): { value: number; next: number } | undefined {
  if (hexStart + 4 > json.length) return undefined;
  let value = 0;
  for (let offset = 0; offset < 4; offset += 1) {
    const code = json.charCodeAt(hexStart + offset);
    if (!isHexDigit(code)) return undefined;
    value = (value << 4) | hexDigitValue(code);
  }
  return { value, next: hexStart + 4 };
}

function consumeUtf16CodeUnit(json: string, index: number, label: string): number {
  const code = json.charCodeAt(index);
  if (code >= 0xd800 && code <= 0xdbff) {
    const next = index + 1 < json.length ? json.charCodeAt(index + 1) : -1;
    if (next < 0xdc00 || next > 0xdfff) {
      throw new ContractValidationError(`${label} contains an unpaired UTF-16 surrogate`);
    }
    return index + 2;
  }
  if (code >= 0xdc00 && code <= 0xdfff) {
    throw new ContractValidationError(`${label} contains an unpaired UTF-16 surrogate`);
  }
  return index + 1;
}

function consumeCanonicalIntegerLexeme(json: string, start: number): number {
  let index = start;
  if (json.charCodeAt(index) === 0x2d) {
    index += 1;
  }
  if (index >= json.length || !isDigit(json.charCodeAt(index))) {
    throw new ContractValidationError('semantic fixture JSON is not valid JSON');
  }
  if (json.charCodeAt(index) === 0x30) {
    index += 1;
  } else {
    index += 1;
    while (index < json.length && isDigit(json.charCodeAt(index))) {
      index += 1;
    }
  }
  const next = index < json.length ? json.charCodeAt(index) : 0;
  if (next === 0x2e || next === 0x65 || next === 0x45) {
    throw new ContractValidationError('semantic fixture JSON contains a noncanonical integer lexeme');
  }
  const lexeme = json.slice(start, index);
  if (lexeme === '-0' || !/^(?:0|-?[1-9][0-9]*)$/.test(lexeme)) {
    throw new ContractValidationError('semantic fixture JSON contains a noncanonical integer lexeme');
  }
  return index;
}

function scanSemanticFixtureJson(json: string): void {
  const length = json.length;
  let index = 0;
  let inString = false;
  while (index < length) {
    const code = json.charCodeAt(index);
    if (inString) {
      if (code === 0x5c) {
        index += 1;
        if (index >= length) {
          throw new ContractValidationError('semantic fixture JSON is not valid JSON');
        }
        if (json.charCodeAt(index) === 0x75) {
          const parsed = parseUnicodeEscape(json, index + 1);
          if (parsed === undefined) {
            throw new ContractValidationError('semantic fixture JSON is not valid JSON');
          }
          if (parsed.value >= 0xd800 && parsed.value <= 0xdbff) {
            if (
              parsed.next + 1 < length &&
              json.charCodeAt(parsed.next) === 0x5c &&
              json.charCodeAt(parsed.next + 1) === 0x75
            ) {
              const low = parseUnicodeEscape(json, parsed.next + 2);
              if (low !== undefined && low.value >= 0xdc00 && low.value <= 0xdfff) {
                index = low.next;
                continue;
              }
            }
            throw new ContractValidationError('semantic fixture JSON contains an unpaired UTF-16 surrogate');
          }
          if (parsed.value >= 0xdc00 && parsed.value <= 0xdfff) {
            throw new ContractValidationError('semantic fixture JSON contains an unpaired UTF-16 surrogate');
          }
          index = parsed.next;
          continue;
        }
        index += 1;
        continue;
      }
      if (code === 0x22) {
        inString = false;
        index += 1;
        continue;
      }
      index = consumeUtf16CodeUnit(json, index, 'semantic fixture JSON');
      continue;
    }
    if (code === 0x22) {
      inString = true;
      index += 1;
      continue;
    }
    if (code === 0x2d || isDigit(code)) {
      index = consumeCanonicalIntegerLexeme(json, index);
      continue;
    }
    index = consumeUtf16CodeUnit(json, index, 'semantic fixture JSON');
  }
  if (inString) {
    throw new ContractValidationError('semantic fixture JSON is not valid JSON');
  }
}

export function preflightSemanticFixtureJson(json: string): unknown {
  if (json.length === 0) {
    throw new ContractValidationError('semantic fixture JSON must not be empty');
  }
  if (json.length > MAX_SEMANTIC_FIXTURE_JSON_BYTES) {
    throw new ContractValidationError(`semantic fixture JSON exceeds ${MAX_SEMANTIC_FIXTURE_JSON_BYTES} bytes`);
  }
  scanSemanticFixtureJson(json);
  const jsonBytes = UTF8_ENCODER.encode(json);
  if (jsonBytes.length > MAX_SEMANTIC_FIXTURE_JSON_BYTES) {
    throw new ContractValidationError(`semantic fixture JSON exceeds ${MAX_SEMANTIC_FIXTURE_JSON_BYTES} bytes`);
  }
  let value: unknown;
  try {
    value = JSON.parse(json);
  } catch {
    throw new ContractValidationError('semantic fixture JSON is not valid JSON');
  }
  assertSemanticFixtureGraph(value);
  return value;
}

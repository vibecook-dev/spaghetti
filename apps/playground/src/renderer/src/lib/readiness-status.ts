/** Complete readiness-vector interpretation for the playground indicator. */

import type { SpaghettiReadiness, SpaghettiReadinessField } from '@vibecook/spaghetti-sdk/observation';

type ReadinessEntry = readonly [name: string, field: SpaghettiReadinessField];

export function readinessFields(readiness: SpaghettiReadiness): ReadinessEntry[] {
  return [
    ['catalog', readiness.catalog],
    ['history', readiness.history],
    ['usage', readiness.usage],
    ['capabilities', readiness.capabilities],
    ['artifacts', readiness.artifacts],
    ['search', readiness.search],
  ];
}

export function readinessIsConverging(readiness: SpaghettiReadiness): boolean {
  return readinessFields(readiness).some(([, field]) => field.state === 'indexing' || field.state === 'pending');
}

function fieldDetail([name, field]: ReadinessEntry): string {
  return `${name}: ${field.detail ?? field.state}`;
}

/** Compact label and complete hover detail for every non-ready field. */
export function readinessIndicator(readiness: SpaghettiReadiness): [label: string | null, detail: string | null] {
  const fields = readinessFields(readiness);
  const unhealthy = fields.filter(([, field]) => field.state === 'degraded' || field.state === 'unavailable');
  const converging = fields.filter(([, field]) => field.state === 'indexing' || field.state === 'pending');
  if (unhealthy.length > 0) {
    return ['degraded', [...unhealthy, ...converging].map(fieldDetail).join('\n')];
  }
  if (converging.length > 0) return ['indexing…', converging.map(fieldDetail).join('\n')];
  return [null, null];
}

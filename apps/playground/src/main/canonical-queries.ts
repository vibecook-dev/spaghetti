/** Product-facing canonical reads. Every operation crosses SpaghettiClient. */

import type { SpaghettiClient, SpaghettiClientResponseMap } from '@vibecook/spaghetti-sdk/client';

export interface ObservationClientProvider {
  getObservationClient(): Promise<SpaghettiClient>;
}

/** First renderer-safe canonical DTO: catalog statistics need no legacy adaptation. */
export async function readCanonicalStats(
  provider: ObservationClientProvider,
): Promise<SpaghettiClientResponseMap['getStats']> {
  const client = await provider.getObservationClient();
  return await client.getStats();
}

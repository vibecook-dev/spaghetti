import { SpaghettiClientError, type SpaghettiClient } from './protocol.js';
import { clientError, normalizeTransportError } from './errors.js';
import { openSpaghettiClient } from './client.js';
import { openNapiTransport, type OpenNapiTransportOptions } from './napi-transport.js';

export interface OpenEmbeddedSpaghettiClientOptions extends OpenNapiTransportOptions {
  clientName?: string;
}

/** Open, optionally observe, and negotiate an embedded N-API engine owner. */
export async function openEmbeddedSpaghettiClient(
  options: OpenEmbeddedSpaghettiClientOptions,
): Promise<SpaghettiClient> {
  const { clientName, ...transportOptions } = options;
  try {
    const transport = await openNapiTransport(transportOptions);
    return await openSpaghettiClient({ transport, clientName });
  } catch (error) {
    if (error instanceof SpaghettiClientError) throw error;
    throw clientError(normalizeTransportError(error, 'napi-open'));
  }
}

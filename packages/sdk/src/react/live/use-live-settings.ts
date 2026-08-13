import { useCallback } from 'react';
import type { SettingsFile } from '../../types/index.js';
import { useSpaghettiClient } from '../context.js';
import { changeBatchMatchesTopic } from './change-filter.js';
import { useAsyncSnapshot } from './use-async-snapshot.js';

export function useLiveSettings(): SettingsFile | null {
  const client = useSpaghettiClient();
  const load = useCallback(async () => (client.getSettings ? await client.getSettings() : null), [client]);
  const subscribe = useCallback(
    (invalidate: () => void) =>
      client.onChange((batch) => {
        if (changeBatchMatchesTopic(batch, { kind: 'settings' })) invalidate();
      }),
    [client],
  );
  return useAsyncSnapshot<SettingsFile | null>(null, load, subscribe).value;
}

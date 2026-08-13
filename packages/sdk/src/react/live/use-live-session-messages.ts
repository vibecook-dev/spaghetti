import { useCallback, useMemo } from 'react';
import type { SessionMessage } from '../../types/index.js';
import { useSpaghettiClient } from '../context.js';
import { changeBatchMatchesTopic } from './change-filter.js';
import { useAsyncSnapshot } from './use-async-snapshot.js';

const MAX_MESSAGES = 500;
const EMPTY_MESSAGES: SessionMessage[] = [];

export interface UseLiveSessionMessagesResult {
  messages: SessionMessage[];
  isLoading: boolean;
  error: unknown;
}

export function useLiveSessionMessages(slug: string, sessionId: string): UseLiveSessionMessagesResult {
  const client = useSpaghettiClient();
  const load = useCallback(
    async () => (await client.getSessionMessages(slug, sessionId, MAX_MESSAGES, 0)).messages,
    [client, sessionId, slug],
  );
  const subscribe = useCallback(
    (invalidate: () => void) =>
      client.onChange((batch) => {
        if (changeBatchMatchesTopic(batch, { kind: 'session', slug, sessionId })) invalidate();
      }),
    [client, sessionId, slug],
  );
  const snapshot = useAsyncSnapshot(EMPTY_MESSAGES, load, subscribe);
  return useMemo(
    () => ({ messages: snapshot.value, isLoading: snapshot.isLoading, error: snapshot.error }),
    [snapshot.error, snapshot.isLoading, snapshot.value],
  );
}

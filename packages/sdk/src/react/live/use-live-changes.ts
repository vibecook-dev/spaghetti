import { useEffect, useMemo, useState } from 'react';
import type { SegmentChangeBatch } from '../../data/segment-types.js';
import type { ChangeTopic } from '../../live/change-events.js';
import { useSpaghettiClient } from '../context.js';
import { changeBatchMatchesTopic } from './change-filter.js';

/** Return the most recent durable invalidation batch matching an optional topic. */
export function useLiveChanges(topic?: ChangeTopic): SegmentChangeBatch | null {
  const client = useSpaghettiClient();
  const [last, setLast] = useState<SegmentChangeBatch | null>(null);
  const topicKey = topic ? JSON.stringify(topic) : '';
  const stableTopic = useMemo(() => topic, [topicKey]);

  useEffect(
    () =>
      client.onChange((batch) => {
        if (changeBatchMatchesTopic(batch, stableTopic)) setLast(batch);
      }),
    [client, stableTopic],
  );

  return last;
}

/**
 * TimeGroupSeparator
 *
 * Displays a centered timestamp badge between message groups.
 */

import React, { memo } from 'react';
import { formatChatTime } from '../utils/time-grouping';

interface TimeGroupSeparatorProps {
  timestamp: string;
}

export const TimeGroupSeparator = memo(function TimeGroupSeparator({ timestamp }: TimeGroupSeparatorProps) {
  const formattedTime = formatChatTime(timestamp);

  return (
    <div className="flex justify-center py-4">
      <span className="text-[10px] font-mono px-3 py-1 rounded-full bg-muted-foreground/15 text-muted-foreground">
        {formattedTime}
      </span>
    </div>
  );
});

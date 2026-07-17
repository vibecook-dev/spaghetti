/**
 * TimelineLine Component
 *
 * Vertical connecting line between timeline items.
 */

import React, { useMemo, memo } from 'react';
import { timeline } from '../theme';

interface TimelineLineProps {
  isLast: boolean;
  dotSize?: number;
}

export const TimelineLine = memo(function TimelineLine({ isLast, dotSize = timeline.dotSize }: TimelineLineProps) {
  if (isLast) return null;

  const style = useMemo(
    () => ({
      left: dotSize / 2 - 0.5,
      top: dotSize,
      bottom: -8,
      backgroundColor: 'var(--border)',
      width: timeline.lineWidth,
    }),
    [dotSize],
  );

  return <div className="absolute opacity-30 transition-all duration-300 ease-in-out" style={style} />;
});

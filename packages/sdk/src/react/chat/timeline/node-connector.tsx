/**
 * NodeConnector Component
 *
 * SVG-based connector line between timeline nodes.
 * Supports three types: straight, curve_in (branching), curve_out (merging).
 */

import React, { memo } from 'react';
import { timeline } from '../theme';

export type ConnectorType = 'straight' | 'curve_in' | 'curve_out' | 'none';

interface NodeConnectorProps {
  startX: number;
  startY: number;
  endX: number;
  endY: number;
  type: ConnectorType;
  color: string;
  strokeWidth?: number;
}

export const NodeConnector = memo(function NodeConnector({
  startX,
  startY,
  endX,
  endY,
  type,
  color,
  strokeWidth = timeline.lineWidth,
}: NodeConnectorProps) {
  if (!Number.isFinite(endY) || type === 'none') return null;

  let d = '';
  const midY = (startY + endY) / 2;

  if (type === 'straight') {
    d = `M ${startX} ${startY} L ${endX} ${endY}`;
  } else if (type === 'curve_in') {
    d = `M ${startX} ${startY} C ${startX} ${midY}, ${endX} ${midY}, ${endX} ${endY}`;
  } else if (type === 'curve_out') {
    d = `M ${startX} ${startY} C ${startX} ${midY}, ${endX} ${midY}, ${endX} ${endY}`;
  }

  const dashArray = type === 'straight' ? '0' : timeline.dashPattern;

  return (
    <svg className="absolute top-0 left-0 w-full overflow-visible pointer-events-none z-0" style={{ height: endY }}>
      <path
        d={d}
        stroke={color}
        strokeWidth={strokeWidth}
        fill="none"
        strokeOpacity={timeline.connectorGlowOpacity}
        className="blur-[1px] transition-all duration-500 ease-in-out"
      />
      <path
        d={d}
        stroke={color}
        strokeWidth={strokeWidth}
        fill="none"
        strokeLinecap="round"
        strokeOpacity={type === 'straight' ? 0.3 : timeline.connectorStrokeOpacity}
        strokeDasharray={dashArray}
        className="transition-all duration-500 ease-in-out"
      />
    </svg>
  );
});

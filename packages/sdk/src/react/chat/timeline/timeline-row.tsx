/**
 * TimelineRow Component
 *
 * Grid-based wrapper for timeline messages.
 * Handles the rail column (icon + SVG connector) and content column.
 */

import React, { memo, useRef, useState, useLayoutEffect, type ReactNode } from 'react';
import { cn } from '../../lib/utils.js';
import { NodeConnector, type ConnectorType } from './node-connector';
import { timeline } from '../theme';

const MAIN_X = timeline.mainX;
const INDENT_X = timeline.indentX;
const ICON_OFFSET_Y = timeline.iconOffsetY;

interface TimelineRowProps {
  icon: ReactNode;
  children: ReactNode;
  indent?: 0 | 1;
  nextIndent?: 0 | 1;
  hasNext?: boolean;
  color?: string;
  isAgent?: boolean;
}

function getConnectorType(currentIndent: number, nextIndent: number): ConnectorType {
  if (currentIndent === 0 && nextIndent === 1) return 'curve_in';
  if (currentIndent === 1 && nextIndent === 0) return 'curve_out';
  return 'straight';
}

export const TimelineRow = memo(function TimelineRow({
  icon,
  children,
  indent = 0,
  nextIndent = 0,
  hasNext = false,
  color = 'var(--border)',
  isAgent = false,
}: TimelineRowProps) {
  const containerRef = useRef<HTMLDivElement>(null);
  const [rowHeight, setRowHeight] = useState(0);

  useLayoutEffect(() => {
    if (containerRef.current) {
      setRowHeight(containerRef.current.offsetHeight);
    }
  }, [children]);

  const startX = indent === 0 ? MAIN_X : INDENT_X;
  const endX = nextIndent === 0 ? MAIN_X : INDENT_X;
  const connectorType = hasNext ? getConnectorType(indent, nextIndent) : 'none';
  const targetY = rowHeight + ICON_OFFSET_Y;
  const lineColor = isAgent || indent === 1 ? timeline.agentColor : color;
  const iconSize = timeline.dotSize;

  return (
    <div
      ref={containerRef}
      className={cn('relative grid gap-4 group animate-fadeInUp')}
      style={{
        gridTemplateColumns: `${timeline.railWidth}px 1fr`,
        minHeight: '60px',
      }}
    >
      <div className="relative h-full w-full">
        {hasNext && rowHeight > 0 && (
          <NodeConnector
            startX={startX}
            startY={ICON_OFFSET_Y}
            endX={endX}
            endY={targetY}
            type={connectorType}
            color={lineColor}
          />
        )}
        <div
          className="absolute z-10 flex items-center justify-center"
          style={{
            left: startX - iconSize / 2,
            top: ICON_OFFSET_Y - iconSize / 2,
            width: iconSize,
            height: iconSize,
          }}
        >
          {icon}
        </div>
      </div>
      <div
        className="pt-5 pb-6 pr-4 min-w-0"
        style={{
          paddingLeft: indent > 0 ? '20px' : '0px',
          opacity: indent > 0 ? 0.95 : 1,
          transition: 'opacity 0.3s, padding 0.3s',
        }}
      >
        {children}
      </div>
    </div>
  );
});

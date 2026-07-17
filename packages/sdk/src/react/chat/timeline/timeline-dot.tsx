/**
 * TimelineDot Component
 *
 * Circular dot with radial glow effect for timeline items.
 */

import React, { useMemo, memo } from 'react';
import { timeline, hexToRgba } from '../theme';

interface TimelineDotProps {
  color: string;
  icon: React.ElementType;
  size?: number;
}

export const TimelineDot = memo(function TimelineDot({ color, icon: Icon, size = timeline.dotSize }: TimelineDotProps) {
  const glowStyle = useMemo(
    () => ({
      background: `radial-gradient(circle, ${hexToRgba(color, timeline.glowInnerOpacity)} ${timeline.glowInnerStop}%, ${hexToRgba(color, timeline.glowOuterOpacity)} ${timeline.glowOuterStop}%)`,
      transform: `scale(${timeline.glowSpread})`,
      filter: `blur(${timeline.glowBlur}px)`,
    }),
    [color],
  );

  return (
    <div
      className="relative z-10 flex items-center justify-center shrink-0 transition-transform duration-200 hover:scale-110"
      style={{ width: size, height: size }}
    >
      <div className="absolute inset-0 rounded-full transition-opacity duration-200" style={glowStyle} />
      <div
        className="relative w-full h-full rounded-full flex items-center justify-center shadow-lg"
        style={{ backgroundColor: color, border: `1px solid ${hexToRgba(color, 40)}` }}
      >
        <div className="absolute inset-1 rounded-full opacity-20" style={{ backgroundColor: color }} />
        <Icon size={size * 0.45} className="text-white relative z-10" />
      </div>
    </div>
  );
});

interface AssistantDotProps {
  size?: number;
}

const createAssistantGlowStyle = () => ({
  background: `radial-gradient(circle, ${hexToRgba('#D97757', timeline.glowInnerOpacity)} ${timeline.glowInnerStop}%, ${hexToRgba('#D97757', timeline.glowOuterOpacity)} ${timeline.glowOuterStop}%)`,
  transform: `scale(${timeline.glowSpread})`,
  filter: `blur(${timeline.glowBlur}px)`,
});

const assistantGlowStyle = createAssistantGlowStyle();

export const AssistantDot = memo(function AssistantDot({ size = timeline.dotSize }: AssistantDotProps) {
  return (
    <div
      className="relative z-10 flex items-center justify-center shrink-0 transition-transform duration-200 hover:scale-110"
      style={{ width: size, height: size }}
    >
      <div className="absolute inset-0 rounded-full transition-opacity duration-200" style={assistantGlowStyle} />
      <div
        className="relative w-full h-full rounded-full flex items-center justify-center shadow-lg"
        style={{ backgroundColor: 'var(--accent)', border: `1px solid ${hexToRgba('#D97757', 40)}` }}
      >
        <div className="absolute inset-1 rounded-full opacity-20" style={{ backgroundColor: '#D97757' }} />
        <span className="font-serif font-bold text-white relative z-10" style={{ fontSize: size * 0.55 }}>
          C
        </span>
      </div>
    </div>
  );
});

/**
 * Chat Utility Helpers
 *
 * Formatting functions and the useIsDark hook for runtime theme detection.
 */

import { useState, useEffect } from 'react';

// =============================================================================
// COLOR HELPERS
// =============================================================================

/**
 * Convert hex color to rgba string
 */
export function hexToRgba(hex: string, opacity: number): string {
  const r = parseInt(hex.slice(1, 3), 16);
  const g = parseInt(hex.slice(3, 5), 16);
  const b = parseInt(hex.slice(5, 7), 16);
  return `rgba(${r}, ${g}, ${b}, ${opacity / 100})`;
}

// =============================================================================
// FORMATTING HELPERS
// =============================================================================

/**
 * Format token count with K/M suffixes
 */
export function formatTokens(count: number): string {
  if (count >= 1_000_000) {
    return `${(count / 1_000_000).toFixed(1)}M`;
  }
  if (count >= 1_000) {
    return `${(count / 1_000).toFixed(1)}K`;
  }
  return count.toString();
}

/**
 * Format duration in milliseconds to human-readable string
 */
export function formatDuration(ms: number): string {
  if (ms < 1000) return `${ms}ms`;
  const seconds = ms / 1000;
  if (seconds < 60) return `${seconds.toFixed(1)}s`;
  const minutes = Math.floor(seconds / 60);
  const remainingSeconds = Math.floor(seconds % 60);
  return `${minutes}m ${remainingSeconds}s`;
}

/**
 * Format ISO timestamp to short time string
 */
export function formatTime(timestamp: string): string {
  const date = new Date(timestamp);
  return date.toLocaleTimeString('en-US', {
    hour: 'numeric',
    minute: '2-digit',
    hour12: true,
  });
}

// =============================================================================
// DARK MODE HOOK
// =============================================================================

/**
 * Hook that detects whether the current theme is dark mode.
 * Uses MutationObserver on <html> class to react to theme changes.
 *
 * Follows the same pattern as ui-v2's WidgetContainer.
 */
export function useIsDark(): boolean {
  const [isDark, setIsDark] = useState(() =>
    typeof document !== 'undefined' ? document.documentElement.classList.contains('dark') : false,
  );

  useEffect(() => {
    const check = () => {
      setIsDark(document.documentElement.classList.contains('dark'));
    };

    check();

    const observer = new MutationObserver(check);
    observer.observe(document.documentElement, {
      attributes: true,
      attributeFilter: ['class'],
    });

    return () => observer.disconnect();
  }, []);

  return isDark;
}

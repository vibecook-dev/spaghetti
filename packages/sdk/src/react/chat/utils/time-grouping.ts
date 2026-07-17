/**
 * Time Grouping Utilities
 *
 * Provides IM-style time grouping functionality for chat messages.
 * Similar to WeChat, Telegram, and other messaging apps.
 */

// =============================================================================
// CONFIGURATION
// =============================================================================

/** Default time threshold in minutes before showing a new timestamp */
export const DEFAULT_TIME_THRESHOLD_MINUTES = 5;

// =============================================================================
// HELPER FUNCTIONS
// =============================================================================

/**
 * Determine if a time separator should be shown between messages.
 *
 * @param currentTimestamp - ISO timestamp of the current message
 * @param prevTimestamp - ISO timestamp of the previous message (null for first message)
 * @param thresholdMinutes - Gap threshold in minutes (default: 5)
 * @returns true if a time separator should be displayed
 */
export function shouldShowTimestamp(
  currentTimestamp: string,
  prevTimestamp: string | null,
  thresholdMinutes: number = DEFAULT_TIME_THRESHOLD_MINUTES,
): boolean {
  // Always show for first message
  if (!prevTimestamp) return true;

  const currentTime = new Date(currentTimestamp).getTime();
  const prevTime = new Date(prevTimestamp).getTime();
  const thresholdMs = thresholdMinutes * 60 * 1000;

  return currentTime - prevTime > thresholdMs;
}

/**
 * Format a timestamp for display in time separators.
 * Uses smart formatting based on how recent the message is.
 *
 * @param timestamp - ISO timestamp string
 * @returns Formatted time string
 */
export function formatChatTime(timestamp: string): string {
  const date = new Date(timestamp);
  const now = new Date();

  // Check if same day
  const isToday = date.toDateString() === now.toDateString();

  // Check if yesterday
  const yesterday = new Date(now);
  yesterday.setDate(yesterday.getDate() - 1);
  const isYesterday = date.toDateString() === yesterday.toDateString();

  // Format time portion
  const timeStr = date.toLocaleTimeString('en-US', {
    hour: 'numeric',
    minute: '2-digit',
    hour12: true,
  });

  if (isToday) {
    return timeStr;
  }

  if (isYesterday) {
    return `Yesterday ${timeStr}`;
  }

  // Check if within this week (last 7 days)
  const daysDiff = Math.floor((now.getTime() - date.getTime()) / (1000 * 60 * 60 * 24));
  if (daysDiff < 7) {
    const weekday = date.toLocaleDateString('en-US', { weekday: 'short' });
    return `${weekday} ${timeStr}`;
  }

  // Check if same year
  const isSameYear = date.getFullYear() === now.getFullYear();
  if (isSameYear) {
    return date.toLocaleDateString('en-US', {
      month: 'short',
      day: 'numeric',
      hour: 'numeric',
      minute: '2-digit',
      hour12: true,
    });
  }

  // Full date for older messages
  return date.toLocaleDateString('en-US', {
    year: 'numeric',
    month: 'short',
    day: 'numeric',
    hour: 'numeric',
    minute: '2-digit',
    hour12: true,
  });
}

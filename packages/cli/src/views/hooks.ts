/**
 * Shared hooks — list navigation and terminal dimensions
 */

import { useState, useCallback, useEffect, useRef, type DependencyList } from 'react';
import { useStdout } from 'ink';

// ─── useListNavigation ─────────────────────────────────────────────────

export interface ListNavigationOptions {
  /** Total number of items */
  itemCount: number;
  /** Lines each item occupies (for viewport math). Default: 4 (card with blank line) */
  itemHeight?: number;
  /** Viewport height in lines. If not set, uses terminal height minus chrome. */
  viewportHeight?: number;
  /** Initial selected index. Default: 0 */
  initialIndex?: number;
}

export interface ListNavigationResult {
  selectedIndex: number;
  scrollOffset: number;
  /** Move selection up by one */
  moveUp(): void;
  /** Move selection down by one */
  moveDown(): void;
  /** Jump to a specific index */
  jumpTo(index: number): void;
}

export function useListNavigation(opts: ListNavigationOptions): ListNavigationResult {
  const { itemCount, itemHeight = 4, viewportHeight, initialIndex = 0 } = opts;
  const { stdout } = useStdout();
  const termHeight = stdout?.rows ?? 24;
  // Reserve lines for header (breadcrumb + hrule) + footer (hrule + hints)
  const effectiveViewport = viewportHeight ?? Math.max(termHeight - 6, 5);
  const visibleItems = Math.max(1, Math.floor(effectiveViewport / itemHeight));

  const [selectedIndex, setSelectedIndex] = useState(initialIndex);
  const [scrollOffset, setScrollOffset] = useState(() => {
    // If initialIndex is beyond the first viewport, scroll so it's visible
    if (initialIndex >= visibleItems) {
      return Math.max(0, initialIndex - Math.floor(visibleItems / 2));
    }
    return 0;
  });

  const clampAndScroll = useCallback(
    (newIndex: number) => {
      const clamped = Math.max(0, Math.min(newIndex, Math.max(0, itemCount - 1)));
      setSelectedIndex(clamped);
      setScrollOffset((prev) => {
        if (clamped < prev) return clamped;
        if (clamped >= prev + visibleItems) return clamped - visibleItems + 1;
        return prev;
      });
    },
    [itemCount, visibleItems],
  );

  // Re-clamp when the list length changes out from under us (live streams,
  // filter toggles). Without this the selection/scroll can point past the end
  // after the list shrinks. Growing the list leaves both untouched.
  useEffect(() => {
    const maxIndex = Math.max(0, itemCount - 1);
    setSelectedIndex((idx) => (idx > maxIndex ? maxIndex : idx));
    setScrollOffset((off) => {
      const maxOffset = Math.max(0, itemCount - visibleItems);
      return off > maxOffset ? maxOffset : off;
    });
  }, [itemCount, visibleItems]);

  const moveUp = useCallback(() => {
    clampAndScroll(selectedIndex > 0 ? selectedIndex - 1 : selectedIndex);
  }, [selectedIndex, clampAndScroll]);

  const moveDown = useCallback(() => {
    clampAndScroll(selectedIndex < itemCount - 1 ? selectedIndex + 1 : selectedIndex);
  }, [selectedIndex, itemCount, clampAndScroll]);

  const jumpTo = useCallback(
    (index: number) => {
      clampAndScroll(index);
    },
    [clampAndScroll],
  );

  return { selectedIndex, scrollOffset, moveUp, moveDown, jumpTo };
}

// ─── useTerminalSize ───────────────────────────────────────────────────

/**
 * Returns terminal dimensions and re-renders on resize.
 *
 * On resize, writes a clear-screen escape (\x1b[2J\x1b[H) to remove
 * stale characters from the previous frame size. Debounced at 50ms
 * to avoid rapid-fire clears during drag-resize.
 */
export function useTerminalSize(): { cols: number; rows: number } {
  const { stdout } = useStdout();
  const [size, setSize] = useState({
    cols: stdout?.columns ?? 80,
    rows: stdout?.rows ?? 24,
  });
  const timerRef = useRef<ReturnType<typeof setTimeout>>(undefined);

  useEffect(() => {
    if (!stdout) return;

    const onResize = () => {
      clearTimeout(timerRef.current);
      timerRef.current = setTimeout(() => {
        // Clear screen to remove stale artifacts from previous dimensions
        process.stdout.write('\x1b[2J\x1b[H');
        setSize({
          cols: stdout.columns ?? 80,
          rows: stdout.rows ?? 24,
        });
      }, 50);
    };

    stdout.on('resize', onResize);
    return () => {
      stdout.off('resize', onResize);
      clearTimeout(timerRef.current);
    };
  }, [stdout]);

  return size;
}

// ─── useAlternateScreen ────────────────────────────────────────────────

/**
 * Alternate-screen management moved to `bin.ts` so the terminal switches
 * into the alt buffer BEFORE Ink's first render. When we owned it here
 * via `useEffect`, the boot-view's first paint landed on the main screen
 * and the subsequent alt-screen switch produced a blank canvas until the
 * next state change — invisible on fast warm-starts where initialize()
 * completed before any further render fired.
 *
 * Kept as a no-op hook so call-sites don't need to change. `bin.ts`
 * registers a `process.on('exit')` handler to restore the main screen.
 */
export function useAlternateScreen(): void {
  useEffect(() => {
    // intentional no-op; see JSDoc above
  }, []);
}

// ─── useAsyncValue ───────────────────────────────────────────────────

/** Resolve one canonical client read without allowing stale effects to win. */
export function useAsyncValue<T>(
  load: () => Promise<T>,
  dependencies: DependencyList,
  initialValue: T,
): { value: T; loading: boolean; error: Error | null } {
  const [value, setValue] = useState<T>(initialValue);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<Error | null>(null);

  useEffect(() => {
    let active = true;
    setLoading(true);
    setError(null);
    void load().then(
      (next) => {
        if (!active) return;
        setValue(next);
        setLoading(false);
      },
      (reason: unknown) => {
        if (!active) return;
        setError(reason instanceof Error ? reason : new Error(String(reason)));
        setLoading(false);
      },
    );
    return () => {
      active = false;
    };
    // The caller supplies the semantic request identity explicitly.
  }, dependencies);

  return { value, loading, error };
}

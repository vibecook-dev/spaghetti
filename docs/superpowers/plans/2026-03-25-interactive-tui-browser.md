> **Historical implementation record.** Do not execute the synchronous API
> snippets below against the current codebase. RFC 011 migrated CLI/TUI reads
> to the asynchronous Rust-backed service; see the
> [Phase 10 closure ledger](../../rfcs/011-phase-10-closure.md).

# Interactive TUI Browser Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add an interactive hierarchical browser to `spag p` that lets users navigate projects → sessions → messages → message detail with arrow keys.

**Architecture:** Zero-dependency thin TUI layer (`tui.ts`) for terminal control, a generic scrollable list (`interactive-list.ts`) for viewport math, and a hierarchical browser (`browse.ts`) that orchestrates 4 view states. When TTY is detected, `projects.ts` delegates to the browser; otherwise falls back to existing static table.

**Tech Stack:** Node.js built-in `process.stdin`/`process.stdout` raw mode, ANSI escape codes, existing `picocolors`/`cli-truncate`/`string-width` utilities. Testing with `node:test` + `node:assert`.

**Spec:** `docs/superpowers/specs/2026-03-25-interactive-tui-browser-design.md`

---

### File Structure

| Action | Path | Responsibility |
|--------|------|----------------|
| Create | `packages/cli/src/lib/tui.ts` | Alternate screen, raw mode, keypress parsing, render, resize, cleanup |
| Create | `packages/cli/src/lib/interactive-list.ts` | Generic scrollable list: viewport math, selection, scroll-follows-cursor |
| Create | `packages/cli/src/commands/browse.ts` | 4-state hierarchical browser, data fetching, level transitions |
| Create | `packages/cli/src/__tests__/tui.test.ts` | Unit tests for keypress parsing and escape sequence generation |
| Create | `packages/cli/src/__tests__/interactive-list.test.ts` | Unit tests for viewport math, scrolling, selection |
| Modify | `packages/cli/src/lib/color.ts:7-21` | Add `session`, `message`, `detail` semantic colors |
| Modify | `packages/cli/src/commands/projects.ts:11-15,37-53` | Add `interactive` option, TTY detection, delegate to browse |
| Modify | `packages/cli/src/index.ts:139-153` | Add `--no-interactive` flag to projects command |

---

### Task 1: Add Semantic Colors to Theme

**Files:**
- Modify: `packages/cli/src/lib/color.ts:7-21`

- [ ] **Step 1: Add the new theme entries**

Add `session`, `message`, and `detail` semantic colors after line 19 (before the closing `}`):

```typescript
// In packages/cli/src/lib/color.ts, add to the theme object:
  session: (s: string) => pc.bold(pc.yellow(s)),
  message: (s: string) => pc.bold(pc.green(s)),
  detail: (s: string) => pc.bold(pc.magenta(s)),
```

These map to the spec's color-per-level scheme: cyan/blue (projects — already `theme.project`), yellow (sessions), green (messages), magenta (message detail).

- [ ] **Step 2: Verify typecheck passes**

Run: `cd packages/cli && npx tsc --noEmit`
Expected: No errors

- [ ] **Step 3: Commit**

```bash
git add packages/cli/src/lib/color.ts
git commit -m "feat(cli): add session/message/detail semantic colors to theme"
```

---

### Task 2: Build the TUI Layer

**Files:**
- Create: `packages/cli/src/lib/tui.ts`
- Create: `packages/cli/src/__tests__/tui.test.ts`

- [ ] **Step 1: Write failing tests for keypress parsing**

Create `packages/cli/src/__tests__/tui.test.ts`:

```typescript
import { test, describe } from 'node:test';
import assert from 'node:assert';
import { parseKeypress } from '../lib/tui.js';

describe('parseKeypress', () => {
  test('parses up arrow', () => {
    assert.strictEqual(parseKeypress(Buffer.from([0x1b, 0x5b, 0x41])), 'up');
  });

  test('parses down arrow', () => {
    assert.strictEqual(parseKeypress(Buffer.from([0x1b, 0x5b, 0x42])), 'down');
  });

  test('parses enter', () => {
    assert.strictEqual(parseKeypress(Buffer.from([0x0d])), 'enter');
  });

  test('parses escape', () => {
    assert.strictEqual(parseKeypress(Buffer.from([0x1b])), 'escape');
  });

  test('parses q', () => {
    assert.strictEqual(parseKeypress(Buffer.from([0x71])), 'q');
  });

  test('parses ctrl+c', () => {
    assert.strictEqual(parseKeypress(Buffer.from([0x03])), 'ctrl-c');
  });

  test('returns null for unknown input', () => {
    assert.strictEqual(parseKeypress(Buffer.from([0x61])), null); // 'a'
  });
});
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cd packages/cli && node --import tsx --test src/__tests__/tui.test.ts`
Expected: FAIL — `parseKeypress` not found

- [ ] **Step 3: Implement `tui.ts`**

Create `packages/cli/src/lib/tui.ts`:

```typescript
/**
 * TUI — Thin terminal control layer for interactive CLI views
 *
 * Provides: alternate screen buffer, raw mode keypress parsing,
 * screen rendering, resize handling, and graceful cleanup.
 * Zero dependencies beyond Node.js built-ins + picocolors.
 */

import cliTruncate from 'cli-truncate';

// ─── Types ──────────────────────────────────────────────────────────────

export type KeyEvent = 'up' | 'down' | 'enter' | 'escape' | 'q' | 'ctrl-c';

export interface TUI {
  /** Clear screen and write lines */
  render(lines: string[]): void;
  /** Register keypress handler */
  onKey(handler: (key: KeyEvent) => void): void;
  /** Current terminal height */
  rows: number;
  /** Current terminal width */
  cols: number;
  /** Register resize handler */
  onResize(handler: () => void): void;
  /** Restore terminal state — MUST be called before exit */
  cleanup(): void;
}

// ─── ANSI Escape Sequences ──────────────────────────────────────────────

const ESC = '\x1b';
const CSI = `${ESC}[`;

const ANSI = {
  enterAltScreen: `${CSI}?1049h`,
  exitAltScreen: `${CSI}?1049l`,
  hideCursor: `${CSI}?25l`,
  showCursor: `${CSI}?25h`,
  clearScreen: `${CSI}2J${CSI}H`,
  moveTo: (row: number, col: number) => `${CSI}${row};${col}H`,
} as const;

// ─── Keypress Parsing (exported for testing) ────────────────────────────

export function parseKeypress(buf: Buffer): KeyEvent | null {
  if (buf.length === 0) return null;

  // Ctrl+C
  if (buf[0] === 0x03) return 'ctrl-c';

  // Enter (CR)
  if (buf[0] === 0x0d) return 'enter';

  // Escape (bare ESC or ESC without valid sequence)
  if (buf[0] === 0x1b) {
    // Arrow keys: ESC [ A/B/C/D
    if (buf.length >= 3 && buf[1] === 0x5b) {
      if (buf[2] === 0x41) return 'up';
      if (buf[2] === 0x42) return 'down';
      // right (0x43) and left (0x44) not mapped — reserved for future
    }
    // Bare escape
    if (buf.length === 1) return 'escape';
    return null;
  }

  // 'q' key
  if (buf[0] === 0x71) return 'q';

  return null;
}

// ─── Factory ────────────────────────────────────────────────────────────

const MIN_ROWS = 10;
const MIN_COLS = 40;

export class TUINotAvailableError extends Error {
  constructor(reason: string) {
    super(`Interactive mode not available: ${reason}`);
    this.name = 'TUINotAvailableError';
  }
}

export function createTUI(): TUI {
  // Precondition checks
  if (!process.stdout.isTTY) {
    throw new TUINotAvailableError('stdout is not a TTY');
  }
  if (!process.stdin.isTTY) {
    throw new TUINotAvailableError('stdin is not a TTY');
  }

  const rows = process.stdout.rows ?? 24;
  const cols = process.stdout.columns ?? 80;

  if (rows < MIN_ROWS || cols < MIN_COLS) {
    throw new TUINotAvailableError(
      `terminal too small (${cols}x${rows}, need ${MIN_COLS}x${MIN_ROWS})`,
    );
  }

  let keyHandler: ((key: KeyEvent) => void) | null = null;
  let resizeHandler: (() => void) | null = null;
  let cleaned = false;

  const tui: TUI = {
    rows,
    cols,

    render(lines: string[]) {
      let output = ANSI.clearScreen;
      for (let i = 0; i < lines.length && i < tui.rows; i++) {
        output += ANSI.moveTo(i + 1, 1) + cliTruncate(lines[i], tui.cols);
      }
      process.stdout.write(output);
    },

    onKey(handler: (key: KeyEvent) => void) {
      keyHandler = handler;
    },

    onResize(handler: () => void) {
      resizeHandler = handler;
    },

    cleanup() {
      if (cleaned) return;
      cleaned = true;

      // Restore stdin
      if (process.stdin.isTTY) {
        process.stdin.setRawMode(false);
      }
      process.stdin.removeListener('data', onData);
      process.stdin.pause();

      // Restore screen
      process.stdout.write(ANSI.showCursor + ANSI.exitAltScreen);

      // Remove resize listener
      process.stdout.removeListener('resize', onResizeEvent);
    },
  };

  // Setup: enter alt screen, hide cursor, enable raw mode
  process.stdout.write(ANSI.enterAltScreen + ANSI.hideCursor);
  process.stdin.setRawMode(true);
  process.stdin.resume();
  process.stdin.setEncoding('utf8');

  // Input handling
  function onData(data: Buffer | string) {
    const buf = typeof data === 'string' ? Buffer.from(data) : data;
    const key = parseKeypress(buf);
    if (key && keyHandler) {
      keyHandler(key);
    }
  }

  process.stdin.on('data', onData);

  // Resize handling
  function onResizeEvent() {
    tui.rows = process.stdout.rows ?? 24;
    tui.cols = process.stdout.columns ?? 80;
    if (resizeHandler) resizeHandler();
  }

  process.stdout.on('resize', onResizeEvent);

  return tui;
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cd packages/cli && node --import tsx --test src/__tests__/tui.test.ts`
Expected: All 7 tests PASS

- [ ] **Step 5: Run typecheck**

Run: `cd packages/cli && npx tsc --noEmit`
Expected: No errors

- [ ] **Step 6: Commit**

```bash
git add packages/cli/src/lib/tui.ts packages/cli/src/__tests__/tui.test.ts
git commit -m "feat(cli): add thin TUI layer with keypress parsing and screen control"
```

---

### Task 3: Build the Interactive List

**Files:**
- Create: `packages/cli/src/lib/interactive-list.ts`
- Create: `packages/cli/src/__tests__/interactive-list.test.ts`

- [ ] **Step 1: Write failing tests**

Create `packages/cli/src/__tests__/interactive-list.test.ts`:

```typescript
import { test, describe } from 'node:test';
import assert from 'node:assert';
import { createListView } from '../lib/interactive-list.js';

// Minimal renderItem: each item is 1 line
const renderItem = (item: string, _idx: number, selected: boolean) => [
  selected ? `> ${item}` : `  ${item}`,
];

describe('createListView', () => {
  test('getLines includes header, items, and footer', () => {
    const view = createListView({
      items: ['a', 'b', 'c'],
      renderItem,
      headerLines: ['Header'],
      footerLines: ['Footer'],
      viewportHeight: 10,
    });
    const lines = view.getLines();
    assert.strictEqual(lines[0], 'Header');
    assert.ok(lines.includes('Footer'));
    assert.ok(lines.some((l) => l.includes('> a'))); // first selected by default
  });

  test('first item is selected by default', () => {
    const view = createListView({
      items: ['a', 'b', 'c'],
      renderItem,
      headerLines: [],
      footerLines: [],
      viewportHeight: 10,
    });
    assert.strictEqual(view.getSelected(), 'a');
    assert.strictEqual(view.getSelectedIndex(), 0);
  });

  test('moveDown advances selection', () => {
    const view = createListView({
      items: ['a', 'b', 'c'],
      renderItem,
      headerLines: [],
      footerLines: [],
      viewportHeight: 10,
    });
    view.moveDown();
    assert.strictEqual(view.getSelected(), 'b');
    assert.strictEqual(view.getSelectedIndex(), 1);
  });

  test('moveUp wraps or clamps at top', () => {
    const view = createListView({
      items: ['a', 'b', 'c'],
      renderItem,
      headerLines: [],
      footerLines: [],
      viewportHeight: 10,
    });
    view.moveUp(); // already at 0
    assert.strictEqual(view.getSelectedIndex(), 0);
  });

  test('moveDown clamps at bottom', () => {
    const view = createListView({
      items: ['a', 'b'],
      renderItem,
      headerLines: [],
      footerLines: [],
      viewportHeight: 10,
    });
    view.moveDown();
    view.moveDown(); // past end
    assert.strictEqual(view.getSelectedIndex(), 1);
  });

  test('reset returns to first item', () => {
    const view = createListView({
      items: ['a', 'b', 'c'],
      renderItem,
      headerLines: [],
      footerLines: [],
      viewportHeight: 10,
    });
    view.moveDown();
    view.moveDown();
    view.reset();
    assert.strictEqual(view.getSelectedIndex(), 0);
  });

  test('updateItems replaces items and clamps index', () => {
    const view = createListView({
      items: ['a', 'b', 'c'],
      renderItem,
      headerLines: [],
      footerLines: [],
      viewportHeight: 10,
    });
    view.moveDown();
    view.moveDown(); // index = 2
    view.updateItems(['x']); // only 1 item now
    assert.strictEqual(view.getSelectedIndex(), 0);
    assert.strictEqual(view.getSelected(), 'x');
  });

  test('updateItems preserves index when valid', () => {
    const view = createListView({
      items: ['a', 'b', 'c'],
      renderItem,
      headerLines: [],
      footerLines: [],
      viewportHeight: 10,
    });
    view.moveDown(); // index = 1
    view.updateItems(['a', 'b', 'c', 'd']); // expanded
    assert.strictEqual(view.getSelectedIndex(), 1);
  });
});

describe('viewport scrolling', () => {
  // 2-line items to test scroll
  const tallRender = (item: string, _idx: number, selected: boolean) => [
    selected ? `> ${item}` : `  ${item}`,
    '  ---',
  ];

  test('scrolls down when selection exceeds viewport', () => {
    // 3 items × 2 lines = 6 lines needed, viewport is 4
    const view = createListView({
      items: ['a', 'b', 'c'],
      renderItem: tallRender,
      headerLines: [],
      footerLines: [],
      viewportHeight: 4, // fits 2 items
    });
    const lines1 = view.getLines();
    assert.ok(lines1.some((l) => l.includes('> a'))); // 'a' visible and selected

    view.moveDown(); // select 'b'
    view.moveDown(); // select 'c' — should scroll
    const lines2 = view.getLines();
    assert.ok(lines2.some((l) => l.includes('> c'))); // 'c' visible
  });

  test('empty items produces no crash', () => {
    const view = createListView({
      items: [] as string[],
      renderItem,
      headerLines: ['Header'],
      footerLines: ['Footer'],
      viewportHeight: 10,
    });
    const lines = view.getLines();
    assert.ok(lines.includes('Header'));
    assert.ok(lines.includes('Footer'));
  });
});
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cd packages/cli && node --import tsx --test src/__tests__/interactive-list.test.ts`
Expected: FAIL — `createListView` not found

- [ ] **Step 3: Implement `interactive-list.ts`**

Create `packages/cli/src/lib/interactive-list.ts`:

```typescript
/**
 * Interactive List — generic scrollable list with selection
 *
 * Manages viewport math and selection state.
 * Returns pre-formatted lines for the caller to pass to tui.render().
 * Knows nothing about spaghetti data models.
 */

export interface ListConfig<T> {
  items: T[];
  renderItem: (item: T, index: number, selected: boolean) => string[];
  headerLines: string[];
  footerLines: string[];
  viewportHeight: number;
}

export interface ListView<T> {
  getLines(): string[];
  moveUp(): void;
  moveDown(): void;
  getSelected(): T | undefined;
  getSelectedIndex(): number;
  updateItems(items: T[]): void;
  reset(): void;
}

export function createListView<T>(config: ListConfig<T>): ListView<T> {
  let items = config.items;
  let selectedIndex = 0;
  let scrollOffset = 0; // index of the first visible item

  function getVisibleItemCount(): number {
    // Calculate how many items fit in viewport by trying to pack them
    if (items.length === 0) return 0;
    let usedLines = 0;
    let count = 0;
    for (let i = scrollOffset; i < items.length; i++) {
      const itemLines = config.renderItem(items[i], i, i === selectedIndex);
      if (usedLines + itemLines.length > config.viewportHeight && count > 0) break;
      usedLines += itemLines.length;
      count++;
    }
    return count;
  }

  function adjustScroll(): void {
    if (items.length === 0) {
      scrollOffset = 0;
      return;
    }

    // Ensure selectedIndex is within bounds
    if (selectedIndex < 0) selectedIndex = 0;
    if (selectedIndex >= items.length) selectedIndex = items.length - 1;

    // Scroll up if selection is above viewport
    if (selectedIndex < scrollOffset) {
      scrollOffset = selectedIndex;
      return;
    }

    // Scroll down if selection is near bottom of viewport (1-item margin)
    // Count lines from scrollOffset to selectedIndex + 1 (margin item)
    const marginIndex = Math.min(selectedIndex + 1, items.length - 1);
    let usedLines = 0;
    for (let i = scrollOffset; i <= marginIndex && i < items.length; i++) {
      const itemLines = config.renderItem(items[i], i, i === selectedIndex);
      usedLines += itemLines.length;
    }
    while (usedLines > config.viewportHeight && scrollOffset < selectedIndex) {
      const removedLines = config.renderItem(
        items[scrollOffset],
        scrollOffset,
        scrollOffset === selectedIndex,
      );
      usedLines -= removedLines.length;
      scrollOffset++;
    }
  }

  return {
    getLines(): string[] {
      const lines: string[] = [];

      // Header
      for (const h of config.headerLines) lines.push(h);

      if (items.length === 0) {
        // Fill viewport with empty space, footer at bottom
        for (let i = 0; i < config.viewportHeight; i++) lines.push('');
      } else {
        // Render visible items
        let usedLines = 0;
        for (let i = scrollOffset; i < items.length; i++) {
          const itemLines = config.renderItem(items[i], i, i === selectedIndex);
          if (usedLines + itemLines.length > config.viewportHeight && usedLines > 0) break;
          for (const l of itemLines) {
            lines.push(l);
            usedLines++;
          }
        }
        // Pad remaining viewport
        while (usedLines < config.viewportHeight) {
          lines.push('');
          usedLines++;
        }
      }

      // Footer
      for (const f of config.footerLines) lines.push(f);

      return lines;
    },

    moveUp(): void {
      if (selectedIndex > 0) {
        selectedIndex--;
        adjustScroll();
      }
    },

    moveDown(): void {
      if (selectedIndex < items.length - 1) {
        selectedIndex++;
        adjustScroll();
      }
    },

    getSelected(): T {
      return items[selectedIndex];
    },

    getSelectedIndex(): number {
      return selectedIndex;
    },

    updateItems(newItems: T[]): void {
      items = newItems;
      // Clamp selection
      if (selectedIndex >= items.length) {
        selectedIndex = Math.max(0, items.length - 1);
      }
      adjustScroll();
    },

    reset(): void {
      selectedIndex = 0;
      scrollOffset = 0;
    },
  };
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cd packages/cli && node --import tsx --test src/__tests__/interactive-list.test.ts`
Expected: All 10 tests PASS

- [ ] **Step 5: Run typecheck**

Run: `cd packages/cli && npx tsc --noEmit`
Expected: No errors

- [ ] **Step 6: Commit**

```bash
git add packages/cli/src/lib/interactive-list.ts packages/cli/src/__tests__/interactive-list.test.ts
git commit -m "feat(cli): add generic interactive list with viewport scrolling"
```

---

### Task 4: Build the Hierarchical Browser

**Files:**
- Create: `packages/cli/src/commands/browse.ts`

This is the largest file. It orchestrates 4 view states, manages level transitions, and renders card-style items using existing format utilities.

- [ ] **Step 1: Create `browse.ts` with types and state setup**

Create `packages/cli/src/commands/browse.ts`:

```typescript
/**
 * Browse command — interactive hierarchical browser
 *
 * Navigates: PROJECTS → SESSIONS → MESSAGES → MESSAGE DETAIL
 * Uses tui.ts for terminal control and interactive-list.ts for list views.
 */

import type {
  SpaghettiAPI,
  ProjectListItem,
  SessionListItem,
  SessionMessage,
  MessagePage,
} from '@vibecook/spaghetti-core';
import { createTUI, TUINotAvailableError } from '../lib/tui.js';
import type { TUI, KeyEvent } from '../lib/tui.js';
import { createListView } from '../lib/interactive-list.js';
import type { ListView } from '../lib/interactive-list.js';
import { theme } from '../lib/color.js';
import {
  formatTokens,
  formatRelativeTime,
  formatNumber,
  formatDuration,
  totalTokens,
} from '../lib/format.js';
import { renderMessage, filterDisplayableMessages } from '../lib/message-render.js';
import cliTruncate from 'cli-truncate';
import pc from 'picocolors';

// ─── Types ──────────────────────────────────────────────────────────────

type ViewLevel = 'projects' | 'sessions' | 'messages' | 'detail';

interface ViewState {
  level: ViewLevel;
  // Context for current level
  project?: ProjectListItem;
  session?: SessionListItem;
  message?: SessionMessage;
  // Selection memory (index to restore when going back)
  projectIndex: number;
  sessionIndex: number;
  messageIndex: number;
}

// ─── Constants ──────────────────────────────────────────────────────────

const PAGE_SIZE = 50;
const LOAD_MORE_THRESHOLD = 5;
const SEPARATOR = (cols: number) => pc.dim('─'.repeat(cols));

// ─── Main Entry Point ───────────────────────────────────────────────────

export async function browseCommand(api: SpaghettiAPI): Promise<void> {
  let tui: TUI;
  try {
    tui = createTUI();
  } catch (err) {
    if (err instanceof TUINotAvailableError) {
      throw err; // caller handles fallback
    }
    throw err;
  }

  const state: ViewState = {
    level: 'projects',
    projectIndex: 0,
    sessionIndex: 0,
    messageIndex: 0,
  };

  // Data caches
  let projects: ProjectListItem[] = [];
  let sessions: SessionListItem[] = [];
  let messages: SessionMessage[] = [];
  let messagePage: MessagePage | null = null;
  let projectFirstPrompts: Map<string, string> = new Map();

  // Active list views
  let projectList: ListView<ProjectListItem> | null = null;
  let sessionList: ListView<SessionListItem> | null = null;
  let messageList: ListView<SessionMessage> | null = null;

  // Detail scroll state
  let detailLines: string[] = [];
  let detailScrollOffset = 0;

  // ─── Data Fetching ──────────────────────────────────────────────────

  function loadProjects(): void {
    projects = api.getProjectList();
    // Sort by last active (most recent first)
    projects.sort(
      (a, b) =>
        new Date(b.lastActiveAt).getTime() - new Date(a.lastActiveAt).getTime(),
    );
    // Fetch first prompt for each project from their latest session
    projectFirstPrompts = new Map();
    for (const p of projects) {
      const sess = api.getSessionList(p.slug);
      if (sess.length > 0) {
        projectFirstPrompts.set(p.slug, sess[0].firstPrompt || '');
      }
    }
  }

  function loadSessions(project: ProjectListItem): void {
    sessions = api.getSessionList(project.slug);
  }

  function loadMessages(project: ProjectListItem, session: SessionListItem): void {
    messagePage = api.getSessionMessages(project.slug, session.sessionId, PAGE_SIZE, 0);
    messages = filterDisplayableMessages(messagePage.messages);
  }

  function loadMoreMessages(): void {
    if (!messagePage || !messagePage.hasMore || !state.project || !state.session) return;
    const nextPage = api.getSessionMessages(
      state.project.slug,
      state.session.sessionId,
      PAGE_SIZE,
      messagePage.offset + messagePage.messages.length,
    );
    messagePage = {
      messages: [...messagePage.messages, ...nextPage.messages],
      total: nextPage.total,
      offset: 0,
      hasMore: nextPage.hasMore,
    };
    messages = filterDisplayableMessages(messagePage.messages);
    if (messageList) {
      messageList.updateItems(messages);
    }
  }

  // ─── Renderers ──────────────────────────────────────────────────────

  function renderProjectItem(
    p: ProjectListItem,
    idx: number,
    selected: boolean,
  ): string[] {
    const cols = tui.cols;
    const prefix = selected ? pc.cyan('▎') : ' ';
    const bg = selected ? pc.bold : (s: string) => pc.dim(s);
    const accent = selected ? pc.cyan : pc.dim;

    const name = bg(p.folderName);
    const branch = accent(p.latestGitBranch || '');
    const prompt = projectFirstPrompts.get(p.slug) || '';
    const promptLine = accent(
      cliTruncate(`"${prompt}"`, Math.max(cols - 6, 20)),
    );
    const stats = accent(
      `${formatNumber(p.sessionCount)} sessions  ·  ${formatNumber(p.messageCount)} msgs  ·  ${formatTokens(totalTokens(p.tokenUsage))} tokens  ·  ${formatRelativeTime(p.lastActiveAt)}`,
    );

    return [
      `${prefix} ${name}  ${branch}`,
      `${prefix} ${promptLine}`,
      `${prefix} ${stats}`,
    ];
  }

  function renderSessionItem(
    s: SessionListItem,
    idx: number,
    selected: boolean,
  ): string[] {
    const cols = tui.cols;
    const prefix = selected ? pc.yellow('▎') : ' ';
    const bg = selected ? pc.bold : (s: string) => pc.dim(s);
    const accent = selected ? pc.yellow : pc.dim;

    const num = bg(`#${idx + 1}`);
    const branch = accent(s.gitBranch || '');
    const prompt = s.firstPrompt || '';
    const promptLine = accent(
      cliTruncate(`"${prompt}"`, Math.max(cols - 6, 20)),
    );
    const stats = accent(
      `${formatNumber(s.messageCount)} msgs  ·  ${formatTokens(totalTokens(s.tokenUsage))} tokens  ·  ${formatDuration(s.lifespanMs)}  ·  ${formatRelativeTime(s.lastUpdate)}`,
    );

    return [
      `${prefix} ${num}  ${branch}`,
      `${prefix} ${promptLine}`,
      `${prefix} ${stats}`,
    ];
  }

  function renderMessageItem(
    msg: SessionMessage,
    idx: number,
    selected: boolean,
  ): string[] {
    const cols = tui.cols;
    const prefix = selected ? pc.green('▎') : ' ';
    const bg = selected ? pc.bold : (s: string) => pc.dim(s);
    const accent = selected ? pc.green : pc.dim;

    let role = msg.type;
    let roleStyled = accent(role);
    if (msg.type === 'user') roleStyled = selected ? pc.green(pc.bold('user')) : pc.dim('user');
    if (msg.type === 'assistant')
      roleStyled = selected ? pc.green(pc.bold('assistant')) : pc.dim('assistant');

    const timestamp =
      'timestamp' in msg && msg.timestamp
        ? accent(formatRelativeTime(msg.timestamp))
        : '';

    // Extract preview text
    let preview = '';
    if (msg.type === 'user') {
      const content = msg.message.content;
      if (typeof content === 'string') preview = content;
      else if (Array.isArray(content)) {
        const textBlock = content.find((b: any) => b.type === 'text');
        if (textBlock && 'text' in textBlock) preview = textBlock.text;
      }
    } else if (msg.type === 'assistant') {
      const blocks = msg.message.content || [];
      const textBlocks = blocks.filter((b: any) => b.type === 'text');
      preview = textBlocks.map((b: any) => b.text).join(' ');
    } else if (msg.type === 'system') {
      preview = '[system]';
    }
    preview = preview.replace(/\n/g, ' ');
    const previewLine = accent(cliTruncate(preview, Math.max(cols - 6, 20)));

    return [
      `${prefix} ${roleStyled}  ${timestamp}`,
      `${prefix} ${previewLine}`,
    ];
  }

  // ─── Header / Footer Builders ───────────────────────────────────────

  function buildHeader(): string[] {
    const cols = tui.cols;
    let breadcrumb = '';
    let hintRight = pc.dim('← Esc  q Quit');

    switch (state.level) {
      case 'projects':
        breadcrumb = theme.project(`Projects`) + pc.dim(` (${projects.length})`);
        break;
      case 'sessions':
        breadcrumb =
          pc.dim(state.project!.folderName) +
          pc.dim(' › ') +
          theme.session(`Sessions`) +
          pc.dim(` (${sessions.length})`);
        break;
      case 'messages':
        breadcrumb =
          pc.dim(state.project!.folderName) +
          pc.dim(' › ') +
          pc.dim(`#${state.sessionIndex + 1}`) +
          pc.dim(' › ') +
          theme.message(`Messages`) +
          pc.dim(` (${messagePage?.total ?? messages.length})`);
        break;
      case 'detail': {
        const role = state.message?.type || '';
        const ts =
          state.message && 'timestamp' in state.message && state.message.timestamp
            ? formatRelativeTime(state.message.timestamp)
            : '';
        breadcrumb =
          pc.dim(state.project!.folderName) +
          pc.dim(' › ') +
          pc.dim(`#${state.sessionIndex + 1}`) +
          pc.dim(' › ') +
          theme.detail(`Message ${state.messageIndex + 1}`) +
          pc.dim(` ${role} · ${ts}`);
        break;
      }
    }

    return [
      `  ${breadcrumb}`,
      `  ${SEPARATOR(cols - 4)}`,
    ];
  }

  function buildFooter(): string[] {
    const cols = tui.cols;
    let hints = '';

    switch (state.level) {
      case 'projects':
        hints = '↑↓ navigate  Enter open  q quit';
        break;
      case 'sessions':
      case 'messages':
        hints = '↑↓ navigate  Enter open  Esc back  q quit';
        break;
      case 'detail':
        hints = `↑↓ scroll  Esc back  q quit  [${detailScrollOffset + 1}/${detailLines.length}]`;
        break;
    }

    return [
      `  ${SEPARATOR(cols - 4)}`,
      `  ${pc.dim(hints)}`,
    ];
  }

  // ─── View Setup ─────────────────────────────────────────────────────

  function setupProjectsView(): void {
    loadProjects();
    const header = buildHeader();
    const footer = buildFooter();
    const viewportHeight = tui.rows - header.length - footer.length;

    projectList = createListView({
      items: projects,
      renderItem: renderProjectItem,
      headerLines: header,
      footerLines: footer,
      viewportHeight,
    });

    // Restore selection if going back
    while (projectList.getSelectedIndex() < state.projectIndex && state.projectIndex < projects.length) {
      projectList.moveDown();
    }
  }

  function setupSessionsView(): void {
    loadSessions(state.project!);
    state.level = 'sessions';
    const header = buildHeader();
    const footer = buildFooter();
    const viewportHeight = tui.rows - header.length - footer.length;

    sessionList = createListView({
      items: sessions,
      renderItem: renderSessionItem,
      headerLines: header,
      footerLines: footer,
      viewportHeight,
    });

    while (sessionList.getSelectedIndex() < state.sessionIndex && state.sessionIndex < sessions.length) {
      sessionList.moveDown();
    }
  }

  function setupMessagesView(): void {
    loadMessages(state.project!, state.session!);
    state.level = 'messages';
    const header = buildHeader();
    const footer = buildFooter();
    const viewportHeight = tui.rows - header.length - footer.length;

    messageList = createListView({
      items: messages,
      renderItem: renderMessageItem,
      headerLines: header,
      footerLines: footer,
      viewportHeight,
    });

    while (messageList.getSelectedIndex() < state.messageIndex && state.messageIndex < messages.length) {
      messageList.moveDown();
    }
  }

  function setupDetailView(): void {
    state.level = 'detail';
    detailScrollOffset = 0;
    const rendered = renderMessage(state.message!, { width: tui.cols - 4 });
    detailLines = rendered.split('\n');
  }

  // ─── Render ──────────────────────────────────────────────────────────

  // Recreates the list view with fresh header/footer on each render.
  // This is simple and correct. At our data scale (< 50 visible items
  // due to pagination), the O(n) index restoration is negligible.
  function fullRender(): void {
    if (state.level === 'detail') {
      const dh = buildHeader();
      const df = buildFooter();
      const viewportHeight = tui.rows - dh.length - df.length;
      const visible = detailLines.slice(
        detailScrollOffset,
        detailScrollOffset + viewportHeight,
      );
      while (visible.length < viewportHeight) visible.push('');
      tui.render([...dh, ...visible.map((l) => `  ${l}`), ...df]);
      return;
    }

    // For list views, recreate with fresh header/footer
    const header = buildHeader();
    const footer = buildFooter();
    const viewportHeight = tui.rows - header.length - footer.length;

    let activeList: ListView<any> | null = null;
    switch (state.level) {
      case 'projects':
        if (projectList) {
          projectList = createListView({
            items: projects,
            renderItem: renderProjectItem,
            headerLines: header,
            footerLines: footer,
            viewportHeight,
          });
          // Restore index
          for (let i = 0; i < state.projectIndex && i < projects.length - 1; i++) {
            projectList.moveDown();
          }
          activeList = projectList;
        }
        break;
      case 'sessions':
        if (sessionList) {
          sessionList = createListView({
            items: sessions,
            renderItem: renderSessionItem,
            headerLines: header,
            footerLines: footer,
            viewportHeight,
          });
          for (let i = 0; i < state.sessionIndex && i < sessions.length - 1; i++) {
            sessionList.moveDown();
          }
          activeList = sessionList;
        }
        break;
      case 'messages':
        if (messageList) {
          messageList = createListView({
            items: messages,
            renderItem: renderMessageItem,
            headerLines: header,
            footerLines: footer,
            viewportHeight,
          });
          for (let i = 0; i < state.messageIndex && i < messages.length - 1; i++) {
            messageList.moveDown();
          }
          activeList = messageList;
        }
        break;
    }

    if (activeList) {
      tui.render(activeList.getLines());
    }
  }

  // ─── Key Handler ────────────────────────────────────────────────────

  function handleKey(key: KeyEvent): void {
    if (key === 'q' || key === 'ctrl-c') {
      tui.cleanup();
      return;
    }

    switch (state.level) {
      case 'projects':
        handleProjectsKey(key);
        break;
      case 'sessions':
        handleSessionsKey(key);
        break;
      case 'messages':
        handleMessagesKey(key);
        break;
      case 'detail':
        handleDetailKey(key);
        break;
    }
  }

  function handleProjectsKey(key: KeyEvent): void {
    if (!projectList || projects.length === 0) {
      if (key === 'escape') tui.cleanup();
      return;
    }

    switch (key) {
      case 'up':
        projectList.moveUp();
        state.projectIndex = projectList.getSelectedIndex();
        fullRender();
        break;
      case 'down':
        projectList.moveDown();
        state.projectIndex = projectList.getSelectedIndex();
        fullRender();
        break;
      case 'enter':
        state.project = projectList.getSelected();
        state.projectIndex = projectList.getSelectedIndex();
        state.sessionIndex = 0;
        setupSessionsView();
        fullRender();
        break;
      case 'escape':
        tui.cleanup();
        break;
    }
  }

  function handleSessionsKey(key: KeyEvent): void {
    if (!sessionList) return;

    switch (key) {
      case 'up':
        sessionList.moveUp();
        state.sessionIndex = sessionList.getSelectedIndex();
        fullRender();
        break;
      case 'down':
        sessionList.moveDown();
        state.sessionIndex = sessionList.getSelectedIndex();
        fullRender();
        break;
      case 'enter':
        if (sessions.length === 0) break;
        state.session = sessionList.getSelected();
        state.sessionIndex = sessionList.getSelectedIndex();
        state.messageIndex = 0;
        setupMessagesView();
        fullRender();
        break;
      case 'escape':
        state.level = 'projects';
        setupProjectsView();
        fullRender();
        break;
    }
  }

  function handleMessagesKey(key: KeyEvent): void {
    if (!messageList) return;

    switch (key) {
      case 'up':
        messageList.moveUp();
        state.messageIndex = messageList.getSelectedIndex();
        fullRender();
        break;
      case 'down':
        messageList.moveDown();
        state.messageIndex = messageList.getSelectedIndex();
        // Check if we need to load more
        if (
          messagePage?.hasMore &&
          state.messageIndex >= messages.length - LOAD_MORE_THRESHOLD
        ) {
          loadMoreMessages();
        }
        fullRender();
        break;
      case 'enter':
        if (messages.length === 0) break;
        state.message = messageList.getSelected();
        state.messageIndex = messageList.getSelectedIndex();
        setupDetailView();
        fullRender();
        break;
      case 'escape':
        state.level = 'sessions';
        setupSessionsView();
        fullRender();
        break;
    }
  }

  function handleDetailKey(key: KeyEvent): void {
    const viewportHeight = tui.rows - 4; // header + footer

    switch (key) {
      case 'up':
        if (detailScrollOffset > 0) {
          detailScrollOffset--;
          fullRender();
        }
        break;
      case 'down':
        if (detailScrollOffset < detailLines.length - viewportHeight) {
          detailScrollOffset++;
          fullRender();
        }
        break;
      case 'escape':
        state.level = 'messages';
        setupMessagesView();
        fullRender();
        break;
    }
  }

  // ─── Run ────────────────────────────────────────────────────────────

  try {
    state.level = 'projects';
    setupProjectsView();
    fullRender();

    tui.onKey(handleKey);
    tui.onResize(() => fullRender());

    // Keep the process alive until cleanup is called
    await new Promise<void>((resolve) => {
      const origCleanup = tui.cleanup.bind(tui);
      tui.cleanup = () => {
        origCleanup();
        resolve();
      };
    });
  } catch (err) {
    tui.cleanup();
    throw err;
  }
}
```

- [ ] **Step 2: Run typecheck**

Run: `cd packages/cli && npx tsc --noEmit`
Expected: No errors. If there are type issues with `SessionMessage` discriminated unions, fix the type assertions.

- [ ] **Step 3: Commit**

```bash
git add packages/cli/src/commands/browse.ts
git commit -m "feat(cli): add hierarchical browser with 4-level navigation"
```

---

### Task 5: Wire Up the Entry Points

**Files:**
- Modify: `packages/cli/src/commands/projects.ts:11-15,37-53`
- Modify: `packages/cli/src/index.ts:139-153`
- Modify: `packages/cli/src/bin.ts:12-16`

- [ ] **Step 1: Add `--no-interactive` flag to Commander registration**

In `packages/cli/src/index.ts`, add the option to the projects command. Replace lines 139-153:

```typescript
  // Projects command
  const projectsCmd = new Command('projects')
    .alias('p')
    .description('List all projects')
    .option('-s, --sort <key>', 'Sort by: active, sessions, messages, tokens, name', 'active')
    .option('-l, --limit <n>', 'Limit results', parseInt)
    .option('--no-interactive', 'Disable interactive mode (use static table)')
    .option('--json', 'Output as JSON')
    .action(async (cmdOpts: ProjectsOptions) => {
      await withService((api) =>
        projectsCommand(api, {
          sort: cmdOpts.sort,
          limit: cmdOpts.limit,
          json: cmdOpts.json,
          interactive: cmdOpts.interactive,
        }),
      );
    });
```

Note: Commander's `--no-interactive` pattern creates a boolean `interactive` property. It defaults to `true` and becomes `false` when `--no-interactive` is passed. So `cmdOpts.interactive === false` means the user wants static output.

- [ ] **Step 2: Update `ProjectsOptions` and `projectsCommand` in `projects.ts`**

Update `packages/cli/src/commands/projects.ts`:

Add to the import at the top:
```typescript
import { browseCommand } from './browse.js';
import { TUINotAvailableError } from '../lib/tui.js';
```

Update the interface:
```typescript
export interface ProjectsOptions {
  sort?: string;
  limit?: number;
  json?: boolean;
  interactive?: boolean;  // Commander --no-interactive sets this to false
}
```

At the top of `projectsCommand`, before the existing logic, add TTY detection and delegation:

```typescript
export async function projectsCommand(api: SpaghettiAPI, opts: ProjectsOptions): Promise<void> {
  // Interactive mode: delegate to browse command when TTY and not explicitly disabled
  const interactive = opts.interactive !== false && !opts.json && !opts.limit;
  if (interactive) {
    try {
      await browseCommand(api);
      return;
    } catch (err) {
      if (err instanceof TUINotAvailableError) {
        // Fall through to static output
      } else {
        throw err;
      }
    }
  }

  // --- Existing static table code below (unchanged) ---
  let projects = api.getProjectList();
  // ... rest of existing function
```

- [ ] **Step 3: Verify `bin.ts` needs no changes**

The `bin.ts` SIGINT handler calls `shutdownService()` then `process.exit(0)`. This is safe because when the TUI is active, stdin is in raw mode which intercepts Ctrl+C (0x03) as data before it becomes a SIGINT signal. The TUI's key handler processes `ctrl-c` and calls `tui.cleanup()` before resolving the browse promise, which then exits normally. The `bin.ts` SIGINT handler only fires when the TUI is NOT active (e.g., during init). No changes needed.

- [ ] **Step 4: Run typecheck**

Run: `cd packages/cli && npx tsc --noEmit`
Expected: No errors

- [ ] **Step 5: Run all existing tests to ensure no regressions**

Run: `cd packages/cli && node --import tsx --test src/__tests__/*.test.ts`
Expected: All existing tests pass (format, resolve, error tests + new tui and interactive-list tests)

- [ ] **Step 6: Commit**

```bash
git add packages/cli/src/commands/projects.ts packages/cli/src/index.ts
git commit -m "feat(cli): wire interactive browser into spag p with TTY detection"
```

---

### Task 6: Manual Integration Test

**Files:** None (testing only)

- [ ] **Step 1: Build the CLI**

Run: `cd packages/cli && pnpm run build`
Expected: Build succeeds

- [ ] **Step 2: Test interactive mode**

Run: `node packages/cli/dist/bin.js p`
Expected: Interactive browser launches in alternate screen. Arrow keys navigate projects. Enter drills into sessions. Esc goes back. `q` exits cleanly.

- [ ] **Step 3: Test `--no-interactive` fallback**

Run: `node packages/cli/dist/bin.js p --no-interactive`
Expected: Static table output (same as before)

- [ ] **Step 4: Test piped output fallback**

Run: `node packages/cli/dist/bin.js p | cat`
Expected: Static table output (stdout is not a TTY)

- [ ] **Step 5: Test `--json` flag still works**

Run: `node packages/cli/dist/bin.js p --json`
Expected: JSON output (bypasses interactive mode)

- [ ] **Step 6: Test empty state handling**

If possible, test with a project that has no sessions or test the code path. Verify "No sessions found" message renders with Esc to go back.

- [ ] **Step 7: Final commit if any fixes were needed**

```bash
git add -A
git commit -m "fix(cli): address integration test findings"
```

---

### Summary

| Task | Files | ~Lines | Dependencies |
|------|-------|--------|-------------|
| 1. Semantic colors | color.ts | +3 | None |
| 2. TUI layer | tui.ts, tui.test.ts | ~160 + 40 | None |
| 3. Interactive list | interactive-list.ts, interactive-list.test.ts | ~130 + 100 | None |
| 4. Browse command | browse.ts | ~400 | Tasks 1-3 |
| 5. Wire entry points | projects.ts, index.ts | ~20 modified | Task 4 |
| 6. Integration test | — | 0 | Task 5 |

### Known Gaps

- **`browse.ts` has no unit tests.** The state machine, level transitions, and position memory are tested only via manual integration testing in Task 6. Unit testing `browse.ts` would require mocking both the TUI and SpaghettiAPI, which is significant plumbing. The core logic is covered indirectly: keypress parsing is tested in `tui.test.ts`, viewport math in `interactive-list.test.ts`. Consider adding `browse.test.ts` with a mock API as a follow-up if the feature proves stable.

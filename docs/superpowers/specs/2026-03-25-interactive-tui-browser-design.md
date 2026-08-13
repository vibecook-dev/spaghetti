> **Historical UI specification.** Its synchronous `SpaghettiAPI` references
> predate RFC 011; the shipped browser uses asynchronous Rust-backed reads.
> See the [Phase 10 closure ledger](../../rfcs/011-phase-10-closure.md).

# Interactive TUI Browser for `spag p`

**Date:** 2026-03-25
**Status:** Approved

## Summary

Add an interactive hierarchical browser to the `spag p` command. When run in a TTY, the command launches a full-screen TUI where users navigate projects → sessions → messages → message detail using arrow keys. Non-TTY usage (piped output, `--no-interactive`) falls back to the existing static table.

## Decisions

| Decision | Choice | Rationale |
|----------|--------|-----------|
| Interactive by default? | Yes, in TTY | Natural UX; static fallback for pipes/scripts via `--no-interactive` |
| Keybindings | Minimal: ↑↓, Enter, Esc, q | Simple to learn, extensible later |
| Message display | Compact list + expand on Enter | Keeps TUI responsive for large sessions |
| List layout | Card-style rows (2-3 lines per item) | Breathing room, shows branch + first prompt + stats |
| Header/chrome | Persistent breadcrumb + hint bar | Always shows position and available actions |
| Session preview | First prompt (truncated) | Most recognizable anchor for identifying sessions |
| Implementation approach | Custom thin TUI layer (~200 lines) | Zero new dependencies, tailored to the exact use case |

## Architecture

### New Files

- **`packages/cli/src/lib/tui.ts`** (~200 lines) — Thin terminal control layer: alternate screen buffer, raw mode, keypress parsing, render loop, resize handling.
- **`packages/cli/src/lib/interactive-list.ts`** (~150 lines) — Generic scrollable list component: takes items + renderer, manages selection/scrolling/viewport.
- **`packages/cli/src/commands/browse.ts`** (~250 lines) — Hierarchical browser: manages 4 view states, orchestrates transitions, fetches data from SpaghettiAPI.

### Modified Files

- **`packages/cli/src/commands/projects.ts`** — Detect TTY + `--no-interactive` flag. Delegate to `browse.ts` when interactive, keep existing static table as fallback.
- **`packages/cli/src/index.ts`** — Pass through `--no-interactive` flag on the `projects` command.

### Dependency Graph

```
projects.ts (entry point)
  ├── [non-TTY or --no-interactive] → existing static table (unchanged)
  └── [TTY] → browse.ts
                ├── interactive-list.ts (generic list logic)
                │     └── tui.ts (raw terminal control)
                └── lib/format.ts, lib/color.ts (existing, reused)
```

No new npm dependencies. The implementation uses Node.js built-in `process.stdin`, `process.stdout`, and ANSI escape codes, alongside existing project utilities (`picocolors`, `cli-truncate`, `string-width`).

## Entry Point: TTY Detection & Flag

The `--no-interactive` flag is added as a Commander `.option()` on the `projects` command. `ProjectsOptions` gains a `noInteractive?: boolean` field.

Interactive mode requires **both** `process.stdout.isTTY` and `process.stdin.isTTY` to be true. The existing `isTTY()` helper checks `stderr` — the TUI must check `stdout` and `stdin` independently. If either is not a TTY, or `--no-interactive` is passed, fall back to the existing static table.

## Module Design

### `tui.ts` — Terminal Control Layer

Provides raw terminal management. Knows nothing about spaghetti data.

**Precondition check:** `createTUI()` asserts `process.stdout.isTTY && process.stdin.isTTY` and throws if not satisfied. Also checks minimum terminal dimensions: `rows >= 10 && cols >= 40`. If either check fails, the caller falls back to static output.

```typescript
interface TUI {
  render(lines: string[]): void          // clear screen + write lines
  onKey(handler: (key: KeyEvent) => void): void  // keypress listener
  rows: number                           // current terminal height
  cols: number                           // current terminal width
  cleanup(): void                        // restore terminal state, must always be called
}

type KeyEvent = 'up' | 'down' | 'enter' | 'escape' | 'q'

function createTUI(): TUI
```

**Capabilities:**
1. **Screen management:** `enterAltScreen()` / `exitAltScreen()` preserves original terminal content. `hideCursor()` / `showCursor()` for clean display.
2. **Keypress handling:** Stdin in raw mode, parses multi-byte escape sequences (arrow keys) into named events. Returns cleanup function that restores terminal state.
3. **Render:** Writes pre-formatted lines to stdout. No diffing — clear + write. Fast enough for our data sizes. All lines are truncated to `tui.cols` before writing to prevent line wrapping in raw mode (which would corrupt the display).
4. **Resize:** Listens to `process.stdout.on('resize')`, triggers re-render callback. Exposes current `rows` and `cols`.
5. **Signal handling:** `SIGINT` and `SIGTERM` call `cleanup()` before exit.

### `interactive-list.ts` — Generic Scrollable List

Manages viewport math and selection state. Returns lines for the caller to pass to `tui.render()`. Knows nothing about projects/sessions.

```typescript
interface ListConfig<T> {
  items: T[]
  renderItem: (item: T, index: number, selected: boolean) => string[]  // 1+ lines per item
  headerLines: string[]        // breadcrumb + separator
  footerLines: string[]        // hint bar
  viewportHeight: number       // tui.rows - header - footer
}

function createListView<T>(config: ListConfig<T>): {
  getLines(): string[]         // full screen of lines to render
  moveUp(): void
  moveDown(): void
  getSelected(): T
  getSelectedIndex(): number
  updateItems(items: T[]): void  // replace items, preserve scroll position (clamp if needed)
  reset(): void
}
```

**Scrolling behavior:**
- Shows as many items as fit in `viewportHeight` (items are multi-line)
- Selection starts at index 0
- Scroll-follows-cursor: scrolls when selection moves past viewport edge
- 1-item margin before edge triggers scroll (user can see what's next)

**`updateItems()`** is used when paginating messages — new items are appended and the selection/scroll position is preserved (clamped if the list shrunk). The list view is recreated for level transitions, but within a level, `updateItems()` handles dynamic data.

### `browse.ts` — Hierarchical Browser

The only file that knows about the 3-level hierarchy and SpaghettiAPI.

**Data fetching for project cards:** `ProjectListItem` has no `firstPrompt` field. When entering the PROJECTS view, `browse.ts` calls `getSessionList(slug)` for each project and extracts `sessions[0].firstPrompt` from the most recent session. Since sessions are pre-loaded in memory after `initialize()`, this is synchronous and instant.

**MESSAGE DETAIL is not a list.** The PROJECTS, SESSIONS, and MESSAGES views use `interactive-list.ts`. The MESSAGE DETAIL view is handled directly by `browse.ts` — it renders the output of `renderMessage()` split on `\n` into individual lines, and manages its own scroll offset. Up/down scroll lines, not items. This keeps `interactive-list.ts` focused on its one job (selectable item lists).

## Navigation State Machine

```
PROJECTS ──Enter──→ SESSIONS ──Enter──→ MESSAGES ──Enter──→ MESSAGE DETAIL
    ↑                   │                   │                     │
    └───────Esc─────────┘                   │                     │
                        ↑                   │                     │
                        └───────Esc─────────┘                     │
                                            ↑                     │
                                            └─────────Esc─────────┘
```

### 4 View States

| State | Header | Items | ↑↓ | Enter | Esc |
|-------|--------|-------|-----|-------|-----|
| **PROJECTS** | `Projects (N)` | Card rows: name+branch, first prompt, stats | Navigate items | Push SESSIONS | Exit browser |
| **SESSIONS** | `project › Sessions (N)` | Card rows: #N+branch, first prompt, stats | Navigate items | Push MESSAGES | Pop to PROJECTS |
| **MESSAGES** | `project › #N › Messages (N)` | 2-line rows: role+time, preview | Navigate items | Push MESSAGE detail | Pop to SESSIONS |
| **MESSAGE DETAIL** | `project › #N › Message N role · time` | Full `renderMessage()` output | Scroll content | No-op (reserved for future actions) | Pop to MESSAGES |

**Position memory:** Going back restores the previous selection index. Drilling into project #3, viewing sessions, pressing Esc returns to project #3.

**`q` exits from any level.** Esc at PROJECTS level also exits (no parent to return to).

**Enter in MESSAGE DETAIL** is a no-op. Future extensions could use it for actions like copying content or opening in `$PAGER`.

## Rendering

### Visual Style

- **Selected item:** Left border accent + tinted background, white text
- **Unselected item:** Dimmed text (gray)
- **Color per level:** Cyan/blue (projects, via `theme.accent`), yellow (sessions, via `pc.yellow`), green (messages, via `pc.green`), magenta (message detail, via `pc.magenta`). Add `session`, `message`, and `detail` semantic colors to `theme` in `color.ts`.
- **Breadcrumb:** Parent levels dimmed, current level bold + colored
- **Hint bar:** Updates per level. Shows only relevant actions.
- **Separator:** Single horizontal line (─) between header and content, content and footer

### Lines Per Item

| Level | Lines | Content |
|-------|-------|---------|
| Projects | 3 | Name + branch, first prompt (truncated, dimmed), stats row |
| Sessions | 3 | Session # + branch, first prompt (truncated, dimmed), stats row |
| Messages | 2 | Role + timestamp, content preview (truncated) |
| Message detail | Variable | Full `renderMessage()` output, scrollable |

### Item Rendering Examples

**Project (selected):**
```
▎ jabali-editor  feat/physics
▎ "add rigid body collision detection to the scene editor"
▎ 8 sessions  ·  523 msgs  ·  890K tokens  ·  5 hours ago
```

**Project (unselected):**
```
  spaghetti  main
  "please look into the cli package, we want to make the tui..."
  12 sessions  ·  847 msgs  ·  1.2M tokens  ·  2 hours ago
```

**Message (selected):**
```
▎ user  5:32 PM
▎ add rigid body collision detection to the scene editor
```

**Message detail:** Reuses existing `renderMessage()` from `lib/message-render.ts`. The returned string is split on `\n` into individual lines for viewport scrolling. Each line is truncated to terminal width. Shows tool calls, thinking blocks, full content. Scroll position indicator at bottom: `[12 / 45 lines]`.

## Error Handling & Edge Cases

### Empty States
- Project with 0 sessions → "No sessions found" centered, Esc to go back
- Session with 0 displayable messages → "No messages", Esc to go back
- No projects at all → Exit interactive mode, show static "No projects found"

### Terminal Edge Cases
- **Resize (`SIGWINCH`):** Re-render, recalculate viewport height, clamp scroll offset
- **Very small terminal (< 10 rows or < 40 cols):** Fall back to static output with hint message
- **Ctrl+C / SIGINT:** `tui.cleanup()` called before exit (alt screen restored, cursor shown)
- **Process crash:** Entire browse loop wrapped in try/finally for `cleanup()`. Worst case (`kill -9`): user runs `reset` to fix terminal.

### Data Edge Cases
- **Internal message types:** `filterDisplayableMessages()` already filters these — reuse it
- **Long project/session names:** Truncate with `cli-truncate` to fit terminal width
- **Large message count:** `getSessionMessages()` supports `limit` and `offset` and returns a `MessagePage` with `hasMore`. Fetch 50 messages on entering MESSAGES view, then fetch the next page of 50 when the selected index is within 5 items of the last loaded message and `hasMore` is true. Append new items via `listView.updateItems()`. Note: `getSessionMessages()` is synchronous (data is pre-loaded in SQLite), so there is no async loading delay — pagination is for memory efficiency, not I/O latency.

### Performance
- Projects and sessions are pre-loaded in memory after `initialize()` — instant access
- All API calls (`getProjectList`, `getSessionList`, `getSessionMessages`) are synchronous after init
- Messages are paginated for memory efficiency (50 at a time), not I/O latency
- Re-render is full clear + write (no diffing needed at this scale)

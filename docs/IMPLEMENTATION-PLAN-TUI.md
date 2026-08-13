> **Historical implementation plan.** UI decisions may remain useful, but the
> synchronous `SpaghettiAPI` data examples below predate RFC 011 and must not be
> copied into current code. Production reads use the asynchronous Rust-backed
> service recorded in the [Phase 10 closure ledger](./rfcs/011-phase-10-closure.md).

# Implementation Plan: TUI Redesign (Ink)

**Companion to**: RFC 001, TUI-DESIGN.md
**Created**: 2026-03-29
**Updated**: 2026-03-29 — migrated from raw ANSI to Ink (React for CLIs)

Decomposed into 7 phases. Each phase produces a working, testable state. Phases are sequential — each builds on the previous. Within a phase, steps can often be parallelized.

**Tech stack**: Ink v6 + React 19 + @inkjs/ui v2

---

## Phase 0: Ink Setup

**Goal**: Add Ink to the project, configure TSX, verify "hello world" renders in the terminal.

### Step 0.1: Install dependencies

```bash
cd packages/cli
pnpm add ink react @inkjs/ui
pnpm add -D @types/react
```

### Step 0.2: Configure TSX in tsconfig

**Modified file**: `packages/cli/tsconfig.json`

Add:
```json
{
  "compilerOptions": {
    "jsx": "react-jsx"
  }
}
```

### Step 0.3: Configure tsup for TSX

**Modified file**: `packages/cli/package.json` or `tsup.config.ts`

Ensure tsup handles `.tsx` files. May need to add `esbuildOptions` for JSX or add `tsup.config.ts`:

```typescript
import { defineConfig } from 'tsup';
export default defineConfig({
  entry: ['src/bin.ts'],
  format: ['esm'],
  target: 'node18',
  // tsup auto-detects TSX via tsconfig jsx setting
});
```

### Step 0.4: Smoke test

Create a minimal `src/views/test.tsx`:

```tsx
import React from 'react';
import { render, Text, Box } from 'ink';

function App() {
  return (
    <Box>
      <Text color="green">Ink works!</Text>
    </Box>
  );
}

render(<App />);
```

Run with `tsx src/views/test.tsx` — confirm it renders in the terminal.

### Step 0.5: Verify build

- `pnpm build` succeeds
- `pnpm typecheck` passes
- Existing CLI commands still work (`spag p`, `spag s .`, etc.)

---

## Phase 1: Shell + Core Navigation Views

**Goal**: Rebuild the existing projects → sessions → messages → detail navigation in Ink. Delete `browse.ts`, `lib/tui.ts`, `lib/interactive-list.ts`.

### Step 1.1: Create types and context

**New file**: `packages/cli/src/views/types.ts` (~50 lines)

```typescript
type ViewType = 'boot' | 'projects' | 'sessions' | 'messages' | 'detail'
  | 'search' | 'stats' | 'memory' | 'todos' | 'plan' | 'subagents' | 'help';

interface ViewEntry {
  type: ViewType;
  component: React.FC;
  breadcrumb: string;
}

interface ViewNav {
  push(view: ViewEntry): void;
  pop(): void;
  replace(view: ViewEntry): void;
  quit(): void;
  enterCommandMode(): void;
  context: ViewContext;
}

interface ViewContext {
  project?: ProjectListItem;
  session?: SessionListItem;
}
```

**New file**: `packages/cli/src/views/context.tsx` (~20 lines)

```tsx
const ViewNavContext = React.createContext<ViewNav>(null!);
export const ViewNavProvider = ViewNavContext.Provider;
export const useViewNav = () => React.useContext(ViewNavContext);
```

### Step 1.2: Create shared hooks

**New file**: `packages/cli/src/views/hooks.ts` (~60 lines)

```typescript
// Custom hooks for data fetching from SpaghettiAPI
export function useProjects(api: SpaghettiAPI): ProjectListItem[];
export function useSessions(api: SpaghettiAPI, slug: string): SessionListItem[];
export function useMessages(api: SpaghettiAPI, slug: string, sessionId: string): { ... };
// Hook for scrollable list selection
export function useListNavigation(itemCount: number, opts?: { onSelect?: (i: number) => void }): {
  selectedIndex: number;
  scrollOffset: number;
  visibleRange: [number, number];
};
```

### Step 1.3: Create chrome components

**New file**: `packages/cli/src/views/chrome.tsx` (~80 lines)

Shared layout components:

```tsx
// Horizontal rule
export function HRule() {
  return <Text dimColor>{'─'.repeat(process.stdout.columns || 80)}</Text>;
}

// Breadcrumb header
export function Header({ breadcrumb }: { breadcrumb: string }) {
  return (
    <Box flexDirection="column">
      <Text>  {breadcrumb}</Text>
      <HRule />
    </Box>
  );
}

// Footer with keybinding hints
export function Footer({ hints }: { hints: string }) {
  return (
    <Box flexDirection="column">
      <HRule />
      <Text dimColor>  {hints}</Text>
      <HRule />
    </Box>
  );
}
```

### Step 1.4: Create Shell component

**New file**: `packages/cli/src/views/shell.tsx` (~120 lines)

The root Ink component:
- `useState` for view stack (`ViewEntry[]`)
- `ViewNavProvider` wraps the active view
- Renders: `<Header>` + active view component + `<Footer>`
- Derives `ViewContext` from the stack
- Command mode state (wired in Phase 5)

```tsx
function Shell({ api }: { api: SpaghettiAPI }) {
  const [stack, setStack] = useState<ViewEntry[]>([...]);
  // ... nav callbacks
  const TopView = stack[stack.length - 1].component;
  return (
    <ViewNavProvider value={nav}>
      <Box flexDirection="column">
        <Header breadcrumb={...} />
        <Box flexGrow={1}><TopView /></Box>
        <Footer hints={...} />
      </Box>
    </ViewNavProvider>
  );
}
```

### Step 1.5: Create ProjectsView

**New file**: `packages/cli/src/views/projects-view.tsx` (~100 lines)

- Fetches project list from API
- Renders scrollable list of `<ProjectCard>` components
- `useInput` for `↑`/`↓`/`Enter`/`Esc`/`q`
- Enter pushes `SessionsView`, q quits

`<ProjectCard>` component (~40 lines):
- Line 1: `▎ {name}` + `{branch}` right-aligned
- Line 2: `  "{first prompt}"` italic dim
- Line 3: `  {stats}` dim
- Uses `<Box>` for layout, `<Text>` for styled text

### Step 1.6: Create SessionsView

**New file**: `packages/cli/src/views/sessions-view.tsx` (~90 lines)

Same pattern as ProjectsView but for sessions. Props: `{ project: ProjectListItem }`.

`<SessionCard>` component:
- Line 1: `▎ #{index}  {branch}` + `{short ID}` right-aligned
- Line 2: `  "{first prompt}"`
- Line 3: `  {stats}`

### Step 1.7: Extract display item logic

**New file**: `packages/cli/src/views/display-items.ts` (~200 lines)

Extract from `browse.ts` (pure logic, no rendering):
- `DisplayItem` type and subtypes
- `buildDisplayItems()` — tool pair merging, task notifications
- `TOOL_CATEGORIES`, `getToolCategory()`, tool category colors
- `toolInputSummary()`, `toolResultSummary()`
- `FILTER_CATEGORIES`, `FilterState`, `applyDisplayFilters()`

This file is plain `.ts` (no JSX) — shared between the TUI and potentially one-off commands.

### Step 1.8: Create MessagesView

**New file**: `packages/cli/src/views/messages-view.tsx` (~200 lines)

The largest view. Props: `{ project, session }`.

Components:
- `<FilterChips>` — the `1:user 2:claude ...` toggle bar
- `<UserMessage>` — right-aligned user block with 256-color bg
- `<ClaudeMessage>` — left-aligned claude block with 256-color bg
- `<ToolCallItem>` — single-line tool call with category color
- `<ThinkingItem>` — single-line thinking preview
- `<SystemItem>` — single-line metadata

State:
- Filter toggles (1-6)
- Selected index
- Scroll offset
- Loaded message range (backward pagination)

Uses `useInput` for navigation + filter toggles.

### Step 1.9: Create DetailView

**New file**: `packages/cli/src/views/detail-view.tsx` (~100 lines)

Scrollable text content. Three variants via props:
- Message detail: full rendered content
- Tool call detail: structured input/output
- Thinking detail: italic thinking text

Uses `useInput` for `↑`/`↓` scroll and `Esc` to pop.

### Step 1.10: Wire up entry point

**Modified file**: `packages/cli/src/bin.ts`

```typescript
import { render } from 'ink';
import React from 'react';
import { Shell } from './views/shell.js';

// Bare command on TTY → launch Ink TUI
const { waitUntilExit } = render(
  React.createElement(Shell, { api }),
  { exitOnCtrlC: true }
);
await waitUntilExit();
```

### Step 1.11: Delete replaced files

- Delete `packages/cli/src/commands/browse.ts` (1318 lines)
- Delete `packages/cli/src/lib/tui.ts` (187 lines)
- Delete `packages/cli/src/lib/interactive-list.ts` (141 lines)

### Step 1.12: Verify

- `spag p` on TTY → Ink TUI with project list
- Navigate projects → sessions → messages → detail
- Back navigation with Esc
- Filter toggles in messages view
- Message pagination
- 256-color message blocks render correctly
- Terminal resize
- `q` and Ctrl+C exit cleanly
- Existing one-off commands still work

**This is the biggest phase.** The core navigation must work identically to the current browse.ts before proceeding.

---

## Phase 2: Entry Point Split

**Goal**: `spag` (bare) → TUI. `spag p` → static table (never interactive). Remove `--no-interactive`.

### Step 2.1: Modify bin.ts

**Modified file**: `packages/cli/src/bin.ts`

```typescript
const args = process.argv.slice(2);
const hasSubcommand = args.length > 0 && !args[0].startsWith('-');
const hasJsonFlag = args.includes('--json');

if (!hasSubcommand && !hasJsonFlag) {
  if (process.stdin.isTTY && process.stdout.isTTY) {
    const service = createSpaghettiService();
    const { waitUntilExit } = render(
      React.createElement(Shell, { service })
    );
    await waitUntilExit();
    return;
  } else {
    // Piped: output summary JSON
    const api = await initService({ silent: true });
    await summaryJSON(api);
    shutdownService();
    return;
  }
}

// Has subcommand: fall through to commander
const program = createProgram();
await program.parseAsync(process.argv);
```

### Step 2.2: Add summaryJSON to dashboard.ts

**Modified file**: `packages/cli/src/commands/dashboard.ts`

Export `summaryJSON(api)` that outputs a JSON summary to stdout.

### Step 2.3: Make projects command always static

**Modified file**: `packages/cli/src/commands/projects.ts`

Remove `browseCommand` import and TUI delegation. Always render static table.

### Step 2.4: Remove interactive flags

**Modified file**: `packages/cli/src/index.ts`

Remove `--no-interactive` / `--interactive` from projects command.

### Step 2.5: Verify

- `spag` on TTY → Ink TUI
- `spag | cat` → JSON summary
- `spag p` → static table (never TUI)
- All subcommands work as before

---

## Phase 3: Boot Screen

**Goal**: Show branded loading screen while core initializes.

### Step 3.1: Create BootView

**New file**: `packages/cli/src/views/boot-view.tsx` (~80 lines)

```tsx
function BootView({ progress, error }: BootViewProps) {
  return (
    <Box borderStyle="round" flexDirection="column" paddingX={1}>
      <Wordmark />
      <Text dimColor>untangle your claude code history</Text>
      <Box marginTop={1}>
        {error
          ? <Text color="red">✗ {error}</Text>
          : <ProgressBar value={progress.current / progress.total} />
        }
      </Box>
      <Text dimColor>{progress.message}  {progress.elapsed}s</Text>
    </Box>
  );
}
```

Uses `@inkjs/ui` `<ProgressBar>` component — no hand-coding needed.

### Step 3.2: Create Wordmark component

**New file**: `packages/cli/src/views/wordmark.tsx` (~20 lines)

Hardcoded 3-line half-block art as `<Text>` lines with bold styling.

### Step 3.3: Integrate into Shell

**Modified file**: `packages/cli/src/views/shell.tsx`

Shell manages initialization lifecycle:
1. Start with `<BootView>` visible
2. Call `service.initialize()` with progress callback
3. Progress updates → re-render `<BootView>` with new values
4. On complete → switch to `<ProjectsView>` (boot screen disappears)
5. Skip boot if init <200ms (warm start)

### Step 3.4: Verify

- Cold start shows boot screen with progress bar
- Progress updates per project
- Transitions to project list on complete
- Warm start skips boot screen
- Error shows in the boot panel, `q` exits

---

## Phase 4: Welcome Panel

**Goal**: Two-column branded header on the home screen.

### Step 4.1: Create WelcomePanel

**New file**: `packages/cli/src/views/welcome-panel.tsx` (~70 lines)

```tsx
function WelcomePanel({ version, stats, perf }: WelcomePanelProps) {
  return (
    <Box borderStyle="round" paddingX={1}>
      {/* Left column */}
      <Box flexDirection="column" flexGrow={1}>
        <Wordmark />
        <Text dimColor>untangle your claude code history</Text>
        <Text dimColor>{perf.dataPath} · {perf.dataSize} · {perf.warmStartMs}ms</Text>
      </Box>
      {/* Divider */}
      <Box width={1}><Text dimColor>│</Text></Box>
      {/* Right column */}
      <Box flexDirection="column" width={26} paddingLeft={1}>
        <StatsColumn stats={stats} />
        <Text dimColor>{'─'.repeat(24)}</Text>
        <Text dimColor>/search  /stats  /help</Text>
      </Box>
    </Box>
  );
}
```

Ink's `<Box>` with flexbox handles the two-column layout that was manual line math before. `borderStyle="round"` gives us the `╭╮╰╯` box for free.

### Step 4.2: Integrate into ProjectsView

**Modified file**: `packages/cli/src/views/projects-view.tsx`

Conditionally render `<WelcomePanel>` when this view is at the bottom of the stack (home).

### Step 4.3: Responsive breakpoints

Use `useStdout()` from Ink to get terminal width:
- Width ≥70: two-column panel
- Width 50-69: single-column panel (no stats)
- Width <50: skip panel, inline header only

### Step 4.4: Verify

- Welcome panel shows on home screen with correct stats
- Right column shows stats
- Panel disappears when navigating into sessions
- Panel reappears when pressing Esc back to projects
- Responsive layout at different terminal widths

---

## Phase 5: Command Mode

**Goal**: `/` key activates command input with live autocomplete below the prompt.

### Step 5.1: Create command registry

**New file**: `packages/cli/src/views/commands.ts` (~50 lines)

Command definitions with names, aliases, args, descriptions, context requirements. Pure data — no React.

### Step 5.2: Create CommandInput component

**New file**: `packages/cli/src/views/command-input.tsx` (~120 lines)

```tsx
function CommandInput({ onExecute, onCancel }: CommandInputProps) {
  const [input, setInput] = useState('');
  const [selectedSuggestion, setSelectedSuggestion] = useState(-1);
  const filtered = matchCommands(input);

  useInput((char, key) => {
    if (key.escape) onCancel();
    if (key.return) {
      if (selectedSuggestion >= 0) {
        // Fill from suggestion
        setInput('/' + filtered[selectedSuggestion].name + ' ');
      } else {
        onExecute(input);
      }
    }
    if (key.tab && filtered.length > 0) {
      // Fill first match
      setInput('/' + filtered[0].name + ' ');
    }
    if (key.upArrow) setSelectedSuggestion(s => Math.max(s - 1, -1));
    if (key.downArrow) setSelectedSuggestion(s => Math.min(s + 1, filtered.length - 1));
  });

  return (
    <Box flexDirection="column">
      <HRule />
      <Box>
        <Text>❯ /</Text>
        <TextInput value={input} onChange={setInput} />
      </Box>
      <HRule />
      {filtered.map((cmd, i) => (
        <SuggestionRow key={cmd.name} command={cmd} selected={i === selectedSuggestion} />
      ))}
    </Box>
  );
}
```

Uses `<TextInput>` from `@inkjs/ui` — cursor positioning, character insertion, backspace all handled automatically. This was the main pain point with raw ANSI that motivated the Ink migration.

### Step 5.3: Integrate into Shell

**Modified file**: `packages/cli/src/views/shell.tsx`

- Add `commandMode` state
- `/` key in any view → `setCommandMode(true)`
- When command mode active, render `<CommandInput>` instead of `<Footer>`
- Suggestion list renders below the prompt (not overlaying content)
- `onExecute` → parse command, resolve context, push appropriate view
- `onCancel` → exit command mode

### Step 5.4: Add flash messages

**Modified file**: `packages/cli/src/views/shell.tsx`

Flash message state with auto-dismiss timer:

```tsx
const [flash, setFlash] = useState<string | null>(null);
useEffect(() => {
  if (flash) {
    const timer = setTimeout(() => setFlash(null), 2000);
    return () => clearTimeout(timer);
  }
}, [flash]);
```

Renders in footer area when active.

### Step 5.5: Verify

- `/` enters command mode with `❯` prompt
- Typing filters suggestions below the prompt
- ↑/↓ selects suggestions, Tab/Enter fills
- Enter executes command
- Esc cancels
- `/ error handling` triggers search shorthand
- Unknown command shows flash error
- Context errors flash when missing project/session

---

## Phase 6: Feature Views

**Goal**: Implement all slash command views. Each is independent.

### Step 6.1: HelpView

**New file**: `packages/cli/src/views/help-view.tsx` (~50 lines)

Static content rendered with `<Box>` and `<Text>`. Three sections: Navigation, Commands, Filters. Any key pops.

### Step 6.2: StatsView

**New file**: `packages/cli/src/views/stats-view.tsx` (~80 lines)

Uses Ink's flexbox for the two-column stats grid. Bar chart with `<Text>` and `█` characters.

```tsx
<Box>
  <Box flexDirection="column" width="50%">
    <StatRow label="Projects" value="38" />
    <StatRow label="Messages" value="86,412" />
  </Box>
  <Box flexDirection="column" width="50%">
    <StatRow label="Sessions" value="1,247" />
    <StatRow label="DB size" value="14.2 MB" />
  </Box>
</Box>
```

### Step 6.3: SearchView

**New file**: `packages/cli/src/views/search-view.tsx` (~130 lines)

The most complex feature view:
- Search result cards with match snippet
- Enter on result does multi-push navigation
- `/` starts new search (replaces current view)

### Step 6.4: MemoryView

**New file**: `packages/cli/src/views/memory-view.tsx` (~50 lines)

Scrollable text. Basic markdown rendering: `#` → bold, lists preserved, code → dim.

### Step 6.5: TodosView

**New file**: `packages/cli/src/views/todos-view.tsx` (~60 lines)

List with status icons: `✓` green, `○` white, `◐` yellow.

### Step 6.6: PlanView

**New file**: `packages/cli/src/views/plan-view.tsx` (~50 lines)

Scrollable text. Same markdown rendering as MemoryView.

### Step 6.7: SubagentsView

**New file**: `packages/cli/src/views/subagents-view.tsx` (~90 lines)

Card list. Enter pushes MessagesView scoped to subagent transcript.

### Step 6.8: Wire all views into Shell

**Modified file**: `packages/cli/src/views/shell.tsx`

Update command execution switch to create and push each view component.

### Step 6.9: Verify each view

For each view: slash command launches it, content renders, scroll works, Esc pops, empty states display.

---

## Phase 7: Polish

### Step 7.1: Search → navigate flow

Implement multi-push on search result Enter. SessionsView and MessagesView accept optional `initialIndex` props.

### Step 7.2: Command history

In-memory array of recent commands. `↑` in command mode (when no suggestion selected) recalls previous.

### Step 7.3: Welcome panel responsive breakpoints

Test and tune at 50, 70, 80, 100, 120 column widths.

### Step 7.4: Warm start fast path

Skip boot screen if init <200ms.

### Step 7.5: One-off command error JSON

Standardize error output: `{ "error": "code", "message": "description" }` with exit codes.

### Step 7.6: Remove unused dependencies

Remove `cli-truncate` from `package.json` (replaced by Ink's layout).

### Step 7.7: Update tests

- Update or remove `tui.test.ts` (no longer relevant — Ink handles terminal)
- Update or remove `interactive-list.test.ts` (replaced by Ink components)
- Add snapshot tests for Ink components using `ink-testing-library`

### Step 7.8: Build and release

- `pnpm typecheck`
- `pnpm build`
- `pnpm test`
- Manual testing of all views and commands
- Update README.md
- Changeset for new version

---

## File Summary

### New files (22)

| File | Phase | Est. lines |
|------|-------|-----------|
| `views/types.ts` | 1 | ~50 |
| `views/context.tsx` | 1 | ~20 |
| `views/hooks.ts` | 1 | ~60 |
| `views/chrome.tsx` | 1 | ~80 |
| `views/shell.tsx` | 1, 3, 5 | ~200 |
| `views/display-items.ts` | 1 | ~200 |
| `views/projects-view.tsx` | 1, 4 | ~120 |
| `views/sessions-view.tsx` | 1 | ~100 |
| `views/messages-view.tsx` | 1 | ~250 |
| `views/detail-view.tsx` | 1 | ~100 |
| `views/boot-view.tsx` | 3 | ~80 |
| `views/wordmark.tsx` | 3 | ~20 |
| `views/welcome-panel.tsx` | 4 | ~70 |
| `views/commands.ts` | 5 | ~50 |
| `views/command-input.tsx` | 5 | ~120 |
| `views/help-view.tsx` | 6 | ~50 |
| `views/stats-view.tsx` | 6 | ~80 |
| `views/search-view.tsx` | 6 | ~130 |
| `views/memory-view.tsx` | 6 | ~50 |
| `views/todos-view.tsx` | 6 | ~60 |
| `views/plan-view.tsx` | 6 | ~50 |
| `views/subagents-view.tsx` | 6 | ~90 |
| `views/index.ts` | 1 | ~20 |

### Modified files (6)

| File | Phase | Change |
|------|-------|--------|
| `package.json` | 0 | Add `ink`, `react`, `@inkjs/ui`, `@types/react`. Remove `cli-truncate`. |
| `tsconfig.json` | 0 | Add `"jsx": "react-jsx"` |
| `bin.ts` | 2, 3 | TTY detection, Ink render for bare command |
| `commands/projects.ts` | 2 | Remove TUI delegation, always static |
| `commands/dashboard.ts` | 2 | Add `summaryJSON()` |
| `index.ts` | 2 | Remove `--no-interactive` flag |

### Deleted files (3)

| File | Phase | Reason |
|------|-------|--------|
| `commands/browse.ts` | 1 | Replaced by Ink views (1318 lines) |
| `lib/tui.ts` | 1 | Replaced by Ink (187 lines) |
| `lib/interactive-list.ts` | 1 | Replaced by Ink components (141 lines) |

### Unchanged files

All one-off command implementations (`sessions.ts`, `messages.ts`, `search.ts`, `stats.ts`, `memory.ts`, `todos.ts`, `subagents.ts`, `plan.ts`, `export.ts`).

All shared libs (`format.ts`, `color.ts`, `table.ts`, `terminal.ts`, `pager.ts`, `resolve.ts`, `error.ts`, `message-render.ts`, `updater.ts`).

---

## Dependency Graph

```
Phase 0 ──→ Phase 1 ──→ Phase 2 ──→ Phase 3 ──→ Phase 4 ──→ Phase 5 ──→ Phase 6 ──→ Phase 7
(ink setup)  (views)     (entry)      (boot)      (welcome)   (commands)   (features)   (polish)

Phase 6 substeps are independent:
  6.1 HelpView
  6.2 StatsView
  6.3 SearchView       (can be built in any order)
  6.4 MemoryView
  6.5 TodosView
  6.6 PlanView
  6.7 SubagentsView
```

---

## Why Ink Changes the Plan

Compared to the original raw-ANSI plan:

| Concern | Raw ANSI | Ink |
|---------|----------|-----|
| Alt screen, raw mode, cleanup | Manual in `tui.ts` | Ink handles automatically |
| Key handling | Manual `parseKeypress()` | `useInput()` hook |
| Text input (command mode) | Hand-code cursor, insertion, backspace | `<TextInput>` from `@inkjs/ui` |
| Progress bar (boot screen) | Hand-code bar rendering | `<ProgressBar>` from `@inkjs/ui` |
| Two-column layout (welcome panel) | Manual column math | `<Box>` flexbox |
| Box borders (welcome panel) | Manual `╭╮╰╯│─` rendering | `borderStyle="round"` prop |
| Scrollable content | Manual offset + viewport math | React state + `useInput` |
| View stack | Manual array + imperative render | React state + conditional rendering |
| Terminal resize | Manual event listener + re-render | Ink handles automatically |
| Diff rendering | Full redraw every frame | Ink diffs terminal output |
| Component reuse | Copy-paste render functions | React components |
| Testing | Manual terminal mocking | `ink-testing-library` snapshots |

**Net effect**: ~600 fewer lines of hand-written infrastructure code. The views themselves are similar in size, but the plumbing around them (terminal management, input parsing, render loop, layout math) is handled by Ink.

---

## Risk Assessment

| Risk | Impact | Mitigation |
|------|--------|------------|
| Ink's React reconciler overhead | Low | Claude Code uses Ink at scale — proven performant |
| 256-color ANSI in Ink components | Medium | Ink supports raw ANSI via `<Text>` with escape codes. Test early in Phase 1.8 |
| Bundle size increase (~490 KB) | Low | Acceptable for a CLI that already ships `better-sqlite3` |
| tsup TSX configuration | Low | Test in Phase 0, fix before proceeding |
| `@inkjs/ui` TextInput behavior | Medium | Test command mode input early in Phase 5.2 |
| Ink fullscreen mode quirks | Medium | Use `render()` with `{ exitOnCtrlC: true }`. Test alt-screen behavior |

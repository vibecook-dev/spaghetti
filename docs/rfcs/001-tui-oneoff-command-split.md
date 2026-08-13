# RFC 001: TUI / One-Off Command Split

**Status**: Historical UI design; synchronous data-access examples superseded
by [RFC 011](./011-rust-observation-query-engine.md)
**Created**: 2026-03-29
**Updated**: 2026-03-29
**Author**: James Yong + Claude

---

## Summary

Redesign spaghetti's CLI around two entry points:

1. **`spag`** (bare) launches a multi-mode interactive TUI with a **view stack** and **slash commands**
2. **`spag <command>`** runs a stateless one-off query for AI agents, scripts, and pipes

The TUI is a proper multi-state application modeled after Claude Code's REPL — a persistent shell where slash commands navigate between different views (browse, search, stats, memory, etc.) and Esc always takes you back.

---

## Motivation

### Problem 1: The bare command is wasted

`spag` with no arguments prints a static dashboard that users see once and forget. The interactive TUI — the best feature — is buried behind `spag p`. New users don't know it exists.

### Problem 2: No user/agent boundary

`spag p` launches a TUI on a TTY but a static table when piped. This conflation means agents accidentally get TUIs and users accidentally get static tables. The `--no-interactive` flag exists solely to work around this.

### Problem 3: Features are isolated

To search, a user must quit the TUI, run `spag search "error"`, read results, then re-enter the TUI. Same for stats, memory, todos, plan. There's no way to flow between features without leaving the interactive context.

### Problem 4: The current TUI is a flat state machine

`browse.ts` has a 4-state enum (`projects | sessions | messages | detail`) with transitions hardcoded in switch statements. Adding a search view means adding a 5th state. Adding stats means a 6th. Each new feature exponentially increases the transition logic. This doesn't scale.

### Solution

A **view stack** architecture with a **command palette**, modeled after Claude Code and vim.

---

## Prior Art

### Claude Code

Claude Code's interaction model:

```
┌──────────────────────────────────────────┐
│ Main REPL                                │
│   Type prompts → get responses           │
│   /help → show help (overlay)            │
│   /model → change model (modal)          │
│   /compact → compact history (action)    │
│   /review → enter review mode (new view) │
│                                          │
│   Every mode has Esc to go back           │
│   The REPL is always "home"              │
└──────────────────────────────────────────┘
```

Key patterns:
- **Home base** — there's always a "main" view you return to
- **Slash commands** — discoverable, typed, some change mode
- **Transient vs persistent** — some commands flash info, others enter a new mode
- **Esc is universal back** — always returns to previous context

### Vim

```
Normal mode ─── / ──→ Search mode (type query, Enter to find, n/N to navigate)
      │──── : ──→ Command mode (type command, Enter to execute)
      │──── i ──→ Insert mode (type text, Esc to return)
      └──── v ──→ Visual mode (select text, Esc to return)
```

Key patterns:
- **Modal** — each mode has its own keybindings and purpose
- **Single-key mode entry** — `/` for search, `:` for commands
- **Esc is universal exit** — always returns to normal mode

### Truffle

Single-state TUI (ratatui dashboard). Works because truffle has few features. Doesn't scale to spaghetti's feature set.

---

## Tech Stack Decision: Ink (React for CLIs)

The TUI is built with **Ink** (React for CLIs) — the same framework used by Claude Code, Gemini CLI, Prisma, and Shopify CLI.

**Why Ink:**
- Same stack as Claude Code itself — spaghetti is browsing Claude Code data, so matching its TUI framework is natural
- React component model maps cleanly to views — `<ProjectsView>`, `<SearchView>`, etc.
- Flexbox layout (Yoga) solves responsive column math (welcome panel, stats grid) without manual line arithmetic
- `useInput` hook for key handling, `<TextInput>` from `@inkjs/ui` for command mode
- `@inkjs/ui` gives us ProgressBar, Spinner, Select, TextInput for free
- Large ecosystem, well-maintained (2.8M weekly downloads, v6.8)
- Contributors can write views in familiar React/TSX

**New dependencies:**
- `ink` (v6.8) — React reconciler for terminals
- `react` (v19) — peer dependency
- `@inkjs/ui` (v2) — TextInput, Spinner, ProgressBar, Select components

**Removed dependencies (replaced by Ink):**
- Custom `lib/tui.ts` — Ink handles alt screen, raw mode, render loop, cleanup
- Custom `lib/interactive-list.ts` — replaced by Ink components with `useInput`
- `cli-truncate` — Ink's `<Text>` handles truncation via `<Box width={n}>`

---

## Architecture: The View Stack

### Core concept

The TUI maintains a **stack of views**. Each view is a React component. Navigation pushes views onto the stack; Esc pops them.

```
View Stack (conceptual):

  [ProjectsView]                          ← spag launches here
  [ProjectsView, SessionsView]            ← Enter on a project
  [ProjectsView, SessionsView, MessagesView]  ← Enter on a session
  [ProjectsView, SessionsView, MessagesView, DetailView]  ← Enter on a message

Slash commands push onto the stack too:

  [ProjectsView, SearchView]              ← /search "error" from projects
  [ProjectsView, StatsView]               ← /stats from projects
  [ProjectsView, SessionsView, MemoryView]  ← /memory from sessions

Esc always pops:

  [ProjectsView, SessionsView, MemoryView]
                                   ↑ Esc
  [ProjectsView, SessionsView]
```

### View as React component

Each view is a React functional component that receives navigation callbacks via context:

```tsx
// View context — provided by the Shell
interface ViewNav {
  push(view: ViewEntry): void;
  pop(): void;
  replace(view: ViewEntry): void;
  quit(): void;
  enterCommandMode(): void;
  context: ViewContext;   // current project/session from stack
}

// A view stack entry
interface ViewEntry {
  type: ViewType;
  component: React.FC;
  breadcrumb: string;
}

// Example view
function ProjectsView() {
  const { push } = useViewNav();
  const [selected, setSelected] = useState(0);
  const projects = useProjects();  // data hook

  useInput((input, key) => {
    if (key.return) {
      push({
        type: 'sessions',
        component: () => <SessionsView project={projects[selected]} />,
        breadcrumb: projects[selected].folderName,
      });
    }
    if (key.escape) quit();
  });

  return (
    <Box flexDirection="column">
      {projects.map((p, i) => (
        <ProjectCard key={p.slug} project={p} selected={i === selected} />
      ))}
    </Box>
  );
}
```

### The Shell component

The top-level Ink component that manages the view stack, command mode, and chrome:

```tsx
function Shell({ api }: { api: SpaghettiAPI }) {
  const [stack, setStack] = useState<ViewEntry[]>([
    { type: 'projects', component: ProjectsView, breadcrumb: 'Projects' }
  ]);
  const [commandMode, setCommandMode] = useState(false);

  const nav: ViewNav = {
    push: (view) => setStack(s => [...s, view]),
    pop: () => setStack(s => s.length > 1 ? s.slice(0, -1) : s),
    replace: (view) => setStack(s => [...s.slice(0, -1), view]),
    quit: () => process.exit(0),
    enterCommandMode: () => setCommandMode(true),
    context: deriveContext(stack),
  };

  const top = stack[stack.length - 1];
  const breadcrumb = stack.map(v => v.breadcrumb).join(' › ');

  return (
    <ViewNavProvider value={nav}>
      <Box flexDirection="column" height="100%">
        <Header breadcrumb={breadcrumb} />
        <Box flexGrow={1}>
          <top.component />
        </Box>
        {commandMode
          ? <CommandInput onExecute={handleCommand} onCancel={() => setCommandMode(false)} />
          : <Footer hints={top.hints} />}
      </Box>
    </ViewNavProvider>
  );
}
```

### Key insight: views are self-contained React components

Each view owns its data and rendering. The shell only handles:
1. The view stack (push/pop/replace via React state)
2. Command mode input (`<CommandInput>` component)
3. Header (breadcrumb) and footer (hints)
4. Terminal lifecycle (Ink handles alt screen, cleanup automatically)

This means adding a new feature = adding a new React component. No modification to the shell or other views.

---

## View Catalog

### Core Navigation Views

These form the primary drill-down hierarchy. Enter pushes the next level, Esc pops back.

#### `ProjectsView`

The home base. Always at the bottom of the stack.

```
  Projects (38)
  ────────────────────────────────────────────

  ▎ spaghetti                        main
    "let's work on the TUI redesign..."
    12 sessions · 3.8k msgs · 1.2M tokens · 2h ago

    voyager                           feat/analysis
    "analyze the chord progression..."
    8 sessions · 2.1k msgs · 890k tokens · yesterday

  ────────────────────────────────────────────
  ↑↓ navigate  ⏎ open  / command  q quit
```

- **Data**: `api.getProjectList()` sorted by last active
- **Enter**: Push `SessionsView` for selected project
- **Keybindings**: ↑↓ navigate, Enter open, / command, q quit

#### `SessionsView`

Sessions for a project.

```
  spaghetti › Sessions (12)
  ────────────────────────────────────────────

  ▎ #1  main                         a1b2c3d4
    "let's work on the TUI redesign..."
    247 msgs · 156k tokens · 45m · 2h ago

    #2  feat/browse                   e5f6g7h8
    "add interactive browser to spag p..."
    89 msgs · 52k tokens · 20m · yesterday

  ────────────────────────────────────────────
  ↑↓ navigate  ⏎ open  Esc back  / command  q quit
```

- **Data**: `api.getSessionList(slug)` sorted by last update
- **Enter**: Push `MessagesView` for selected session
- **Esc**: Pop (back to projects)

#### `MessagesView`

Messages for a session, with filter chips and tool call merging.

```
  spaghetti › #1 › Messages (142/247)
  1:user 2:claude 3:thinking 4:tools 5:system 6̶:̶i̶n̶t̶e̶r̶n̶a̶l̶
  ────────────────────────────────────────────

                        2h ago  USER
   let's work on the TUI redesign...

   CLAUDE  2h ago
   I'll start by reading the current implementation...

   file Read  packages/cli/src/commands/browse.ts  ✓ 1318 lines
   shell Bash  git log --oneline -20  ✓ 20 lines

  ────────────────────────────────────────────
  ↑↓ navigate  1-6 filter  ⏎ open  Esc back  / command  q quit
```

- **Data**: `api.getSessionMessages()` + `buildDisplayItems()` + filters
- **Enter**: Push `DetailView` for selected item
- **1-6**: Toggle filter categories
- **Esc**: Pop (back to sessions)
- **Auto-pagination**: Loads older messages when scrolling near bottom

#### `DetailView`

Full content of a message, tool call, or thinking block. Scrollable text.

```
  spaghetti › #1 › Message 3 (assistant · 2h ago)
  ────────────────────────────────────────────

  I'll start by reading the current implementation
  to understand the browse command structure.

  The current `browse.ts` implements a 4-state
  hierarchical browser...

  [continues]

  ────────────────────────────────────────────
  ↑↓ scroll  Esc back  / command  [12 / 47 lines]
```

- **Data**: Rendered message content (text, tool input/output, thinking)
- **↑↓**: Scroll
- **Esc**: Pop (back to messages)

### Feature Views (pushed by slash commands)

These are independent views pushed onto the stack by slash commands. They can be invoked from any level.

#### `SearchView`

```
  Search: "architecture"  (47 results)
  ────────────────────────────────────────────

  ▎ spaghetti · #3 · 2 days ago
    "...discussing the Architecture C plan for..."

    spaghetti · #1 · 5 days ago
    "...reviewed the architecture document and..."

    voyager · #2 · 1 week ago
    "...clean architecture pattern for the..."

  ────────────────────────────────────────────
  ↑↓ navigate  ⏎ jump to message  Esc back  / new search
```

- **Pushed by**: `/search <query>` or `/ <query>` (single slash + space + text)
- **Data**: `api.search({ text: query })`
- **Enter**: Navigate to the matched message in context — pushes `[SessionsView, MessagesView, DetailView]` with the correct project/session/message selected
- **Esc**: Pop (back to wherever you were)
- **`/`**: Start a new search (replace this view)

#### `StatsView`

```
  Stats
  ────────────────────────────────────────────

  Overview
    Projects:     38
    Sessions:     1,247
    Messages:     86,412

  Token Usage
    Input:          12.4M
    Output:         8.7M
    Cache read:     45.2M
    ─────────────────────────
    Total:          66.3M

  Top Projects by Tokens
    spaghetti        ████████████████████  1.2M
    voyager          ████████████         890k
    jabali-editor    ██████████           780k
    study-portfolio  █████                420k
    duke-os          ███                  280k

  ────────────────────────────────────────────
  ↑↓ scroll  Esc back
```

- **Pushed by**: `/stats` or `/st`
- **Data**: `api.getStats()` + `api.getProjectList()` — same data as `statsCommand`
- **↑↓**: Scroll (content may exceed viewport)
- **Esc**: Pop

#### `MemoryView`

```
  spaghetti › Memory
  ────────────────────────────────────────────

  # Memory Index

  - [project_spaghetti_audit.md](...)
    Full audit results: type gaps, new .claude dirs

  - [project_cli_redesign.md](...)
    CLI redesign: TUI as default, slash commands

  ────────────────────────────────────────────
  ↑↓ scroll  Esc back
```

- **Pushed by**: `/memory` or `/mem`
- **Context**: Uses the project from the current navigation position. If at root (no project selected), shows a project picker or prompts "select a project first"
- **Data**: `api.getProjectMemory(slug)`
- **Esc**: Pop

#### `TodosView`

```
  spaghetti › #1 › Todos
  ────────────────────────────────────────────

  ✓ Read the current browse.ts implementation
  ✓ Design view stack architecture
  ○ Implement TUIShell class
  ○ Add slash command parser
  ○ Implement SearchView

  ────────────────────────────────────────────
  ↑↓ scroll  Esc back
```

- **Pushed by**: `/todos` or `/t`
- **Context**: Requires a session. If at project level, uses the latest session. If at root, shows error.
- **Data**: `api.getSessionTodos(slug, sessionId)`
- **Esc**: Pop

#### `PlanView`

```
  spaghetti › #1 › Plan
  ────────────────────────────────────────────

  ## Implementation Plan

  Phase 1: Entry point restructure
    1. Modify bin.ts for TTY detection
    2. Move browse to default entry
    ...

  ────────────────────────────────────────────
  ↑↓ scroll  Esc back
```

- **Pushed by**: `/plan` or `/pl`
- **Context**: Same as todos — needs a session
- **Data**: `api.getSessionPlan(slug, sessionId)`
- **Esc**: Pop

#### `SubagentsView`

```
  spaghetti › #1 › Subagents (3)
  ────────────────────────────────────────────

  ▎ Explore  agent-abc123
    "Explore truffle TUI and CLI design"

    Plan  agent-def456
    "Design view stack architecture"

    general-purpose  agent-ghi789
    "Search for slash command implementations"

  ────────────────────────────────────────────
  ↑↓ navigate  ⏎ view transcript  Esc back
```

- **Pushed by**: `/subagents` or `/sub`
- **Enter**: Push a `MessagesView` showing the subagent's transcript
- **Context**: Needs a session
- **Esc**: Pop

#### `HelpView`

```
  Help
  ────────────────────────────────────────────

  Navigation
    ↑ ↓       Move selection up/down
    Enter     Open / drill into
    Esc       Go back
    q         Quit

  Commands (press / then type)
    /search <query>    Search all messages
    /stats             Usage statistics
    /memory            Project MEMORY.md
    /todos             Session todos
    /plan              Session plan
    /subagents         Subagent transcripts
    /export            Export current view
    /help              This help screen

  Filters (messages view)
    1  user        2  claude      3  thinking
    4  tools       5  system      6  internal

  ────────────────────────────────────────────
  Press any key to dismiss
```

- **Pushed by**: `/help` or `/?` or `?`
- **Any key**: Pop (dismiss)

---

## Command Mode

### Activation

Pressing `/` at any view enters command mode. The footer transforms into an input line:

```
Before:
  ────────────────────────────────────────────
  ↑↓ navigate  ⏎ open  / command  q quit

After:
  ────────────────────────────────────────────
  / █                                    Esc cancel
```

### Input handling

In command mode, the TUI shell intercepts all keys:

- **Printable characters** → append to input buffer
- **Backspace** → delete last character
- **Enter** → parse and execute command
- **Escape** → cancel, return to navigation
- **Tab** → cycle through matching command completions

### Command parsing

```typescript
function parseCommand(input: string): { name: string; args: string } | null {
  const trimmed = input.trim();
  if (!trimmed) return null;

  // "/search foo bar" → { name: 'search', args: 'foo bar' }
  // "/ foo bar" → shorthand for search
  const match = trimmed.match(/^(\S+)\s*(.*)/);
  if (!match) return null;

  const name = match[1].toLowerCase();
  const args = match[2].trim();

  return { name, args };
}
```

### Command routing

```typescript
const COMMANDS: Record<string, CommandDef> = {
  'search':    { aliases: ['s', 'find', 'grep'], needsArgs: true,  handler: searchHandler },
  'stats':     { aliases: ['st'],                 needsArgs: false, handler: statsHandler },
  'memory':    { aliases: ['mem'],                needsArgs: false, handler: memoryHandler },
  'todos':     { aliases: ['t'],                  needsArgs: false, handler: todosHandler },
  'plan':      { aliases: ['pl'],                 needsArgs: false, handler: planHandler },
  'subagents': { aliases: ['sub'],                needsArgs: false, handler: subagentsHandler },
  'export':    { aliases: ['x'],                  needsArgs: false, handler: exportHandler },
  'help':      { aliases: ['?', 'h'],             needsArgs: false, handler: helpHandler },
  'quit':      { aliases: ['q'],                  needsArgs: false, handler: () => ({ type: 'quit' }) },
};
```

### Shorthand: bare search

Typing `/ error handling` (slash, space, then text) is treated as `/search error handling`. This makes the most common operation fastest — just like vim's `/` for search.

### Tab completion

When typing in command mode, Tab cycles through matching commands:

```
/st█        → Tab → /stats
/se█        → Tab → /search
/su█        → Tab → /subagents
```

Only command names are completed, not arguments.

### Error feedback

Invalid commands show a brief flash message in the footer:

```
  ────────────────────────────────────────────
  Unknown command: "foo". Type /help for available commands.
```

The message auto-dismisses after 2 seconds or on any keypress.

---

## Context Passing

Slash commands inherit context from the current navigation position.

### Context resolution

```typescript
interface ViewContext {
  project?: ProjectListItem;    // set when inside a project
  session?: SessionListItem;    // set when inside a session
  messageIndex?: number;        // set when inside messages
}
```

The TUI shell derives context from the view stack:

```typescript
function deriveContext(stack: View[]): ViewContext {
  const ctx: ViewContext = {};
  for (const view of stack) {
    if (view instanceof SessionsView) ctx.project = view.project;
    if (view instanceof MessagesView) {
      ctx.project = view.project;
      ctx.session = view.session;
    }
    // ... etc
  }
  return ctx;
}
```

### Context requirements

| Command | Needs project? | Needs session? | Fallback |
|---------|---------------|----------------|----------|
| `/search` | No | No | Searches all projects |
| `/stats` | No | No | Shows global stats |
| `/memory` | Yes | No | Error: "Navigate to a project first" |
| `/todos` | Yes | Yes | Uses latest session for selected project |
| `/plan` | Yes | Yes | Uses latest session for selected project |
| `/subagents` | Yes | Yes | Uses latest session for selected project |
| `/export` | Optional | Optional | Exports everything, or scoped to current context |
| `/help` | No | No | Always available |

---

## Search → Navigate Flow

The most complex interaction: pressing Enter on a search result navigates to that message in context.

### How it works

1. User types `/search architecture`, SearchView is pushed
2. Results show project + session + snippet for each match
3. User presses Enter on a result
4. The TUI:
   a. Pops the SearchView
   b. Pushes SessionsView for the result's project (pre-selecting the right session)
   c. Pushes MessagesView for the result's session (pre-scrolling to the right message)
   d. Pushes DetailView for the matched message

This is a **multi-push**: one Enter press pushes 3 views. The user can then Esc back through each level naturally.

```
Before Enter:
  [ProjectsView, SearchView]

After Enter on a result in "spaghetti" project, session #3, message 42:
  [ProjectsView, SessionsView(spaghetti), MessagesView(#3, scrolled to 42), DetailView(msg 42)]
```

---

## One-Off Commands (Agent Interface)

Unchanged from RFC v1. All subcommands are stateless, non-interactive, exit after output.

```bash
spag p --json              # list projects
spag s . --json            # sessions for cwd project
spag m . 1 --json          # messages from latest session
spag search "error" --json # full-text search
spag st --json             # usage statistics
```

Every command guarantees: no raw mode, no alt screen, no prompts, pipe-safe, `--json` for structured output.

---

## File Changes

### Modified files

| File | Change |
|------|--------|
| `packages/cli/package.json` | Add `ink`, `react`, `@inkjs/ui` dependencies. Add `tsup` JSX config. |
| `packages/cli/tsconfig.json` | Enable `"jsx": "react-jsx"` for TSX support |
| `packages/cli/src/bin.ts` | TTY detection → render `<Shell>` via Ink for bare command |
| `packages/cli/src/commands/projects.ts` | Remove `browseCommand` import and TUI delegation. Always static. |
| `packages/cli/src/index.ts` | Remove `--no-interactive` / `--interactive` flags |

### New files (TSX components)

| File | Purpose |
|------|---------|
| `packages/cli/src/views/types.ts` | `ViewEntry`, `ViewNav`, `ViewContext`, `CommandDef` types |
| `packages/cli/src/views/shell.tsx` | `<Shell>` — root Ink component, view stack, command mode |
| `packages/cli/src/views/context.tsx` | `ViewNavProvider` + `useViewNav()` React context |
| `packages/cli/src/views/hooks.ts` | Shared hooks: `useProjects()`, `useTerminalSize()`, etc. |
| `packages/cli/src/views/chrome.tsx` | `<Header>`, `<Footer>`, `<Breadcrumb>`, `<HRule>` components |
| `packages/cli/src/views/boot-view.tsx` | `<BootView>` — loading screen with progress bar |
| `packages/cli/src/views/welcome-panel.tsx` | `<WelcomePanel>` — box-drawn panel with wordmark + stats |
| `packages/cli/src/views/projects-view.tsx` | `<ProjectsView>` with `<ProjectCard>` |
| `packages/cli/src/views/sessions-view.tsx` | `<SessionsView>` with `<SessionCard>` |
| `packages/cli/src/views/messages-view.tsx` | `<MessagesView>` with display item components |
| `packages/cli/src/views/detail-view.tsx` | `<DetailView>` — scrollable message/tool/thinking content |
| `packages/cli/src/views/search-view.tsx` | `<SearchView>` — query results with jump-to-message |
| `packages/cli/src/views/stats-view.tsx` | `<StatsView>` — usage statistics dashboard |
| `packages/cli/src/views/memory-view.tsx` | `<MemoryView>` — project MEMORY.md |
| `packages/cli/src/views/todos-view.tsx` | `<TodosView>` — session todo list |
| `packages/cli/src/views/plan-view.tsx` | `<PlanView>` — session plan |
| `packages/cli/src/views/subagents-view.tsx` | `<SubagentsView>` — subagent list + transcript drill-down |
| `packages/cli/src/views/help-view.tsx` | `<HelpView>` — keybindings and command reference |
| `packages/cli/src/views/command-input.tsx` | `<CommandInput>` — slash command input with autocomplete |
| `packages/cli/src/views/display-items.ts` | `DisplayItem` types, `buildDisplayItems()`, tool categories (extracted from browse.ts) |
| `packages/cli/src/views/index.ts` | Barrel export |

### Deleted files

| File | Reason |
|------|--------|
| `packages/cli/src/commands/browse.ts` | Replaced by `views/shell.tsx` + view components |
| `packages/cli/src/lib/tui.ts` | Replaced by Ink (alt screen, raw mode, render loop, cleanup) |
| `packages/cli/src/lib/interactive-list.ts` | Replaced by Ink components with `useInput` |

---

## Implementation Plan

### Phase 1: View stack infrastructure

Extract the view interface and TUIShell from the existing browse.ts. No new features — just restructure.

1. Create `views/types.ts` with `View`, `ViewAction`, `ViewContext`
2. Create `views/shell.ts` with `TUIShell` (view stack, render loop, key dispatch)
3. Extract `ProjectsView` from browse.ts into `views/projects-view.ts`
4. Extract `SessionsView` into `views/sessions-view.ts`
5. Extract `MessagesView` + DisplayItem logic into `views/messages-view.ts`
6. Extract `DetailView` into `views/detail-view.ts`
7. Wire up: `browse.ts` creates `TUIShell`, pushes `ProjectsView`, calls `.run()`
8. Verify: existing navigation works exactly as before

**Test**: All existing behavior preserved. The refactor is purely structural.

### Phase 2: Entry point + one-off split

1. Modify `bin.ts`: bare command + TTY → `TUIShell.run()`, bare + piped → summary JSON
2. `projects.ts`: remove TUI delegation, always static
3. `index.ts`: remove `--interactive` / `--no-interactive` flags

**Test**: `spag` launches TUI, `spag p` shows static table, `spag p --json` shows JSON.

### Phase 3: Command mode

1. Extend `KeyEvent` in `tui.ts` with `/`, backspace, printable chars
2. Add command input state to `TUIShell`
3. Implement command parsing and routing
4. Add `/help` (simplest command — static text)
5. Add `/quit`

**Test**: `/` enters command mode, typing works, Esc cancels, `/help` shows help, `/quit` exits.

### Phase 4: Feature views (incremental, each is independent)

Each view can be shipped independently:

1. `/search` → `SearchView` (query input + results list + jump-to-message)
2. `/stats` → `StatsView` (reuse stats command rendering)
3. `/memory` → `MemoryView` (render MEMORY.md)
4. `/todos` → `TodosView` (render todos)
5. `/plan` → `PlanView` (render plan)
6. `/subagents` → `SubagentsView` (list + drill into transcript)
7. `/export` → export handler (action, not a view)

### Phase 5: Polish

1. Tab completion for command names
2. Command history (↑ in command mode recalls previous)
3. Flash error messages for invalid commands
4. Search result highlighting (bold matched terms)
5. `?` as shortcut for `/help` from any view

---

## Alternatives Considered

### A: Single-file state machine (current approach, extended)

Add more states to the `ViewLevel` enum in browse.ts. Rejected: transitions become O(n^2), the file grows beyond 2000 lines, and every new feature requires modifying the central switch statements.

### B: Raw ANSI with custom view stack (no framework)

Keep the existing `tui.ts` + `interactive-list.ts` and build the view stack on top. Each view returns `string[]` lines. Rejected: workable but requires hand-coding text input (command mode cursor, character insertion), flexbox-style layouts (welcome panel columns), and component reuse patterns that Ink provides out of the box. The additional ~490KB bundle cost is justified by development velocity and maintainability.

### C: OpenTUI (Zig-native TUI)

Newer framework with native Zig core, multi-framework support. Rejected: pre-1.0, requires Bun >=1.2 (spaghetti targets Node), daily breaking changes, too risky for a published npm package.

### D: Plugin-based view system

Views loaded dynamically from separate packages. Rejected: over-engineered for a single-package CLI. The view files are small (50-150 lines each) and benefit from being co-located.

### E: Tabbed interface (all views visible simultaneously)

Show tabs at the top (Browse | Search | Stats | ...) and switch between them. Rejected: tabs don't match the drill-down interaction model. The view stack is more natural for hierarchical data exploration. Also, terminal width is too narrow for meaningful tab labels.

---

## Open Questions

1. **Should search results be scoped by context?** If you're inside a project and type `/search`, should it search only that project or globally? Proposed: global by default, add `/search -p . <query>` for project-scoped. The search view shows which project each result belongs to anyway.

2. **Should `/export` produce output after TUI exit or write to a file?** Proposed: queue the export, clean up TUI, print to stdout. User redirects with `> file.json`.

3. **Should there be a `/goto` command?** E.g., `/goto spaghetti 3` to jump directly to session #3 of spaghetti. Nice to have but not essential for v1.

4. **Should the view stack have a depth limit?** The stack could theoretically grow unbounded (search → jump to result → search again → jump → ...). Proposed: no limit. Memory cost is negligible (each view holds a reference to data, not a copy).

5. **Should `spag <project>` (bare project name) be a shortcut?** E.g., `spag spaghetti` launches TUI at SessionsView for that project. Useful but adds argument-parsing ambiguity. Defer to a follow-up.

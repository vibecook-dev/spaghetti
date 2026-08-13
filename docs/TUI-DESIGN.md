> **Historical data-access examples.** The visual design remains a reference,
> but synchronous `api.*` examples predate RFC 011. Current CLI/TUI reads are
> asynchronous and Rust-backed; see the
> [Phase 10 closure ledger](./rfcs/011-phase-10-closure.md).

# Spaghetti TUI Design

**Status**: Draft
**Created**: 2026-03-29
**Companion to**: RFC 001 (TUI / One-Off Command Split)

Design target: 80 columns baseline, scales naturally to wider terminals.
Visual style: Matches Claude Code and Truffle — box-drawn welcome panel, horizontal rules, half-block wordmark.
Tech stack: **Ink v6** (React for CLIs) + React 19 + @inkjs/ui v2 — same framework as Claude Code and Gemini CLI.

### Implementation notes (Ink-specific)

- **Box borders**: The welcome panel and boot screen use `<Box borderStyle="round">` which renders `╭╮╰╯│─` automatically.
- **Two-column layouts**: Achieved with nested `<Box>` components and flexbox (`flexGrow`, `width`).
- **Key handling**: All `useInput()` hooks from Ink — no custom keypress parsing.
- **Text input**: Command mode uses `<TextInput>` from `@inkjs/ui` — cursor, insertion, backspace all handled.
- **Progress bar**: Boot screen uses `<ProgressBar>` from `@inkjs/ui`.
- **256-color**: Message blocks use raw ANSI escape codes within `<Text>` components for the 256-color palette (Ink passes through raw escapes).
- **Horizontal rules**: `<Text dimColor>{'─'.repeat(cols)}</Text>` using `useStdout()` for terminal width.
- **Render diffing**: Ink automatically diffs terminal output — no manual full-screen redraws.

---

## Boot Screen (loading)

The TUI enters alt screen immediately on launch and shows a boot screen while the core service initializes (scanning `~/.claude`, parsing JSONL files, building SQLite indexes). This takes ~1-6 seconds on first run and ~28ms on warm starts.

### Cold start (first run or data changed)

```
╭ Spaghetti v0.2.2 ───────────────────────────────────────────────────────────╮
│                                                                             │
│  ▄▀▀ █▀█ ▄▀▄ █▀▀ █ █ █▀▀ ▀█▀ ▀█▀ █                                        │
│  ▀▄▄ █▀▀ █▀█ █ █ █▀█ █▀   █   █  █                                         │
│  ▄▄▀ █   █ █ ▀▀▀ ▀ ▀ ▀▀▀  ▀   ▀  ▀                                         │
│                                                                             │
│  untangle your claude code history                                          │
│                                                                             │
│  ████████████████░░░░░░░░░░░░░░░░░░░░  12/38 projects   Parsed spaghetti   │
│                                                                      3.2s  │
╰─────────────────────────────────────────────────────────────────────────────╯
```

### Progress states

The boot screen progresses through these phases:

```
│  ░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░  Initializing...              0.0s  │
```

```
│  ██░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░  Scanning ~/.claude            0.3s  │
```

```
│  ████████████████░░░░░░░░░░░░░░░░░░░░  12/38 projects   Parsed voyager    │
│                                                                      2.1s  │
```

```
│  ████████████████████████████████████  38/38 projects   Building index    │
│                                                                      4.8s  │
```

### Warm start (no changes, fast path)

When the DB is fresh and no re-parsing is needed, the boot screen flashes briefly (~28ms) and transitions directly to the home screen. If the init completes in under 200ms, the boot screen is skipped entirely — go straight to the welcome panel + project list.

### Layout rules

- The boot screen reuses the welcome panel box but with a **single-column layout** (no right-side stats — data isn't loaded yet)
- Wordmark and tagline are centered/left-aligned as usual
- Progress bar sits at the bottom of the box, above the closing `╰───╯`
- Progress bar: `████` green filled + `░░░░` dim empty, 36 chars wide
- Right of bar: `{current}/{total} projects` when parsing, or phase label
- Status text: current action (e.g., `Parsed spaghetti`, `Building index`, `Scanning ~/.claude`)
- Elapsed time: right-aligned, dim, `{n.n}s` format
- The second-to-last line inside the box holds the bar + counts, last line holds elapsed time

### Transition to home screen

When initialization completes:
1. The progress bar fills to 100%
2. Brief flash (100ms) showing completion
3. The boot screen is replaced by the full welcome panel + project list (the boot box smoothly becomes the welcome panel — same shape, content fills in)

### Error during boot

If initialization fails, the boot screen shows the error inside the box:

```
╭ Spaghetti v0.2.2 ───────────────────────────────────────────────────────────╮
│                                                                             │
│  ▄▀▀ █▀█ ▄▀▄ █▀▀ █ █ █▀▀ ▀█▀ ▀█▀ █                                        │
│  ▀▄▄ █▀▀ █▀█ █ █ █▀█ █▀   █   █  █                                         │
│  ▄▄▀ █   █ █ ▀▀▀ ▀ ▀ ▀▀▀  ▀   ▀  ▀                                         │
│                                                                             │
│  ✗ Failed to initialize                                                     │
│                                                                             │
│  Could not find Claude Code data at ~/.claude                               │
│  Is Claude Code installed?                                                  │
│                                                                             │
│  Press q to quit                                                            │
╰─────────────────────────────────────────────────────────────────────────────╯
```

- `✗` in red, error message in white, help text in dim
- `q` exits the TUI cleanly

---

## Welcome Panel (home screen only)

The welcome panel is shown only at the `ProjectsView` (home) level. It collapses to a breadcrumb header when navigating into any other view.

```
╭ Spaghetti v0.2.2 ───────────────────────────────────────────────────────────╮
│                                                    │                        │
│  ▄▀▀ █▀█ ▄▀▄ █▀▀ █ █ █▀▀ ▀█▀ ▀█▀ █               │ Projects          38   │
│  ▀▄▄ █▀▀ █▀█ █ █ █▀█ █▀   █   █  █                │ Sessions       1,247   │
│  ▄▄▀ █   █ █ ▀▀▀ ▀ ▀ ▀▀▀  ▀   ▀  ▀                │ Messages      86,412   │
│                                                    │ Tokens         66.3M   │
│  untangle your claude code history                 │                        │
│                                                    │ ──────────────────────  │
│  ~/.claude · 512 MB · 28ms                         │ /search /stats /help   │
│                                                    │                        │
╰─────────────────────────────────────────────────────────────────────────────╯
```

**Layout rules:**
- Title: `╭ Spaghetti v{VERSION} ───...╯`
- Left column: wordmark (3 lines), blank line, tagline, blank line, data path + perf
- Right column: stats (4 key-value rows), divider, command hints
- Divider: `│` character at a fixed column (~55% width)
- Box uses rounded corners: `╭ ╮ ╰ ╯`

**Responsive behavior:**
- Below 70 cols: right column is dropped, left column fills the box
- Below 50 cols: welcome panel is hidden entirely, show inline header instead

---

## View: ProjectsView (home)

```
╭ Spaghetti v0.2.2 ───────────────────────────────────────────────────────────╮
│                                                    │                        │
│  ▄▀▀ █▀█ ▄▀▄ █▀▀ █ █ █▀▀ ▀█▀ ▀█▀ █               │ Projects          38   │
│  ▀▄▄ █▀▀ █▀█ █ █ █▀█ █▀   █   █  █                │ Sessions       1,247   │
│  ▄▄▀ █   █ █ ▀▀▀ ▀ ▀ ▀▀▀  ▀   ▀  ▀                │ Messages      86,412   │
│                                                    │ Tokens         66.3M   │
│  untangle your claude code history                 │                        │
│                                                    │ ──────────────────────  │
│  ~/.claude · 512 MB · 28ms                         │ /search /stats /help   │
│                                                    │                        │
╰─────────────────────────────────────────────────────────────────────────────╯

  ▎ spaghetti                                                         main
    "let's work on the TUI redesign..."
    12 sessions · 3.8k msgs · 1.2M tokens · 2h ago

    voyager                                                    feat/analysis
    "analyze the chord progression for..."
    8 sessions · 2.1k msgs · 890k tokens · yesterday

    jabali-editor                                                      main
    "fix the texture atlas packing bug..."
    6 sessions · 1.9k msgs · 780k tokens · 3 days ago

    study-portfolio                                                    main
    "set up the 3D portfolio with R3F..."
    4 sessions · 943 msgs · 420k tokens · 1 week ago

─────────────────────────────────────────────────────────────────────────────
↑↓ navigate  ⏎ open  / command  q quit
─────────────────────────────────────────────────────────────────────────────
```

**Card layout (per project):**
- Line 1: `▎ {name}` left-aligned, `{branch}` right-aligned
  - Selected: `▎` in accent color, name bold white, branch in accent
  - Unselected: space prefix, name white, branch dim
- Line 2: `  "{first prompt}"` — italic, dim, truncated to terminal width - 6
- Line 3: `  {sessions} sessions · {msgs} msgs · {tokens} tokens · {relative time}`
  - Selected: counts white, tokens yellow, time dim
  - Unselected: all dim
- Line 4: blank (breathing room between cards)

**Keybindings:**
- `↑` / `↓` — move selection
- `Enter` — push SessionsView for selected project
- `/` — enter command mode
- `q` — quit

**Data source:** `api.getProjectList()` sorted by `lastActiveAt` descending.

---

## View: SessionsView

```
  spaghetti › Sessions (12)
─────────────────────────────────────────────────────────────────────────────

  ▎ #1  main                                                      a1b2c3d4
    "let's work on the TUI redesign..."
    247 msgs · 156k tokens · 45m · 2h ago

    #2  feat/browse                                               e5f6g7h8
    "add interactive browser to spag p..."
    89 msgs · 52k tokens · 20m · yesterday

    #3  main                                                      i9j0k1l2
    "fix the streaming JSONL parser..."
    156 msgs · 98k tokens · 30m · 3 days ago

    #4  feat/worker-pool                                          m3n4o5p6
    "parallelize project parsing with workers..."
    201 msgs · 134k tokens · 1h · 5 days ago

    #5  main                                                      q7r8s9t0
    "add FTS5 search with auto-sync triggers..."
    67 msgs · 41k tokens · 15m · 1 week ago

─────────────────────────────────────────────────────────────────────────────
↑↓ navigate  ⏎ open  Esc back  / command  q quit
─────────────────────────────────────────────────────────────────────────────
```

**Header:**
- Breadcrumb: `{project} › Sessions ({count})`
- Project name dim, "Sessions" in accent, count dim

**Card layout (per session):**
- Line 1: `▎ #{index}  {branch}` left-aligned, `{short session ID}` right-aligned (8 chars, dim)
  - Selected: `▎` yellow, index bold white, branch yellow
  - Unselected: space prefix, index white, branch dim
- Line 2: `  "{first prompt}"` — italic, dim, truncated
- Line 3: `  {msgs} msgs · {tokens} tokens · {duration} · {relative time}`
  - Selected: counts white, tokens yellow, time dim
  - Unselected: all dim
- Line 4: blank

**Keybindings:**
- `↑` / `↓` — move selection
- `Enter` — push MessagesView for selected session
- `Esc` — pop (back to projects, welcome panel reappears)
- `/` — enter command mode
- `q` — quit

**Data source:** `api.getSessionList(projectSlug)` sorted by `lastUpdate` descending.

---

## View: MessagesView

```
  spaghetti › #1 › Messages (247)
  1:user 2:claude 3:thinking 4:tools 5̶:̶s̶y̶s̶t̶e̶m̶ 6̶:̶i̶n̶t̶e̶r̶n̶a̶l̶
─────────────────────────────────────────────────────────────────────────────

                                                            2h ago   USER ▐
   let's work on the TUI redesign, first please write
   up a comprehensive RFC for this

  ▌ CLAUDE  2h ago
   Let me deeply understand the current CLI
   implementation before writing the RFC.

   file  Read    packages/cli/src/index.ts                          ✓
   file  Read    packages/cli/src/bin.ts                            ✓
   file  Read    packages/cli/src/commands/dashboard.ts             ✓
   shell Bash    git log --oneline -20                              ✓
   file  Read    packages/cli/src/commands/browse.ts                ✓
   file  Read    packages/cli/src/lib/tui.ts                        ✓

                                                       1h 50m ago   USER ▐
   good, now let's look at truffle's core and cli

─────────────────────────────────────────────────────────────────────────────
↑↓ navigate  1-6 filter  ⏎ detail  Esc back  / command  q quit
─────────────────────────────────────────────────────────────────────────────
```

**Header:**
- Line 1: Breadcrumb `{project} › #{session index} › Messages ({shown}/{total})`
  - If filters hide some: show `(142/247)`, otherwise `(247)`
- Line 2: Filter chips — `1:user 2:claude 3:thinking 4:tools 5:system 6:internal`
  - Active: key white, label in category color
  - Inactive: key dim, label strikethrough dim

**Display items:**

Messages are pre-processed into display items (existing `buildDisplayItems` logic):
- Tool pairs merged (tool_use + tool_result = one item)
- Thinking blocks extracted as separate items
- Task notification messages merged into Agent tool calls

**User message block:**
```
                                                    {time}   USER ▐
 {preview text, truncated to width}
```
- Right-aligned: timestamp + "USER" label + `▐` edge marker
- Selected: background color 236, label color 79 (teal), text color 79
- Unselected: background color 233, label color 36, text color 36
- Blank line above and below (breathing room)

**Claude message block:**
```
▌ CLAUDE  {time}
 {preview text, truncated to width}
```
- Left-aligned: `▌` edge marker + "CLAUDE" label + timestamp
- Selected: background color 235, label color 216 (peach), text color 216
- Unselected: background color 233, label color 173, text color 173
- Blank line above and below

**Tool call item (single line):**
```
 {category}  {toolName}    {input summary}                    {status}
```
- Category label dim, tool name in category color, input truncated, status right-aligned
- `✓` green for success, `✗` red for error, `·` dim for no result
- Category colors: file=cyan, search=yellow, shell=red, agent=magenta, nav=blue, web=green, mcp=dim cyan

**Thinking item (single line):**
```
 ··· thinking  ~{tokens}  {first line preview}
```
- `···` in magenta (selected) or dim
- Token estimate dim
- Preview italic dim

**System/metadata items (single line):**
```
 {symbol} {description}
```
- Symbols: `⏱` turn duration, `⚠` api error, `◇` compact boundary, `■` stop hook, `◆` generic system, `§` summary, `⟳` progress, `·` internal

**Keybindings:**
- `↑` / `↓` — move selection
- `1`–`6` — toggle filter category
- `Enter` — push DetailView for selected item
- `Esc` — pop (back to sessions)
- `/` — enter command mode
- `q` — quit

**Pagination:** Loads last PAGE_SIZE (50) messages first. Scrolling near bottom loads older messages automatically.

**Data source:** `api.getSessionMessages()` → `buildDisplayItems()` → `applyDisplayFilters()`.

---

## View: DetailView

### Message detail (user or assistant)

```
  spaghetti › #1 › Message 3 (assistant · 2h ago)
─────────────────────────────────────────────────────────────────────────────

  Let me deeply understand the current CLI implementation
  before writing the RFC.

  The current `browse.ts` implements a 4-state hierarchical
  browser with these view levels:

  1. Projects — list all projects sorted by last active
  2. Sessions — list sessions for a selected project
  3. Messages — display items with filter toggles
  4. Detail — scrollable full content of a message

  Each level has its own renderer and key handler, but they
  share a single global `ViewState` object and...

  [continues]

─────────────────────────────────────────────────────────────────────────────
↑↓ scroll  Esc back  / command                              [12 / 47 lines]
─────────────────────────────────────────────────────────────────────────────
```

### Tool call detail

```
  spaghetti › #1 › Read packages/cli/src/index.ts
─────────────────────────────────────────────────────────────────────────────

  Read
  ────

  Input:
    file_path: /Users/james/Projects/.../packages/cli/src/index.ts

  Result:
    /**
     * Program setup — exported for testing
     */

    import { Command } from 'commander';
    import { initService, shutdownService } from './lib/init.js';
    import { dashboardCommand } from './commands/dashboard.js';
    ...

─────────────────────────────────────────────────────────────────────────────
↑↓ scroll  Esc back  / command                             [1 / 354 lines]
─────────────────────────────────────────────────────────────────────────────
```

### Thinking detail

```
  spaghetti › #1 › Thinking (~2.4k tokens)
─────────────────────────────────────────────────────────────────────────────

  The user wants me to write a comprehensive RFC for the
  TUI/one-off command split. Let me first understand what
  exists today...

  The current browse.ts has a flat 4-state machine. Each
  state has its own render function and key handler. The
  transitions are hardcoded in switch statements...

  [continues]

─────────────────────────────────────────────────────────────────────────────
↑↓ scroll  Esc back  / command                              [1 / 128 lines]
─────────────────────────────────────────────────────────────────────────────
```

**Header:**
- Breadcrumb varies by item type:
  - Message: `{project} › #{session} › Message {n} ({role} · {time})`
  - Tool call: `{project} › #{session} › {toolName} {input summary}`
  - Thinking: `{project} › #{session} › Thinking (~{tokens} tokens)`

**Content area:** Full rendered content, indented 2 spaces. Content uses `renderMessage()` for messages, structured key-value for tools, italic for thinking.

**Footer:** Scroll position shown as `[{offset} / {total} lines]` right-aligned.

**Keybindings:**
- `↑` / `↓` — scroll one line
- `Esc` — pop (back to messages)
- `/` — enter command mode
- `q` — quit

---

## View: SearchView

```
  Search: "error handling" (23 results)
─────────────────────────────────────────────────────────────────────────────

  ▎ spaghetti · #3 · 3 days ago                                assistant
    "...the error handling in the streaming parser needs
    to catch JSON parse failures gracefully..."

    voyager · #1 · 1 week ago                                      user
    "add better error handling for the audio analysis
    pipeline when FFT fails..."

    jabali-editor · #5 · 2 weeks ago                           assistant
    "...wrapped the texture loader with error handling
    that falls back to a placeholder..."

    duke-os · #2 · 3 weeks ago                                 assistant
    "...the page fault error handling needs to
    distinguish between lazy allocation faults..."

─────────────────────────────────────────────────────────────────────────────
↑↓ navigate  ⏎ jump to message  Esc back  / new search
─────────────────────────────────────────────────────────────────────────────
```

**Header:** `Search: "{query}" ({count} results)`

**Card layout (per result):**
- Line 1: `▎ {project} · #{session} · {time}` left-aligned, `{role}` right-aligned
  - Selected: `▎` accent, project white, session/time dim, role in accent
  - Unselected: space prefix, all dim
- Line 2-3: `  "{snippet with match context}"` — match terms bolded
  - Snippet: ~2 lines, centered on the match position
  - Selected: white text, matched terms bold
  - Unselected: dim text, matched terms slightly brighter
- Line 4: blank

**Keybindings:**
- `↑` / `↓` — move selection
- `Enter` — multi-push: pop SearchView, push SessionsView → MessagesView → DetailView for the matched message
- `Esc` — pop (back to previous view)
- `/` — start a new search (replaces current SearchView)

**Data source:** `api.search({ text: query })`.

**Empty state:**
```
  Search: "xyzzy123" (0 results)
─────────────────────────────────────────────────────────────────────────────

  No results found for "xyzzy123"

  Try a different search term, or check spelling.

─────────────────────────────────────────────────────────────────────────────
Esc back  / new search
─────────────────────────────────────────────────────────────────────────────
```

---

## View: StatsView

```
  Stats
─────────────────────────────────────────────────────────────────────────────

  Overview
    Projects      38            Sessions      1,247
    Messages      86,412        DB size       14.2 MB

  Token Usage
    Input         12.4M         Cache read    45.2M
    Output        8.7M          Cache write   3.1M
                                ───────────────────
                                Total         66.3M

  Top Projects
    spaghetti        ████████████████████  1.2M tokens
    voyager          ████████████         890k
    jabali-editor    ██████████           780k
    study-portfolio  █████                420k
    duke-os          ███                  280k

─────────────────────────────────────────────────────────────────────────────
↑↓ scroll  Esc back
─────────────────────────────────────────────────────────────────────────────
```

**Layout:**
- "Overview" section: 2-column key-value grid (label dim, value white)
- "Token Usage" section: 2-column, with divider line and total row
- "Top Projects": project name padded, bar chart `████`, token count right

**Bar chart:** `█` blocks, max width 20 chars at 80 cols. Largest project gets full bar, others proportional.

**Keybindings:**
- `↑` / `↓` — scroll (if content exceeds viewport)
- `Esc` — pop
- `q` — quit

**Data source:** `api.getStats()` + `api.getProjectList()`.

---

## View: MemoryView

```
  spaghetti › Memory
─────────────────────────────────────────────────────────────────────────────

  # Memory Index

  - project_spaghetti_audit_2026-03-20.md
    Full audit results: type gaps, new .claude dirs
    (teams/, backups/), performance bottlenecks,
    and prioritized fix plan

  - project_cli_redesign_direction.md
    CLI redesign: TUI as default, slash commands,
    agent-facing one-offs, tighter core exports
    (modeled after truffle)

─────────────────────────────────────────────────────────────────────────────
↑↓ scroll  Esc back
─────────────────────────────────────────────────────────────────────────────
```

**Header:** `{project} › Memory`

**Content:** Raw MEMORY.md text, indented 2 spaces. Basic markdown rendering:
- `#` headings → bold
- `- ` list items → preserved
- Links → show text only (URL stripped in TUI)
- Code blocks → dim background if possible, otherwise just indented

**Empty state:**
```
  spaghetti › Memory
─────────────────────────────────────────────────────────────────────────────

  No MEMORY.md found for this project.

─────────────────────────────────────────────────────────────────────────────
Esc back
─────────────────────────────────────────────────────────────────────────────
```

**Context:** Requires a project. If at root with no project selected, shows error flash: `"Navigate to a project first"`.

**Data source:** `api.getProjectMemory(slug)`.

---

## View: TodosView

```
  spaghetti › #1 › Todos (5)
─────────────────────────────────────────────────────────────────────────────

  ✓ Read the current browse.ts implementation
  ✓ Design view stack architecture
  ✓ Write RFC for TUI/one-off split
  ○ Implement TUIShell class
  ○ Add slash command parser

─────────────────────────────────────────────────────────────────────────────
↑↓ scroll  Esc back
─────────────────────────────────────────────────────────────────────────────
```

**Header:** `{project} › #{session} › Todos ({count})`

**Item layout:**
- Completed: `✓` green + text (dim, not struck through — struck through is hard to read)
- Pending: `○` white + text (white)
- In progress: `◐` yellow + text (yellow)

**Context:** Requires a session. If at project level with no session selected, uses the latest session. If at root, shows error flash.

**Data source:** `api.getSessionTodos(slug, sessionId)`.

---

## View: PlanView

```
  spaghetti › #1 › Plan
─────────────────────────────────────────────────────────────────────────────

  ## Implementation Plan

  Phase 1: Entry point restructure
    1. Modify bin.ts for TTY detection
    2. Move browse to default entry point
    3. Remove --no-interactive flag

  Phase 2: View stack infrastructure
    1. Create View interface and ViewAction type
    2. Create TUIShell orchestrator
    3. Extract existing views from browse.ts

  Phase 3: Slash commands
    1. Add command input mode to TUIShell
    2. Implement /help (static text)
    3. Implement /search (new view)

─────────────────────────────────────────────────────────────────────────────
↑↓ scroll  Esc back
─────────────────────────────────────────────────────────────────────────────
```

**Header:** `{project} › #{session} › Plan`

**Content:** Plan content rendered as indented text. Same basic markdown rendering as MemoryView.

**Context:** Requires a session. Same fallback logic as TodosView.

**Data source:** `api.getSessionPlan(slug, sessionId)`.

---

## View: SubagentsView

```
  spaghetti › #1 › Subagents (3)
─────────────────────────────────────────────────────────────────────────────

  ▎ Explore  agent-abc123
    "Explore truffle TUI and CLI design"
    47 messages

    Plan  agent-def456
    "Design view stack architecture"
    23 messages

    general-purpose  agent-ghi789
    "Search for slash command implementations"
    89 messages

─────────────────────────────────────────────────────────────────────────────
↑↓ navigate  ⏎ view transcript  Esc back
─────────────────────────────────────────────────────────────────────────────
```

**Header:** `{project} › #{session} › Subagents ({count})`

**Card layout (per subagent):**
- Line 1: `▎ {agentType}  {agentId}`
  - Selected: `▎` magenta, type bold white, ID dim
  - Unselected: space prefix, type magenta, ID dim
- Line 2: `  "{description}"` — italic, dim
- Line 3: `  {messageCount} messages` — dim
- Line 4: blank

**Keybindings:**
- `↑` / `↓` — move selection
- `Enter` — push a MessagesView showing the subagent's transcript
- `Esc` — pop
- `q` — quit

**Context:** Requires a session. Same fallback logic as TodosView.

**Empty state:**
```
  spaghetti › #1 › Subagents (0)
─────────────────────────────────────────────────────────────────────────────

  No subagents in this session.

─────────────────────────────────────────────────────────────────────────────
Esc back
─────────────────────────────────────────────────────────────────────────────
```

**Data source:** `api.getSessionSubagents(slug, sessionId)`.

---

## View: HelpView

```
  Help
─────────────────────────────────────────────────────────────────────────────

  Navigation
    ↑ ↓         Move selection up/down
    Enter       Open / drill into selected item
    Esc         Go back to previous view
    q           Quit spaghetti

  Commands (press / then type)
    /search <query>     Search all messages          /s
    /stats              Usage statistics             /st
    /memory             Project MEMORY.md            /mem
    /todos              Session todo list            /t
    /plan               Session plan                 /pl
    /subagents          Subagent transcripts         /sub
    /export             Export current view           /x
    /help               This help screen             /?

  Message Filters (messages view only)
    1  user     2  claude     3  thinking
    4  tools    5  system     6  internal

─────────────────────────────────────────────────────────────────────────────
Press any key to dismiss
─────────────────────────────────────────────────────────────────────────────
```

**Layout:**
- Three sections: Navigation, Commands, Filters
- Section headers bold
- Key column left-aligned, description right (commands also show alias)
- Footer: "Press any key to dismiss" (not the usual keybinding bar)

**Keybindings:**
- Any key → pop (dismiss)

---

## Command Mode (overlay)

Command mode is a transient overlay. It replaces the footer with an input prompt and shows a live autocomplete suggestion list above it — same pattern as Claude Code's slash command UI.

### Entering command mode

Press `/` at any view. The bottom of the screen transforms:

**Before:**
```
─────────────────────────────────────────────────────────────────────────────
↑↓ navigate  ⏎ open  / command  q quit
─────────────────────────────────────────────────────────────────────────────
```

**After (empty input — all commands shown below the prompt):**
```
─────────────────────────────────────────────────────────────────────────────
❯ /█
─────────────────────────────────────────────────────────────────────────────
  /search <query>       Search all messages
  /stats                Usage statistics
  /memory               Project MEMORY.md
  /todos                Session todo list
  /plan                 Session plan
  /subagents            Subagent transcripts
  /export               Export current view
  /help                 This help screen
```

The suggestion list renders **below** the prompt, outside the main content area. This means the view content above the prompt is never obscured — same pattern as Claude Code.

### Live filtering

As you type, the suggestion list filters to matching commands. Matches are by prefix on the command name:

```
─────────────────────────────────────────────────────────────────────────────
❯ /s█
─────────────────────────────────────────────────────────────────────────────
  /search <query>       Search all messages
  /stats                Usage statistics
  /subagents            Subagent transcripts
```

```
─────────────────────────────────────────────────────────────────────────────
❯ /st█
─────────────────────────────────────────────────────────────────────────────
  /stats                Usage statistics
```

When only one match remains and the input exactly matches, the suggestion list shows just that command with its description.

When no commands match (user is typing arguments or an unknown command), the suggestion list is hidden:

```
─────────────────────────────────────────────────────────────────────────────
❯ /search error handling█
─────────────────────────────────────────────────────────────────────────────
```

### Suggestion list layout

Each suggestion is one line:

```
  /{name}               {description}
```

- Command name: white, left-aligned with consistent padding
- Description: dim, truncated to available width
- Commands with arguments show the argument hint: `/search <query>`
- Selected suggestion (via ↑↓): highlighted with accent background or bold
- Max visible suggestions: 8 (scrollable if more commands are added later)

### Suggestion selection

- `↑` / `↓` — move selection through filtered suggestions
- `Tab` or `Enter` on a selected suggestion — fill the command name into the input
- `Enter` with no suggestion selected — execute whatever is typed
- `Esc` — cancel, return to normal view

Flow example:

```
1. User presses /
2. All commands shown, none selected
3. User types "s" → filtered to /search, /stats, /subagents
4. User presses ↓ → first suggestion (/search) highlighted
5. User presses Tab → input becomes "/search "
6. Suggestion list hides (command resolved, now typing args)
7. User types "error handling"
8. User presses Enter → executes /search "error handling"
```

### Shorthand: bare search

Typing `/ ` (slash, space) then text is treated as `/search {text}`. This makes the most common operation fastest:

```
❯ / error handling█       →  executes as: /search error handling
```

When the input starts with `/ ` (slash + space), the suggestion list hides immediately since the user is entering a search query.

### Alias resolution

Commands can be invoked by their alias. The suggestion list shows the canonical name, but aliases resolve correctly:

| Input | Resolves to |
|-------|-------------|
| `/s` + Enter | Ambiguous — show suggestions |
| `/st` + Enter | `/stats` |
| `/mem` + Enter | `/memory` |
| `/t` + Enter | `/todos` |
| `/pl` + Enter | `/plan` |
| `/sub` + Enter | `/subagents` |
| `/x` + Enter | `/export` |
| `/?` + Enter | `/help` |

If the input uniquely matches a command name or alias, Enter executes it directly without needing to select from suggestions.

### Error flash

Invalid commands (no match for the typed text) show a brief message:

```
─────────────────────────────────────────────────────────────────────────────
  Unknown command: "foo". Type /help for available commands.
─────────────────────────────────────────────────────────────────────────────
```

Dismisses after 2 seconds or on any keypress, then returns to normal view.

### Context errors

Commands that require context show inline errors:

```
─────────────────────────────────────────────────────────────────────────────
  /todos requires a session. Navigate to a project first.
─────────────────────────────────────────────────────────────────────────────
```

Same dismiss behavior as error flash.

---

## Common Chrome

### Footer (all views except HelpView)

Two horizontal rules sandwiching keybinding hints:

```
─────────────────────────────────────────────────────────────────────────────
{keybinding hints}
─────────────────────────────────────────────────────────────────────────────
```

Hint format: `{key} {description}` separated by two spaces. Key in white, description dim.

### Empty states

All list views show a centered message when empty:

```
  {header}
─────────────────────────────────────────────────────────────────────────────

  {description of what's missing}

─────────────────────────────────────────────────────────────────────────────
{relevant keybindings only}
─────────────────────────────────────────────────────────────────────────────
```

### Breadcrumb

All views except ProjectsView show a breadcrumb as the first line:

```
  {project} › {session} › {view name} ({count})
```

- Ancestor segments: dim
- Current segment: accent color
- Count: dim, in parentheses
- Separator: `›` dim

---

## Color Palette

All colors use ANSI 256-color for consistency with the existing browse.ts design.

### Semantic colors

| Purpose | 256 color | Used for |
|---------|-----------|----------|
| User label | 79 (teal) | USER tag, user-related accents |
| User label dim | 36 | Unselected user items |
| Claude label | 216 (peach) | CLAUDE tag, claude-related accents |
| Claude label dim | 173 | Unselected claude items |
| Timestamp | 248 (light gray) | All timestamps |
| User bg | 233 | User message block background |
| User bg selected | 236 | Selected user message block |
| Claude bg | 233 | Claude message block background |
| Claude bg selected | 235 | Selected claude message block |

### Tool category colors (picocolors)

| Category | Color | Tools |
|----------|-------|-------|
| file | cyan | Read, Write, Edit, Glob, NotebookEdit, LSP |
| search | yellow | Grep, ToolSearch, WebSearch |
| shell | red | Bash, KillShell |
| agent | magenta | Agent, SendMessage, Task*, TodoWrite |
| nav | blue | Skill, EnterPlanMode, ExitPlanMode, Enter/ExitWorktree |
| web | green | WebFetch |
| mcp | dim cyan | mcp__* tools |
| other | dim | AskUserQuestion, Cron*, Team* |

### UI chrome colors (picocolors)

| Element | Style |
|---------|-------|
| Welcome panel border | dim |
| Wordmark | bold white |
| Tagline | dim |
| Data path / perf | dim |
| Horizontal rules | dim |
| Breadcrumb ancestors | dim |
| Breadcrumb current | accent (white or yellow) |
| Keybinding keys | white |
| Keybinding descriptions | dim |
| Selected list `▎` marker | category color |
| Filter chip active | category color |
| Filter chip inactive | strikethrough dim |
| Bar chart `████` | cyan |
| Stats labels | dim |
| Stats values | white |
| Error flash | red |
| `❯` prompt | white |

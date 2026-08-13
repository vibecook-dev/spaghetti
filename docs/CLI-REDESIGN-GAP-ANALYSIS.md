> **Historical gap analysis.** References to a synchronous `SpaghettiAPI` or
> TypeScript database service predate RFC 011 and do not describe the current
> production graph. See the
> [Phase 10 closure ledger](./rfcs/011-phase-10-closure.md).

# Spaghetti CLI Redesign — Gap Analysis & Design Direction

**Status**: Draft
**Created**: 2026-03-29
**Reference**: Truffle CLI design at `../../../truffle/docs/cli-design.md`

---

## Part 1: Core/CLI Separation — Gaps

### What truffle gets right

Truffle's `@vibecook/truffle` core exports a **tiny public surface**:

```
index.ts
├── createMeshNode(options)     # Single factory — the "right" entry point
├── resolveSidecarPath()        # One utility
├── Re-exports of types from truffle-native
└── Nothing else
```

A consumer sees 1 factory, 1 utility, and types. Nothing internal leaks.

### Where spaghetti-core leaks internals

Spaghetti's `@vibecook/spaghetti-core` barrel export exposes ~15 internal factories:

```
index.ts  (current)
├── createSpaghettiService()    ✅ The "right" entry point
├── createSpaghettiAppService() ⚠️  Internal — only used by createSpaghettiService()
├── createFileService()         ⚠️  Internal wiring detail
├── createSqliteService()       ⚠️  Internal wiring detail
├── createQueryService()        ⚠️  Internal — needs sqliteFactory
├── createIngestService()       ⚠️  Internal — needs sqliteFactory
├── createSearchIndexer()       ⚠️  Internal
├── createSegmentStore()        ⚠️  Internal
├── AgentDataServiceImpl        ⚠️  Internal — the impl class
├── WorkerPool types            ⚠️  Internal — worker thread internals
├── All segment/summary types   ✅ Needed by consumers
├── SCHEMA_VERSION, initializeSchema  ⚠️  Internal
└── SpaghettiAPI                ✅ The public interface
```

**Impact**: Third-party consumers (or an AI agent using the lib) don't know what to call. The JSDoc on `createSpaghettiService()` is the only hint, but the barrel export suggests everything is equally public.

### Proposed fix (follow truffle's pattern)

```
index.ts  (proposed)
├── createSpaghettiService(options)   # THE entry point
├── SpaghettiAPI                      # The public interface type
├── SpaghettiServiceOptions           # Options for the factory
├── Response types (ProjectListItem, SessionListItem, MessagePage, etc.)
├── Query types (SearchQuery, SearchResultSet, StoreStats, etc.)
├── Domain types (SessionMessage, ToolName, etc.)
└── Nothing else — no internal factories, no worker types, no schema utils
```

Add a separate `@vibecook/spaghetti-core/internals` or `@vibecook/spaghetti-core/advanced` subpath export for power users who genuinely need to wire things manually.

---

## Part 2: TUI vs One-Off Commands — Truffle's Pattern

### Truffle's two modes

| Mode | Entry | Purpose | Audience |
|------|-------|---------|----------|
| **TUI** | `truffle` (bare) | Interactive dashboard, live peer discovery, chat, file transfer | Humans at a terminal |
| **One-off commands** | `truffle ls`, `truffle ping`, `truffle cp --json` | Stateless query/action, exit immediately | Scripts, AI agents, pipes |

**Key design decisions from truffle:**

1. **Bare command = TUI** on a TTY, falls back to `status --json` when piped
2. **Every one-off command has `--json`** for machine-readable output
3. **Daemon bridges both** — TUI and one-off commands talk to the same daemon via JSON-RPC
4. **7 design principles**: reads like English, beautiful by default, zero-config, fail with a fix, show don't tell, names not addresses, one command one job

### Current spaghetti state

Spaghetti currently has:
- `spag` (bare) → dashboard (static, non-interactive)
- `spag browse` → interactive TUI browser (projects → sessions → messages → detail)
- `spag p`, `spag s`, `spag m`, etc. → one-off commands with `--json` support

**Gaps:**

| Truffle pattern | Spaghetti status | Gap |
|---|---|---|
| Bare command = TUI | Bare = static dashboard | `spag` should launch the TUI directly |
| TUI is the primary UX | TUI is a secondary `browse` subcommand | Should be promoted to the default |
| One-off commands for agents | Exists but no explicit agent design | Need to formalize agent-facing API |
| `--json` on everything | Exists on all commands | ✅ Already good |
| Daemon bridges both modes | N/A (spaghetti is read-only, no daemon needed) | Not applicable |

---

## Part 3: Proposed Redesign Direction

### Design principle: Claude Code as inspiration

Spaghetti should feel like Claude Code's own TUI — because it's literally browsing Claude Code data. The interaction model:

1. **`spag` = launch TUI** (interactive, keyboard-driven, Claude Code aesthetic)
2. **Slash commands inside TUI** for features (like Claude Code's `/help`, `/clear`, etc.)
3. **One-off commands** (`spag p`, `spag s --json`, `spag search "error" --json`) for AI agents and scripting

### TUI mode (user-facing)

```
$ spag

  ╔══════════════════════════════════════════════════╗
  ║  Spaghetti v0.2.2                     28ms      ║
  ║  38 projects · 1,247 sessions · 86,412 messages  ║
  ╚══════════════════════════════════════════════════╝

  PROJECTS                          SESSIONS    MESSAGES    LAST ACTIVE
  ● spaghetti                       12          3,841       2 hours ago
  ● voyager                         8           2,104       yesterday
  ● jabali-editor                   6           1,887       3 days ago
  ● study-portfolio                 4           943         1 week ago
  ● duke-os                         3           612         2 weeks ago

  ↑↓ navigate  ⏎ open  / search  ? help  q quit
```

**Slash commands inside TUI:**

| Command | Action |
|---------|--------|
| `/search <query>` or `/` | Full-text search |
| `/stats` | Usage statistics |
| `/memory` | View current project's MEMORY.md |
| `/todos` | View session todos |
| `/plan` | View session plan |
| `/export` | Export current view |
| `/help` | Show available commands |

**Navigation** (keyboard-driven, no mouse):
- `↑`/`↓` or `j`/`k` — navigate list
- `Enter` — drill into selected item
- `Backspace` or `Esc` — go back one level
- `/` — search
- `q` — quit (or go back if not at root)

### One-off commands (agent-facing)

```bash
# All return structured JSON, exit immediately
spag projects --json                    # List all projects
spag sessions spaghetti --json          # Sessions for a project
spag messages . 1 --json                # Messages from latest session
spag search "error handling" --json     # Full-text search
spag stats --json                       # Usage statistics
spag memory spaghetti --json            # MEMORY.md content
spag todos . 1 --json                   # Todos for a session
spag subagents . 1 --json               # Subagent list
```

**Agent design principles:**
- Every command works non-interactively with `--json`
- Exit codes: 0 = success, 1 = error, 2 = not found
- Errors as JSON: `{ "error": "project_not_found", "query": "xyz" }`
- No prompts, no colors, no progress bars when `--json` is used
- Pipe-friendly: `spag messages . 1 --json | jq '.messages[].content'`

### What changes from current design

| Current | Proposed | Why |
|---------|----------|-----|
| `spag` → static dashboard | `spag` → launch TUI | TUI is the primary UX |
| `spag browse` → TUI | Removed as subcommand | Promoted to default |
| `spag p` → one-off | Kept as-is | Agent/script access |
| `spag m` → one-off | Kept as-is | Agent/script access |
| No slash commands | `/search`, `/stats`, `/memory`, etc. | Discoverable features inside TUI |
| Separate `browse.ts` TUI | TUI is the main entry when TTY | Follows truffle's bare-command pattern |

### Detection: TUI vs one-off

```typescript
if (process.argv.length === 2 && process.stdin.isTTY) {
  // Bare `spag` on a terminal → launch TUI
  launchTUI();
} else {
  // Has subcommand or piped → run one-off command
  runCommand();
}
```

---

## Part 4: Implementation Priority

1. **Tighten core exports** — reduce barrel export to public API only
2. **Promote TUI to default** — `spag` bare → TUI, remove `browse` subcommand
3. **Add slash commands to TUI** — start with `/search` and `/help`
4. **Formalize agent output** — ensure all one-off commands have clean `--json`
5. **Apply truffle's 7 principles** — error messages, beautiful defaults, zero-config

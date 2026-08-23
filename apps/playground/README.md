# @vibecook/spaghetti-playground

Private Electron desktop app demonstrating `@vibecook/spaghetti-sdk` end-to-end.

## Requirements

Node 24 (see root `.nvmrc`). `nvm use` before working in this directory.

## Commands

```bash
pnpm -F @vibecook/spaghetti-playground dev     # run in dev (Vite HMR + Electron)
pnpm -F @vibecook/spaghetti-playground build   # produce out/{main,preload,renderer}
pnpm -F @vibecook/spaghetti-playground start   # preview a built bundle
pnpm bench:query-topology                      # measure the real canonical IPC topology
```

The topology benchmark copies the committed fixture to scratch storage, builds
the production Electron entries, and measures canonical queries across
`MessageChannelMain` and the SDK UtilityProcess. Pass
`-- --report-json /tmp/spaghetti-ipc.json` for the complete machine-readable
report; see the
[Phase 10 benchmark record](../../docs/rfcs/011-phase-10-playground-ipc-benchmark.md).

## Where the index lives

SQLite cache (not `~/.spaghetti`):

```text
macOS:  ~/Library/Application Support/@vibecook/spaghetti-playground/cache/spaghetti-rs.db
Windows: %APPDATA%/@vibecook/spaghetti-playground/cache/spaghetti-rs.db
Linux:  ~/.config/@vibecook/spaghetti-playground/cache/spaghetti-rs.db
```

The Rust observation engine is unconditional. A historical `"engine": "ts"`
setting is ignored and cannot restore a TypeScript database owner.

## Operational notes (cache health)

1. **Prefer graceful quit** — close the window or Cmd+Q. The app awaits the
   observation service's `dispose()` so native observation, queries, IPC, and
   SQLite close in order. Durable commits remain crash-safe, but graceful quit
   avoids unnecessary recovery work.
2. **Do not run `PRAGMA integrity_check`** (or other long exclusive SQLite tools) against the live playground DB while it is ingesting.
3. **On persistent `SQLITE_CORRUPT` / “database disk image is malformed”**:
   quit the owner before removing the rebuildable cache, then restart to
   reconcile `~/.claude` / `~/.codex` / `~/.grok`:

```bash
rm -f ~/Library/Application\ Support/@vibecook/spaghetti-playground/cache/spaghetti-rs.db*
```

4. **Single instance** — a second `dev` / `start` focuses the existing window instead of opening a second process on the same cache.

The SDK build uses a Node-API Rust addon rather than a JavaScript SQLite
binding. No Node/Electron `better-sqlite3` ABI swap or rebuild step is needed.
`predev` / `prestart` still build the SDK so Electron never loads stale client
or declaration artifacts.

## Architecture

```text
renderer -> preload -> Electron main broker -> SDK UtilityProcess
                         |                    `- one Rust observation owner
                         |                         `- framed SpaghettiClient ports
                         `-> file-explorer UtilityProcess -> mille MessagePort
```

- **SDK UtilityProcess** unconditionally owns one Rust observation service and
  its SQLite lifecycle. Claude Code is primary; Codex/Grok are auto-detected.
  Progress/ready/change events return through the main broker, and quit awaits
  utility disposal.
- Electron main negotiates `SpaghettiClient` connections over transferred,
  versioned framed `MessagePort`s using the storage-free
  `@vibecook/spaghetti-sdk/client` entry. Product RPC methods delegate to the
  same async Rust-backed service; no main/preload/renderer process opens the
  database.
- **preload** exposes `window.spaghetti` (`src/shared/ipc.ts`).
- **renderer** uses the **archive / paper** design (EB Garamond, ink on
  cream or ink-black paper, light/dark toggle). Multi-source project
  browser with `sourceId` badges, full-text search (`⌘K`), live session
  tails, Structure panel (mille files + session artifacts), and
  ProjectPage-style message filters.

## Renderer features

| Surface | How |
|---|---|
| Library startup | Catalog projects and sessions render before transcript decoding; catalog-only sessions stay visibly non-readable until decoded rows replace them |
| Readiness | One indicator follows catalog, history, usage, capabilities, artifacts, and search; degraded/unavailable fields remain explicit |
| Search | `⌘K` / header **Search** → `api.search` overlay; Enter opens project/session |
| Live chat | `onChange` → append new messages at the tail; “N new” pill when scrolled up |
| Message filters | Session bar: type/tool solo (pin) + mute (eye) + text filter (ProjectPage parity) |
| Artifacts | Session **Artifacts** drawer: plan, todos, task, subagents, MEMORY.md |
| Files panel | `⌘B` / **Files** — rightmost [@vibecook/mille](https://www.npmjs.com/package/@vibecook/mille) tree for the selected project's `absolutePath` |
| Source filter | Project list chips when multiple agents are indexed |
| Stats | Header: segment count, DB size, FTS index size |

### Mille file explorer

Native file tree runs in an Electron **UtilityProcess** (`src/utility/fx-host.ts`)
so the NAPI `.node` binary never loads in the renderer. Main forks the host
with `WORKSPACE_ROOT`, waits for `ready`, then transfers a `MessagePort`.
Opening the panel against a project path (or switching projects) restarts
the host for that folder.

UI theme: **`minimal`** (`@vibecook/mille-ui/icons/minimal` +
`@vibecook/mille-ui/theme/minimal.css`) under `data-mille-theme="minimal"`,
matching the archive Structure panel. Packages are linked from the local
`mille` monorepo (`file:../../../../mille/packages/...`).

## Notes

The published `<AgentDataPlayground />` and live hooks now accept the same
asynchronous React client used over IPC and suppress stale Promise results. The
app retains its custom archive shell in `App.tsx` for product-specific layout,
not because of a synchronous API limitation.

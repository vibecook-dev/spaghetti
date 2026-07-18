# @vibecook/spaghetti-playground

Private Electron desktop app demonstrating `@vibecook/spaghetti-sdk` end-to-end.

## Requirements

Node 24 (see root `.nvmrc`). `nvm use` before working in this directory.

## Commands

```bash
pnpm -F @vibecook/spaghetti-playground dev     # run in dev (Vite HMR + Electron)
pnpm -F @vibecook/spaghetti-playground build   # produce out/{main,preload,renderer}
pnpm -F @vibecook/spaghetti-playground start   # preview a built bundle
```

## Where the index lives

SQLite cache (not `~/.spaghetti`):

```text
macOS:  ~/Library/Application Support/@vibecook/spaghetti-playground/cache/spaghetti-{rs,ts}.db
Windows: %APPDATA%/@vibecook/spaghetti-playground/cache/spaghetti-{rs,ts}.db
Linux:  ~/.config/@vibecook/spaghetti-playground/cache/spaghetti-{rs,ts}.db
```

Engine preference: `<userData>/settings.json` (`"engine": "rs" | "ts"`, default `rs`).

## Operational notes (cache health)

1. **Prefer graceful quit** — close the window or Cmd+Q. The app awaits `api.dispose()` so live pipelines drain and SQLite closes cleanly. Avoid `kill -9` during first cold ingest; native bulk can leave a half-written cache if terminated mid-write.
2. **Do not run `PRAGMA integrity_check`** (or other long exclusive SQLite tools) against the live playground DB while it is ingesting.
3. **On `SQLITE_CORRUPT` / “database disk image is malformed”**: the SDK
   **self-recovers** once — it deletes the shared cache and re-ingests from
   `~/.claude` / `~/.codex` / `~/.grok`. The loading screen shows
   `SQLite cache malformed — deleting and re-ingesting from disk…`.

   Manual wipe (if recovery still fails, or you want a clean slate):

```bash
# Native engine cache
rm -f ~/Library/Application\ Support/@vibecook/spaghetti-playground/cache/spaghetti-rs.db*

# Or TypeScript engine cache
rm -f ~/Library/Application\ Support/@vibecook/spaghetti-playground/cache/spaghetti-ts.db*

# Optional: force engine in settings.json
# { "engine": "rs" }
```

Preflight: `PRAGMA quick_check` before multi-source ingest also auto-wipes a
corrupt file before exclusive native starts.

4. **Single instance** — a second `dev` / `start` focuses the existing window instead of opening a second process on the same cache.

## Native module (`better-sqlite3`) ABI note

`better-sqlite3` is a native module and only has **one** copy in the pnpm
store, shared between `packages/sdk` (tested under Node) and this app (runs
under Electron). Node and Electron use different V8 ABIs, so the binary
can't satisfy both at once.

The workflow:

| Step | ABI | Who |
|---|---|---|
| `pnpm install` | **Node** (prebuild) | monorepo default |
| `pnpm -F @vibecook/spaghetti-playground dev` | builds **SDK dist**, then **Electron** ABI for better-sqlite3 | playground |
| After playground, before SDK tests / CLI | rebuild for **Node** | you |

`predev` / `prestart` run `pnpm -F @vibecook/spaghetti-sdk build` so Electron always
loads recovery and other SDK fixes from `packages/sdk/dist` (not a stale build).

```bash
# After running the playground, restore Node ABI for tests / spag CLI:
pnpm -F @vibecook/spaghetti-playground rebuild:node
# or from repo root:
pnpm rebuild better-sqlite3
```

Symptom if you forget: Node fails with
`NODE_MODULE_VERSION 145` vs `137` (Electron vs Node) when opening better-sqlite3.

Requires Xcode CLT (macOS) / Python 3 / node-gyp prerequisites to compile
from source if the prebuild isn't available.

## Architecture

```
┌──────────────┐   ipcMain.handle   ┌──────────────────────────────────┐
│   renderer   │ ─────────────────▶ │              main                 │
│  React 19    │  window.spaghetti  │   SpaghettiService               │
│  SDK /react  │ ◀───── events ──── │   sources: ~/.claude (+ codex/   │
│  mille-ui    │  window.mille      │   db: <userData>/cache           │
└──────────────┘                    │   live: true → safeBulk bulk    │
        ▲           contextBridge   │                                  │
        │                           │   utilityProcess → fx-host       │
        │ MessagePort (fx-port)     │     @vibecook/mille FileExplorer │
        └─ preload (typed) ─────────┴──────────────────────────────────┘
```

- **main** owns a single `SpaghettiService` with `live: true` (and thus
  crash-safer bulk SQLite settings). Primary source is Claude Code;
  Codex / Grok are auto-detected. Progress/ready/change events go to all
  renderer windows. Quit uses `dispose()`.
- **preload** exposes `window.spaghetti` (`src/shared/ipc.ts`).
- **renderer** uses the **archive / paper** design (EB Garamond, ink on
  cream or ink-black paper, light/dark toggle). Multi-source project
  browser with `sourceId` badges, full-text search (`⌘K`), live session
  tails, Structure panel (mille files + session artifacts), and
  ProjectPage-style message filters.

## Renderer features

| Surface | How |
|---|---|
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

## Notes

The renderer does **not** mount `<AgentDataPlayground />` directly — that
component assumes synchronous SDK calls, but over IPC every call is a
Promise. The shell in `App.tsx` is a read-only project/session browser.

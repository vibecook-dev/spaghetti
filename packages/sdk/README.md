# @vibecook/spaghetti-sdk

Local-first SDK for multi-agent history — index Claude Code, Codex, Grok (and more) into SQLite, query sessions/messages/artifacts, and run full-text search.

Part of [Spaghetti](https://github.com/vibecook-dev/spaghetti).

[![npm](https://img.shields.io/npm/v/@vibecook/spaghetti-sdk.svg)](https://www.npmjs.com/package/@vibecook/spaghetti-sdk)
[![node](https://img.shields.io/badge/node-%3E%3D18-brightgreen.svg)](https://nodejs.org/)

## Install

```bash
npm install @vibecook/spaghetti-sdk
# or
pnpm add @vibecook/spaghetti-sdk
```

The SDK depends on `@vibecook/spaghetti-sdk-native` (Rust ingest core). A single platform-specific prebuilt binary (~4 MB) is pulled in via napi-rs `optionalDependencies`, so `npm install` just works on macOS (x64/arm64) and Linux (x64/arm64-gnu). On any platform without a prebuild the SDK falls back to the pure-TypeScript ingest path transparently — no configuration required.

React components are shipped under the `/react` subpath and require React 19 (peer).

## Core API

```ts
import { createSpaghettiService } from '@vibecook/spaghetti-sdk';

// Defaults: native Rust engine, ~/.claude primary source, ~/.spaghetti/cache/spaghetti-rs.db index.
const spaghetti = createSpaghettiService();
await spaghetti.initialize();

const projects = spaghetti.getProjectList();
const sessions = spaghetti.getSessionList(projects[0].slug);
const messages = spaghetti.getSessionMessages(projects[0].slug, sessions[0].sessionId);
const results = spaghetti.search({ text: 'worker thread' });

spaghetti.shutdown();
```

### Options

`createSpaghettiService(options?)` accepts:

| Field | Type | Default | Description |
|---|---|---|---|
| `rootDir` | `string` | `~/.claude` | Primary agent data root when using the default Claude Code source. |
| `claudeDir` | `string` | — | **Deprecated.** Alias for `rootDir`. |
| `source` | `AgentSource` | Claude Code | Explicit primary agent adapter. |
| `additionalSources` | `AgentSource[]` | `[]` | Extra agents (e.g. Codex, Grok) into the same index. |
| `dbPath` | `string` | `~/.spaghetti/cache/spaghetti-{rs,ts}.db` | SQLite index path. Default varies by engine so switching engines doesn't force a re-ingest. |
| `engine` | `'rs' \| 'ts'` | resolved via [Engine selection](#engine-selection) | Pin the ingest engine for this service. Takes precedence over the process-wide `SPAG_ENGINE` env var and the persisted `~/.spaghetti/config.json` setting. |
| `live` | `boolean` | `false` | Enable Plane 2 live disk ingest. |
| `dataService` | `AgentDataService` | — | Inject a custom/mock implementation (testing). |

```ts
// Pin the engine for this service without mutating global state.
const svc = createSpaghettiService({ engine: 'rs', dbPath: '/tmp/my-index.db' });
```

Two instances in the same process can point at different `rootDir`s as long as each has its own `dbPath` — same DB file from two services risks `SQLITE_BUSY`.

### Key methods

- `getProjectList()`
- `getSessionList(projectSlug)`
- `getSessionMessages(projectSlug, sessionId, limit?, offset?)`
- `getProjectMemory(projectSlug)`
- `getSessionTodos(projectSlug, sessionId)`
- `getSessionPlan(projectSlug, sessionId)`
- `getSessionSubagents(projectSlug, sessionId)`
- `getSubagentTimeline(projectSlug, sessionId, agentId, request)`
- `search(query)`
- `getStats()`
- `rebuildIndex()`
- `onProgress()` / `onReady()` / `onChange()`

## React components

```tsx
import { SpaghettiProvider, AgentDataPlayground } from '@vibecook/spaghetti-sdk/react';

export default function App() {
  return (
    <SpaghettiProvider api={api}>
      <AgentDataPlayground />
    </SpaghettiProvider>
  );
}
```

Exports include `SpaghettiProvider`, `useSpaghettiAPI`, `AgentDataPlayground`, `ProjectCard`, `SessionCard`, `MessageEntry`, `DetailOverlay`, `MetaRow`, `Badge`, and formatter utilities (`formatTokenCount`, `formatRelativeTime`, `formatDuration`, `formatBytes`).

## Data flow

At init, the service:

- discovers project/session files under `~/.claude`
- parses projects in streaming mode (Rust with rayon parallelism on the default `rs` engine; TS worker threads with sequential fallback on the `ts` engine)
- writes normalized rows into a SQLite database at `~/.spaghetti/cache/spaghetti-{rs,ts}.db` (per-engine by default; overridable via `dbPath`)
- builds and maintains FTS5 search indexes — the native path drops the auto-sync triggers during bulk ingest and rebuilds the index in one pass at finalize
- tracks file fingerprints so warm starts skip unchanged work (a no-change warm start returns in ~120 ms even against 1 GB+ of source data)

Query and ingest share one SQLite connection to avoid `SQLITE_BUSY` conflicts.

## Architecture (three planes)

Composition follows a stable internal shape (see monorepo `docs/THREE-PLANE-INGEST-ARCHITECTURE.md`):

| Piece | Role |
|---|---|
| `AgentSource` / `createClaudeCodeSource()` | Agent product roots (`~/.claude`, `~/.spaghetti`) |
| Static ingest | Cold/warm full parse into SQLite |
| Live disk (`{ live: true }` → `api.live`) | Watcher + incremental writes + `Change` events |
| Runtime (`api.runtime`) | Hooks stream + channel session discovery |
| Durable store | Shared SQLite query + ingest + data store |

```ts
import { createClaudeCodeSource, createSpaghettiService } from '@vibecook/spaghetti-sdk';

const source = createClaudeCodeSource({ rootDir: '/custom/.claude' });
const svc = createSpaghettiService({ source, live: true });
await svc.initialize();

// Plane 2 — disk deltas (only when live: true)
svc.live?.onChange({ type: 'session', slug: '…', sessionId: '…' }, (e) => {
  console.log('disk change', e.type);
});

// Plane 3 — hooks / channel sessions (always on default factory)
svc.runtime?.onEvent((e) => {
  if (e.type === 'hook') console.log(e.name, e.sessionId);
});
```

`claudeDir` remains supported and seeds the default Claude Code source.  
Long-lived surfaces (CLI TUI, Electron playground) pass `{ live: true }` by default; one-shot CLI commands stay pull-only.

## Engine selection

The SDK ships two ingest engines:

- **`rs` (default)** — native Rust addon (`@vibecook/spaghetti-sdk-native`), runs via napi-rs. Roughly 2× faster cold start than the TS path on a 1 GB+ `~/.claude` after the RFC 004 writer tuning.
- **`ts`** — pure-TypeScript path. Kept as ground truth for the diff harness and used automatically when the native binary is unavailable for the host platform.

Resolution order (first match wins):

1. Per-service `createSpaghettiService({ engine })`
2. `SPAG_ENGINE=ts|rs` env var
3. Legacy `SPAG_NATIVE_INGEST=0|1` env var (`0` → ts, `1` → rs)
4. Persisted `engine` field in `~/.spaghetti/config.json`
5. Default: `rs`

Correctness parity between the two engines is enforced by `pnpm test:ingest-diff` (small fixture) and `pnpm test:ingest-diff:medium` (exercises every rare `SessionMessage` / content-block variant) in CI.

## Native dependency

Two native modules are in play:

- **`better-sqlite3`** — the underlying SQLite driver used by both engines. Prebuilds available for Node 18+ on common platforms; falls back to `node-gyp` source build if no prebuild matches.
- **`@vibecook/spaghetti-sdk-native`** — the Rust ingest addon. napi-rs publishes per-platform binaries as `optionalDependencies` (darwin-{x64,arm64}, linux-{x64,arm64}-gnu). Any platform without a prebuild loads the TS fallback silently.

## Migration from `@vibecook/spaghetti-core`

`@vibecook/spaghetti-sdk` replaces the deprecated `@vibecook/spaghetti-core` and the private `@vibecook/spaghetti-ui`.

```diff
- import { createSpaghettiService } from '@vibecook/spaghetti-core';
+ import { createSpaghettiService } from '@vibecook/spaghetti-sdk';

- import { SpaghettiProvider } from '@vibecook/spaghetti-ui';
+ import { SpaghettiProvider } from '@vibecook/spaghetti-sdk/react';
```

The public API is unchanged.

## License

[MIT](https://github.com/vibecook-dev/spaghetti/blob/main/LICENSE) — James Yong

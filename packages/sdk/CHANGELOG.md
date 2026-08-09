# @vibecook/spaghetti-core

## Removed in 0.6.0

The runtime surface is gone. There is no replacement for hook or channel
streaming; transcript ingest, query, and live updates are unaffected.

- `SpaghettiAPI.runtime`, `SpaghettiRuntime`, `createSpaghettiRuntime`
- `createRuntimeBridge`, `RuntimeBridge`, `CreateRuntimeBridgeOptions`
- `RuntimeEvent` and its three guards
- `createHookEventWatcher`, `getDefaultHookEventsPath`, and their types
- `createChannelRegistry`, `createChannelClient`, `createChannelManager`, and their types
- the `types/spaghetti/` hook and channel wire types, plus the
  `types/hook-events.js` and `types/channel-messages.js` shims
- `AgentSourcePaths.hookEventsFile`, `.channelSessionsDir`, `.channelMessagesDir`
- `ws` and `@types/ws`
- `buildClaudeCodePaths` / `buildCodexPaths` / `buildGrokPaths` lost their now-unused
  `stateDir` parameter

**Retained:** `listActiveSessionsFromDir`, `isProcessAlive`,
`ListActiveSessionsOptions`, `ActiveSessionFile`, and
`AgentSourcePaths.sessionsDir`. They moved from `planes/` to
`sources/claude-code/`; the public export names are unchanged.

```ts
// Before
const sessions = api.runtime?.listActiveSessions({ requireAlive: true }) ?? [];

// After
const source = createClaudeCodeSource();
const sessions = listActiveSessionsFromDir(source.paths.sessionsDir, { requireAlive: true });
```

See [RFC 007](../../docs/rfcs/007-retire-runtime-bridge.md).

## 0.4.0

### Minor Changes

- Support newer Claude Code data needed by the updated CLI surfaces:
  - active channel/session metadata
  - richer message variants and envelope fields
  - validator coverage aligned with current real-world Claude data

### Patch Changes

- Validation and test pipeline improvements for current data formats

## 0.3.0

## 0.2.2

## 0.2.1

### Patch Changes

- [`a2944a1`](https://github.com/jamesyong-42/spaghetti/commit/a2944a18261f9e40426a51a850839e4cdd57053d) Thanks [@jamesyong-42](https://github.com/jamesyong-42)! - Truffle-style update command, cross-platform fixes, eslint + prettier
  - `spaghetti update` command — checks npm registry and installs latest version
  - Background update check notifies on startup (24h interval, non-blocking)
  - Windows cross-platform compatibility (path.sep, CRLF, pager fallback)
  - ESLint + Prettier configured with CI integration
  - Cross-platform CI matrix (ubuntu, macOS, Windows)

## 0.2.0

### Minor Changes

- Initial public release of Spaghetti CLI and core library.
  - 10 CLI commands: projects, sessions, messages, search, stats, memory, todos, subagents, plan, export
  - Architecture C: dedicated SQLite tables, persistent FTS5, streaming parser, worker threads
  - Auto-update, cross-platform (macOS, Linux, Windows)
  - Data recovery from legacy databases

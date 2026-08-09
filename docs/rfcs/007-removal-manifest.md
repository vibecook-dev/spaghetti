# RFC 007 Removal Manifest

**Companion to:** [RFC 007 — Retire the Runtime Bridge](./007-retire-runtime-bridge.md)
**Created:** 2026-08-08 · **Executed:** 2026-08-08 (single release, RFC v3)
**Baseline commit:** `211f4b1` — the last commit containing Plane 3

This is the record of what was removed and what deliberately survived. It stays
in the repo as the answer to "where did hooks/chat/plugins go?" and as the
allow-list for any future grep that expects Plane 3 to be gone.

| Category | Meaning |
| --- | --- |
| `removed` | Deleted outright |
| `retain-diagnostic` | Survives so leftovers in Claude Code stay diagnosable |
| `retain-history` | Dated records kept as written, with a supersession note |
| `not-plane-3` | Matches a naive search but is unrelated — do not delete |

---

## 1. Removed

### 1.1 SDK modules

| Path | Was |
| --- | --- |
| `packages/sdk/src/planes/runtime-bridge.ts` | bridge factory |
| `packages/sdk/src/runtime/` | `api.runtime` facade |
| `packages/sdk/src/events/` | runtime event union + guards |
| `packages/sdk/src/io/hook-event-watcher.ts` | hook JSONL tail |
| `packages/sdk/src/io/channel-registry.ts` | channel discovery watcher |
| `packages/sdk/src/io/channel-client.ts` | websocket channel client (sole `ws` importer) |
| `packages/sdk/src/io/channel-manager.ts` | client-fleet manager |
| `packages/sdk/src/types/spaghetti/` | hook + channel wire types |
| `packages/sdk/src/types/hook-events.ts` | re-export shim |
| `packages/sdk/src/types/channel-messages.ts` | re-export shim |
| `packages/sdk/src/planes/__tests__/runtime-bridge.test.ts` | bridge tests |

### 1.2 SDK edits

| Path | Removed |
| --- | --- |
| `api.ts` | `SpaghettiRuntime` import; the `runtime` field; "runtime pipelines" in the `close()` doc |
| `create.ts` | bridge import, construction, and the third `createSpaghettiAppService` argument |
| `app-service.ts` | runtime imports, the `runtimeBridge` field, the `runtime` field, the constructor parameter, `runtimeBridge?.stop()`, and the factory parameter |
| `index.ts` | bridge / event / runtime exports |
| `planes/index.ts` | `createRuntimeBridge` export |
| `io/index.ts` | hook-watcher and channel exports |
| `types/index.ts` | `export * from './spaghetti/index.js'` |
| `sources/types.ts` | `hookEventsFile`, `channelSessionsDir`, `channelMessagesDir`. **`sessionsDir` retained** |
| `sources/{claude-code,codex,grok}/paths.ts` | the same three fields, and the now-unused `stateDir` parameter on each `build*Paths` |
| `sources/__tests__/claude-code-source.test.ts` | hook/channel path assertions and the bridge test |
| `package.json` | `ws`, `@types/ws` |

### 1.3 CLI modules

| Path | Was |
| --- | --- |
| `packages/cli/src/commands/hooks.ts` | `spag hooks` |
| `packages/cli/src/commands/chat.ts` | `spag chat` |
| `packages/cli/src/commands/plugin.ts` | `spag plugin` |
| `packages/cli/src/lib/plugins.ts` | superseded by `lib/plugin-leftovers.ts` |
| `packages/cli/src/views/hooks-monitor-view.tsx` | hooks monitor TUI view |
| `packages/cli/src/views/chat-view.tsx` | chat TUI view |

### 1.4 CLI edits

| Path | Removed |
| --- | --- |
| `index.ts` | hooks/chat/plugin imports, known-command rows, and `Command` registrations |
| `views/types.ts` | `'hooks-monitor'` and `'chat'` `ViewType` discriminants |
| `views/menu-view.tsx` | Hooks Monitor and Chat entries, their push targets, and the re-indexed keyboard handler |
| `lib/doctor-report.ts` | `HookEventsReport` / `ChannelSessionsReport` and their collectors; the bridge in `collectIndexLive`, now `listActiveSessionsFromDir` |
| `commands/doctor.ts`, `views/doctor-view.tsx` | Hook events and Channel sessions sections |
| `package.json` | `ws`, `@types/ws` |

### 1.5 Packages and manifests

| Path | Note |
| --- | --- |
| `packages/claude-code-hooks-plugin/` | whole package |
| `packages/claude-code-channels-plugin/` | whole package |
| `.claude-plugin/marketplace.json` | file and directory deleted — nothing else used it |
| `pnpm-lock.yaml` | regenerated; no importer for either package |

### 1.6 Docs and site

| Path | Change |
| --- | --- |
| `docs/THREE-PLANE-INGEST-ARCHITECTURE.md` | content moved to `TWO-PLANE-INGEST-ARCHITECTURE.md`; old path is now a supersession pointer |
| `site/api.html` | Runtime section and its sidebar link |
| `site/commands.html` | `spag hooks`, `spag chat`, `spag plugin` articles; meta description |
| `site/index.html` | "Three planes" → "Two planes"; Hooks Monitor menu line |
| `packages/cli/README.md` | command rows, TUI tree entries, examples; replaced by a short "Removed" section with the manual commands |
| `packages/sdk/README.md` | runtime plane row and usage example |
| `docs/coverage/claude-code.md`, `scripts/coverage/claude_code/claim.json` | active sessions now cite `listActiveSessionsFromDir` |
| `docs/PARSER-CLASS-DIAGRAM.md` | plugin/module omission note |

### 1.7 Never shipped

Built during the staged plan, deleted when RFC v3 dropped the deprecation
window. Recorded so nobody looks for them:

`lib/deprecation.ts`, `lib/plugin-cleanup.ts` (planner + capability gate +
executor), `lib/retirement-banner.ts`, `spag plugin uninstall --yes`, the SDK
`@deprecated` annotations, `docs/MIGRATION-plane-3.md`,
`docs/rfcs/007-release-evidence.md`, and their tests
(`plugin-cleanup.test.ts`, `plugin-uninstall-matrix.test.ts`,
`retirement-banner.test.ts`, `claude-cli-capabilities.test.ts`).

---

## 2. `retain-diagnostic`

| Path | Retains |
| --- | --- |
| `packages/cli/src/lib/plugin-leftovers.ts` | the read-only probe; the identities `spaghetti-hooks@spaghetti`, `spaghetti-channel@spaghetti`, marketplace `spaghetti`, `CANONICAL_REPO = vibecook-dev/spaghetti` |
| `packages/cli/src/__tests__/plugin-leftovers.test.ts` | tri-state, scope, source-normalisation, degraded-input coverage |
| `packages/cli/src/__tests__/fixtures/claude-cli/` | captured `claude plugin list --json` / `marketplace list --json` fixtures for the fallback path |
| `packages/cli/src/lib/doctor-report.ts` | `leftoverLines`, `leftoverManualCommands` |
| `packages/cli/src/commands/doctor.ts`, `views/doctor-view.tsx` | the leftover section; manual commands are raw `claude …`, never `spag plugin` |
| `packages/cli/src/commands/uninstall.ts` | plugin cleanup before npm removal; cache paths distinguished from a full purge |

Keep the doctor section for at least one additional minor release.

---

## 3. `not-plane-3`

Matches a naive search. Do not delete:

| Path | Why it matches |
| --- | --- |
| `packages/cli/src/views/hooks.ts` | React list-navigation + terminal-size helpers |
| `apps/playground/src/utility/sdk-runtime.ts` | the playground's `SdkRuntime` host class — verified: no bridge, no channel import |
| `packages/sdk/src/react/chat/` | SDK React transcript components |
| `packages/sdk/src/react/live/use-live-session-messages.ts` | RFC 005 live hook whose doc comment says "chat-view" |
| `packages/sdk/src/sources/claude-code/active-sessions.ts` | relocated in Phase 0; keeps `listActiveSessionsFromDir`, `isProcessAlive`, `ListActiveSessionsOptions` |
| `packages/sdk/src/types/claude/toplevel-files-data.ts` | `ActiveSessionFile` |
| `packages/sdk/src/sources/types.ts` | `sessionsDir` |
| `packages/sdk/src/{native,workers/worker-pool,live/watcher}.ts` | incidental "runtime" wording |
| `ws@8.20.0` in the lockfile | transitive via `ink` — not owned by the SDK or CLI |

---

## 4. `retain-history`

| Path | Treatment |
| --- | --- |
| `docs/THREE-PLANE-INGEST-ARCHITECTURE.md` | supersession pointer at the old path so dated links resolve |
| `docs/PR-PLAN-THREE-PLANE-SHAPE.md` | supersession note; PR6 and its Plane 3 references are historical |
| `docs/rfcs/006-normalized-message-model.md` | companion link repointed |
| `CHANGELOG.md`, `packages/*/CHANGELOG.md` | prior entries retained; removal entry added |

---

## 5. Verification

| Gate | Result |
| --- | --- |
| `pnpm build` / `typecheck` / `lint` / `format:check` | pass |
| CLI suite | 26 tests, all pass |
| Doctor renders without removed sections, still reports active indexing | pass |
| Removed commands give the normal unknown-command response | pass |
| `pnpm why ws` / `@types/ws` — not owned by SDK or CLI | pass (transitive via `ink` only) |
| Lockfile has no importer for either plugin package | pass |
| Built SDK declarations contain no Plane 3 symbol | pass |
| No production import of a deleted module | pass |

The SDK suite is red for an unrelated toolchain reason — Node v26.6.0 has no
`better-sqlite3@12.9.0` prebuild and the `node-gyp` fallback fails on MSVC
(`LNK1117`). 37 tests fail with `ERR_DLOPEN_FAILED`, every stack through
`SqliteServiceImpl`, none touching Plane 3. It predates this work.

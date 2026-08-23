# @vibecook/spaghetti-sdk

Local-first, async SDK for the Rust-owned Spaghetti observation and query
engine. One engine process can observe Claude Code, Codex, and Grok roots,
materialize their canonical history into SQLite, serve queries, and replay
durable changes.

Part of [Spaghetti](https://github.com/vibecook-dev/spaghetti).

## Install

```bash
npm install @vibecook/spaghetti-sdk
```

The SDK requires the platform-specific `@vibecook/spaghetti-sdk-native`
prebuild. RFC 011 deliberately has no production TypeScript ingest fallback:
if the native package cannot load, initialization fails with platform and
installation details.

## Product API

```ts
import { homedir } from 'node:os';
import { join } from 'node:path';
import { createObservationService } from '@vibecook/spaghetti-sdk';

const service = createObservationService({
  dbPath: join(homedir(), '.spaghetti/cache/spaghetti-rs.db'),
  sources: [
    { adapterId: 'claude-code', roots: [join(homedir(), '.claude')] },
    { adapterId: 'codex', roots: [join(homedir(), '.codex')] },
    { adapterId: 'grok', roots: [join(homedir(), '.grok')] },
  ],
  live: true,
});

await service.initialize();

const projects = await service.getProjectList();
const project = projects[0];
const sourceId = project.members[0].sourceId;
const sessions = await service.getSessionList(project, { sourceId });
const messages = await service.getSessionMessages(
  sessions[0].projectSlug,
  sessions[0].sessionId,
  50,
  0,
  { sourceId },
);
const results = await service.search({ text: 'worker thread', sourceId });

await service.dispose();
```

Every read is asynchronous and crosses the canonical `SpaghettiClient`
contract. The compatibility DTO mapping is source-neutral; applications never
parse Claude, Codex, or Grok envelopes.

### Options

| Field | Type | Required | Description |
| --- | --- | --- | --- |
| `dbPath` | `string` | yes | File-backed SQLite database owned exclusively by this service. |
| `sources` | `{ adapterId: string; roots: string[] }[]` | yes | One entry per compiled adapter. Duplicate adapter IDs are rejected. |
| `queryWorkers` | `number` | no | Persistent read-only query workers; native default is 2. |
| `ownerLabel` | `string` | no | Diagnostic label recorded in owner metadata. |
| `live` | `boolean` | no | Subscribe to durable projection changes; defaults to `true`. |
| `signal` | `AbortSignal` | no | Cancels startup while still cleaning up partial ownership. |

The product surface includes projects, sessions, messages, timeline/facets,
memory, tasks, plans, tool results, subagents, workflows, search, usage, stats,
teams, lifecycle events, rebuild, snapshots, and IPC serving.

Only one observation host may own a database. A competing host fails with the
current owner's metadata; `dispose()` releases watchers, query workers, IPC
hosts, the writer, and the database lock.

## Canonical client API

Use the lower-level host when a tool wants canonical Rust DTOs rather than the
product compatibility facade:

```ts
import { openObservationHost } from '@vibecook/spaghetti-sdk/observation';

const host = await openObservationHost({ dbPath, sources });
const sourcePage = await host.client.listSources();
const projectPage = await host.client.listProjects();
const health = await host.client.getHealth();
await host.dispose();
```

`@vibecook/spaghetti-sdk/client` contains transport-neutral N-API and framed
IPC clients. Electron main/render processes can use a framed client while one
utility process remains the sole native owner. Durable subscriptions wait on
the Rust writer's commit signal and replay from `(commitSeq, ordinal)`; use
`getSubscriptionMetrics()` for local delivery/lag diagnostics.

## Watching one session (`observeSession`)

`observeSession` attaches to a single session tree and yields typed events. It
opens no database, enumerates no unrelated sessions, and holds one batch in
memory at a time, so a slow consumer applies backpressure to the native queue
instead of growing an array.

```ts
import { observeSession, isSemanticEvent } from '@vibecook/spaghetti-sdk';

const observer = observeSession({
  adapter_id: 'claude-code',
  agent_root: `${homedir()}/.claude`,
  transcript_path: transcriptPath, // may not exist yet
});

try {
  for await (const event of observer) {
    switch (event.type) {
      case 'bootstrap_complete':
      case 'resync_complete':
        commitStagedEpoch(event.scope_epoch); // the barrier carries full coverage
        break;
      case 'overflow':
        discardEpoch(event.scope_epoch); // a replacement snapshot follows
        break;
      case 'reset':
        reloadObject(event.source.object_path);
        break;
      case 'source_error':
        if (event.terminal) warn(event.message);
        break;
      case 'closed':
        break;
      default:
        if (isSemanticEvent(event)) apply(event);
    }
  }
} finally {
  await observer.close();
}
```

Leaving the loop — `break`, `return`, or a thrown error — closes the
attachment; the `finally` above only matters if you also want `close()` to have
resolved before continuing. Iteration is single-consumer: the observer returns
the same iterator every time, because a second one would take events from the
first rather than receive its own copy.

A live session has no natural end, so `for await` parks on the next event
rather than returning. Call `close()` from elsewhere — a signal handler, a
session switch — to end the iteration; the loop then drains what is already
queued and stops. When you already hold an `AbortSignal`, pass it instead:

```ts
const observer = observeSession(request, { signal: controller.signal });
for await (const event of observer) apply(event);
// Aborting ended the loop cleanly. Propagate it only if you want to:
controller.signal.throwIfAborted();
```

Aborting is a clean stop, not an error: queued events and the final `closed`
event are still delivered and the loop returns rather than throwing, so a
consumer applying events never loses the ones it already has to a rejection it
did not ask for.

### Replacing `watchSessionTranscript`

`watchSessionTranscript` still works and still ships, but it is superseded and
will be removed one release after downstream consumers migrate. The differences
that matter when porting:

| `watchSessionTranscript` | `observeSession` |
| --- | --- |
| raw transcript lines from one file | reduced RFC 012C revisions from the whole session tree, subagent transcripts and declared sidecars included |
| callbacks | `for await` |
| a rewritten file is silently re-read | `reset` names the object and its new generation |
| no continuity claim | `scope_epoch` plus `overflow` → full replacement snapshot |
| no identity | `semantic_revision_ref` on every semantic event, equal to what a durable query returns for the same revision |

Deduplicate on `(scope_epoch, event_id)`; `sequence` is the delivery order
inside one attachment and is not comparable across attachments or to a durable
commit sequence.

### Typing the payload

Every event type is generated from Rust — regenerate with `pnpm generate:types`
rather than editing anything under `src/generated/`. `SemanticEvent.value` is
still `unknown`: the per-family value types come from `adapter/facts.rs` in
Wave 2. Until then, narrow on `family` and validate the value at your own
boundary.

```ts
if (isSemanticEvent(event) && event.family === 'usage_v2') {
  const usage = event.value as MyUsageShape; // your own contract, for now
}
```

## Architecture and compatibility

- Rust owns discovery, bounded reads, decoding, projection transactions,
  SQLite, queries, watchers, retries, and durable change retention.
- TypeScript owns lifecycle composition, transport, DTO compatibility, and
  presentation only.
- The retired TypeScript engine and old Rust bulk/live writer remain in the
  repository solely as differential oracles. They are not package exports and
  are absent from default native builds.
- Legacy `SPAG_ENGINE`, `SPAG_NATIVE_INGEST`, and persisted `engine: "ts"`
  values are ignored by production.

See [RFC 011](../../docs/rfcs/011-rust-observation-query-engine.md) and the
[Phase 0–8 audit](../../docs/rfcs/011-phase-0-8-audit.md).

## React components

Presentation components remain available from
`@vibecook/spaghetti-sdk/react` and require React 19 as an optional peer. Pass a
structurally checked asynchronous client through `<SpaghettiProvider
client={client}>`. Live hooks load snapshots in effects and suppress late
results from superseded scopes; they never execute a database read during
render.

## Requirements

- Node.js `>=22.13.0`
- A supported native prebuild for the host platform
- At least one explicit agent data root

## License

[MIT](https://github.com/vibecook-dev/spaghetti/blob/main/LICENSE) — James Yong

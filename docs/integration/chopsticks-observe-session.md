# Migrating from `watchSessionTranscript` to `observeSession`

For Chopsticks (and anything else tailing a Claude session). Written against
`@vibecook/spaghetti-sdk` 0.8.0.

- **Contract:** [RFC 012D](../rfcs/012d-session-scoped-observation.md)
- **Reference:** [SDK README → Watching one session](../../packages/sdk/README.md#watching-one-session-observesession)
- **Implementation:** `packages/sdk/src/observe-session.ts`, `crates/spaghetti-napi/src/observer/`

## Why

`watchSessionTranscript` tails one JSONL file and hands you raw parsed lines. It
does not follow subagent transcripts, it re-reads a rewritten file without
telling you, it makes no claim about having delivered everything, and it gives
you no identity you can join against a durable query.

`observeSession` attaches to the whole session tree, reduces records into typed
RFC 012C revisions, names discontinuities, and carries the same
`semantic_revision_ref` a durable query returns for the same revision. It still
opens no database.

## Compatibility window

`watchSessionTranscript` still ships in 0.8.0 and still works. It is removed
**one release after** you migrate. Both can run side by side while you compare
them.

## The request

```ts
import { observeSession, isSemanticEvent } from '@vibecook/spaghetti-sdk';

const observer = observeSession({
  adapter_id: 'claude-code',
  agent_root: `${homedir()}/.claude`,   // must exist
  transcript_path: transcriptPath,      // may not exist yet
  // native_session_id?: string   — if you have one, it is checked, not trusted
  // include_descendants?: boolean — default true
  // max_queued_events?, max_queued_bytes?, poll_interval_ms?
});
```

Fields are snake_case because the type is generated from Rust
(`ObserveSessionRequest` in `packages/sdk/src/generated/`).

Validation is synchronous. An unusable agent root, a locator outside the
adapter's declared source roots, or a `native_session_id` that disagrees with
the locator **throws from `observeSession`**. Once attached, failures arrive as
events instead, so they stay ordered with the data they affect.

Attaching before the transcript exists is supported and is the normal case for
a hook that fires at session start: you get an empty but *complete* bootstrap
with an active watch, and the file's later creation is a `live` event — not a
reset.

## The loop

```ts
try {
  for await (const event of observer) {
    switch (event.type) {
      case 'bootstrap_complete':
      case 'resync_complete':
        commitStagedEpoch(event.scope_epoch); // barrier carries full coverage
        break;
      case 'overflow':
        discardEpoch(event.scope_epoch);      // replacement snapshot follows
        break;
      case 'reset':
        reloadObject(event.source.object_path);
        break;
      case 'source_error':
        if (event.terminal) warn(event.message);
        break;
      case 'error':
        // the observer itself gave up; no further continuity claim
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

Iteration is single-consumer: `[Symbol.asyncIterator]()` returns the same
iterator every time. Leaving the loop — `break`, `return`, or a throw — closes
the attachment.

## Epochs and resync

Everything the observer delivers belongs to a `scope_epoch`. Deduplicate on
**`(scope_epoch, event_id)`**, never on `event_id` alone: a new epoch
deliberately replays stable ids.

When continuity is lost — today that means the queue saturated — you get
`overflow` naming the invalid epoch and its last contiguous sequence. Ordinary
delivery stops, and a complete replacement snapshot arrives in the next epoch,
ending in `resync_complete`. The rules for a consumer:

1. freeze the old epoch's state when `overflow` arrives;
2. stage the new epoch separately;
3. swap atomically at `resync_complete`;
4. **remove every entity absent from the replacement** — the barrier's
   `family_manifest` gives per-family counts and a digest precisely so absence
   is actionable;
5. never merge partial staging into the old epoch.

You may keep showing the old epoch as stale while resync runs. You may not
treat it as current.

`sequence` orders delivery **within one attachment only**. It is not comparable
across attachments, and never comparable to a durable `atCommitSeq`.

## Closing

`close()` is idempotent and resolves once every owned watch, read, and decode
has stopped. A live session has no natural end, so a parked `for await` needs
something to close it — a session switch, a shutdown handler. If you already
hold an `AbortSignal`, pass it:

```ts
const observer = observeSession(request, { signal: controller.signal });
for await (const event of observer) apply(event);
controller.signal.throwIfAborted(); // only if you want the abort to propagate
```

Aborting is a clean stop, not an error: queued events and the final `closed`
event are still delivered and the loop returns. That is deliberate — a consumer
applying events should not lose the ones it already has to a rejection it did
not ask for.

## What you actually receive today

The wire carries all eleven families. **The Claude adapter emits three of
them**: `actor_run`, `actor_affiliation`, and `usage_v2`.

`message`, `content_block`, `tool`, `effective_state`, `user_input_request`,
`task`, `plan`, and `native_marker` have reducers, fixtures, and wire support
but no Claude emitter yet — they arrive with lane L5 of the landing. Plan for
them (switch on `family`, treat an unhandled family as ignorable) but do not
ship a feature that depends on them yet.

`SemanticEvent.value` is typed `unknown` until L5 puts `ts_rs::TS` on
`adapter/facts.rs`. Narrow on `family` and validate at your own boundary:

```ts
if (isSemanticEvent(event) && event.family === 'usage_v2') {
  const usage = event.value as MyUsageShape; // your contract, for now
}
```

Native content never travels on this stream: transcript streams are declared
`HashOnly`, so `event.source` carries the record's digest and byte range, not
its bytes. If you need message text, read it yourself or use a durable query.

## Porting table

| `watchSessionTranscript` | `observeSession` |
| --- | --- |
| `watchSessionTranscript(path, opts)` | `observeSession({ adapter_id, agent_root, transcript_path }, opts)` |
| `tail.onMessage(cb)` | `for await (const event of observer)` |
| `tail.onError(cb)` | `source_error` (one object) and `error` (the observer) events, in stream order |
| `tail.poll()` | not needed — the watcher-directed sweep does it; latency is ~8 ms p95 |
| `tail.stop()` | `await observer.close()`, or `options.signal` |
| `event.message` (a raw `SessionMessage`) | a reduced RFC 012C revision in `event.value`, plus `event.family` |
| `event.msgIndex`, `event.byteOffset` | `event.sequence` for order; `event.source.byte_start` / `byte_end` for position |
| `event.rewrite` | a `reset` event naming the object, its old and new generation, and the reason |
| one file | root transcript + subagent transcripts + declared sidecars |
| no identity | `event_id`, `scope_epoch`, and `semantic_revision_ref` |
| no continuity claim | `overflow` → full replacement epoch |
| `options.pollIntervalMs` | `poll_interval_ms` on the request (a reconciliation fallback, not the primary path) |
| `options.errorBackoffMs` | none — a failed object is retried by the sweep and reported non-terminal |

`SessionMessage` is unchanged and still exported; it is simply no longer what
this stream delivers.

## Suggested migration order

1. Bump the SDK. Keep `watchSessionTranscript` running and unchanged.
2. Attach `observeSession` in parallel behind a flag, applying into a shadow
   reducer. Compare against the tail's output on real sessions.
3. Move `usage_v2` off the tail first — it is emitted today and it is where the
   tail was least correct (it saw repeated rows as separate consumption).
4. Handle `overflow`/`resync_complete` properly before you rely on the observer
   for anything you would not recompute.
5. Flip the flag; keep the tail as fallback for one release.
6. Remove the tail import once L5 lands the remaining families and you no
   longer need raw lines.

## Not there yet

- No `readArtifact`. Workflow definitions, journals, and team configuration are
  not readable through the observer.
- No consumer-requested resync. `OverflowReason` declares
  `consumer_requested`, but only `queue_full` is produced today.
- No `capabilities()`. The `family_manifest` on a barrier is the honest
  substitute: it reports what was actually reduced.

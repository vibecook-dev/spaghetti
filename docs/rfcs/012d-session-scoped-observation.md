# RFC 012D: Database-free session-scoped observation

- **Status:** Implemented (landing 2026-08-23); ratification pending owner review
- **Created:** 2026-08-15 · **Trimmed to what shipped:** 2026-08-23
- **Parent:** [RFC 012 umbrella](./012-evidence-backed-adapters-and-progressive-readiness.md)
- **Depends on:** [RFC 012A](./012a-agent-adaptation-and-engine-boundaries.md)
  and [RFC 012C](./012c-runtime-semantics-and-usage-v2.md)
- **Landing:** [landing plan](./012-landing-plan.md) §3.1 (consumer) and §8 (lanes L1, L2)
- **Downstream:** [Chopsticks migration note](../integration/chopsticks-observe-session.md)
- **Full 2026-08-15 draft:** [archive/012d-…-2026-08-15-draft.md](./archive/012d-session-scoped-observation-2026-08-15-draft.md)
- **Owns:** scoped observer lifecycle, scope confinement, event identity, scope
  epochs and full-replacement resync, bounded queues, and the control lane
- **Does not own:** durable query or readiness authority (012B), adapter
  decoding (012A), runtime fact meanings (012C)

## 1. What this document is now

The 2026-08-15 draft specified an observer with negotiated contract versions,
an application-receipt protocol between the SDK helper and its consumer, a
bounded artifact-read API, and a `poll()` completion-ticket algebra. The first
implementation of that was ~74k lines and no consumer ever attached to it.

What shipped is 3,636 production lines over `append_delimited` and
`decode_record`, sharing the RFC 012C reducers with durable ingestion. Every
semantic guarantee below is enforced and tested; the negotiation, receipt, and
artifact layers are not there, and §8 says so.

## 2. Decisions (as implemented)

1. **One attachment, one known root.** The observer follows only declared
   relations from one root transcript. It opens no database, no migration, no
   query pool, and no whole-adapter host, and it never enumerates a global
   agent root to find its scope.
2. **One decode spine.** Source driver → `SourceRecord` → adapter decoder →
   `FactBatch` → RFC 012C reducers. The observer is not a second parser and
   adapters do not implement a second tail.
3. **Watch before scan, reset before replay.** Watches are installed before
   the initial read. Filesystem notifications are hints; the bounded
   reconciliation sweep is authoritative and stands on its own.
4. **Every event has a deterministic `event_id` and belongs to a
   `scope_epoch`.** Consumers deduplicate by `(scope_epoch, event_id)` — never
   by `event_id` across epochs, because a new epoch rebuilds state.
5. **Root and actor identity travel with every event**, controls included.
   Identity is final before the first watch: a locator that disagrees with a
   declared session id fails the attach rather than emitting a provisional key.
6. **Bootstrap, live, and correction are distinct phases**, named on the wire.
7. **Continuity loss is explicit.** A saturated queue does not drop events
   quietly; it invalidates the epoch, says so, and follows with a complete
   replacement snapshot in a new epoch.
8. **Semantic revisions carry `SemanticRevisionRef`** — the same value a
   durable query returns for the same revision. That is the join identity;
   `event_id` is the delivery idempotency key and is not a substitute.
9. **No native content leaves the observer.** Transcript streams are declared
   `HashOnly`: an event carries the record's digest and byte range, not its
   bytes.
10. **Observation failure never fails the agent.** One unreadable object is an
    event, not the end of the stream.

## 3. Semantic rules the observer enforces

**Attach.** `ObserverHandle::open` settles identity, adapter support, and the
declared scope program synchronously, then hands the scope to one owner thread.
A bad request fails the call. A named-but-absent root is not an error: it
produces an empty complete bootstrap with an active watch, and later creation
is `live`, not a reset.

**Scope.** Every followed object names the relation that admitted it
(`ObjectCoverage.relation_id`). A locator outside the adapter's declared source
roots is refused before any watch is installed. A sidecar joins the scope only
when evidence names it, and an evidence-named sidecar that does not exist costs
nothing until it appears.

**Ordering.** `sequence` is monotonic within one attachment. It is not
comparable across attachments and never comparable to a durable commit
sequence. Record order is preserved within one object generation; across
objects, sequence is delivery order only.

**Generations.** Truncation, wholesale rewrite, and file replacement each bump
the generation and emit `reset` with the old and new generation and a reason
before any corrected replay. A partial trailing line is buffered until it
completes rather than decoded as a record.

**Barriers.** `bootstrap_complete` and `resync_complete` carry the full
`family_manifest` and `coverage`, so a consumer can swap a staged epoch
atomically and know that an entity absent from a complete family is genuinely
gone. Family digests are topology-neutral: a clean bootstrap and a completed
resync at equal coverage produce equal digests.

**Overflow.** On queue saturation the observer marks the epoch invalid, emits
`overflow` with the last contiguous sequence, suppresses ordinary delivery, and
builds a complete replacement snapshot in the next epoch. Two attachments to
one tree derive the same event ids, which is what makes the replacement
comparable.

**Close.** Idempotent. It rejects new source work, wakes any parked wait, and
waits for every owned watch, read, decode, and delivery to stop. Exactly one
`closed` event is emitted and it reports how many events were discarded.

## 4. Shipped interface

**Rust.** `crates/spaghetti-napi/src/observer/` — `mod.rs` (`ObserverHandle`,
`ObserverError`), `request.rs`, `scope.rs`, `identity.rs`, `object.rs`,
`queue.rs`, `runtime.rs`, `event.rs`, `state.rs`, `bindings.rs`.

**Native.** `crates/spaghetti-napi/index.d.ts`:

```ts
observeSession(request: string | Record<string, unknown>): SpaghettiSessionObserver

class SpaghettiSessionObserver {
  poll(max?): string                                  // JSON array of ObserverEvent
  waitForEvents(timeoutMs, max?): Promise<string>
  status(): ObserverStatus
  close(): Promise<void>
}
```

The stream crosses N-API as a JSON string on purpose: it measured 2.3–2.6×
faster than marshalling the same events as napi objects.

**SDK.** `packages/sdk/src/observe-session.ts` wraps that in an async
iterator — `observeSession(request, options)` returning a `SessionObserver`
with `status()` and `close()`, plus the `isSemanticEvent` narrowing helper.
One batch is buffered at a time, so a slow consumer applies backpressure to the
native queue instead of growing a JavaScript array. `options.signal` ends the
attachment cleanly: queued events and the final `closed` still arrive and the
loop returns rather than throwing.

**Generated events** (`packages/sdk/src/generated/`, from `observer/event.rs`
and `observer/identity.rs`): `ObserverEvent` is the tagged union —
eleven semantic variants sharing `SemanticEvent`, plus `unknown_evidence`,
`bootstrap_complete`, `reset`, `overflow`, `resync_complete`, `source_error`,
`closed`, `error`. Supporting types: `ObserveSessionRequest`, `ObserverEventId`,
`ObserverFamily`, `ObserverPhase`, `SemanticOperation`, `SourcePosition`,
`ActorRef`, `ActorAttribution`, `ObserverBarrier`, `FamilyManifestEntry`,
`ObjectCoverage`, `OverflowEvent`, `OverflowReason`, `ResetEvent`,
`ClosedEvent`, `SourceErrorEvent`, `ObserverErrorEvent`,
`UnknownEvidenceEvent`.

Usage documentation: `packages/sdk/README.md`, section *Watching one session*.

## 5. Acceptance tests

28 behavioural Rust tests in `crates/spaghetti-napi/src/observer/tests/`
against real temporary files — no mocks, no digest-stability tests.

`lifecycle.rs` (9): bootstrap from a real transcript, live append,
partial trailing line, truncation reset-before-replay, wholesale rewrite as one
discontinuity, file replacement as discontinuity not append, repeated usage row
adding nothing while a correction replaces, an uninterpretable record not
terminating the stream, and a session larger than the pass bound still
completing bootstrap first.

`scope.rs` (12): subagent transcript created after bootstrap joining the scope,
every followed object naming its relation, a transcript outside the projects
root refused before any watch, a declared session id disagreeing with the
locator failing attachment, attaching before the root exists, a sidecar joining
only on evidence, one unreadable child not stopping siblings, an unmappable
sidecar arriving as bounded evidence, a change with no watcher notification
still picked up by the sweep, a pass reading only what changed, a burst
coalescing into few passes, and an evidence-named sidecar that does not exist
costing nothing.

`epoch.rs` (4): a saturated live queue reporting continuity loss instead of
dropping events, a replacement snapshot equalling a clean bootstrap at the same
coverage, idempotent close reporting what it discarded, and two attachments to
one tree deriving the same event ids.

`families.rs` (3): all eleven families reducing and appearing in the
replacement manifest, an exact repeat adding nothing, retraction emptying a
family.

**SDK, on real `.claude`-shaped trees.**
`packages/sdk/src/__tests__/observe-session.test.ts` (9): the generated union
over `for await`, live append without reattaching, a subagent transcript
joining after attach, a rewrite reported as `reset`, leaving the loop closing
the attachment, single-consumer iteration ending after `closed`, an aborted
signal ending iteration cleanly, an already-aborted signal closing immediately,
and an invalid locator failing at attach.
`packages/sdk/src/__tests__/session-observer.test.ts` (3) covers the native
binding directly, including the JSON-string request form.

## 6. Performance, measured

- Append → consumer p95: **8.3 ms** at 674 in-scope objects (0.2 ms at one
  object), down from 40.2 ms before the watcher-directed dirty set and stat
  pre-check sweep.
- Object opens during bootstrap: 21,254 → **1,428**.
- Root bootstrap of a 43.7 MB tree: **635 ms**, decoder-bound — adapter 64%,
  I/O 22%, reduce 10%. Getting under the landing plan §6 budget of 500 ms for a
  50 MB tree is Claude-decode work, tracked as lane L5 item 3.

## 7. Superseded sections of the 2026-08-15 draft

- §5 `ObserveSessionRequest` with `supported_contract_versions`,
  `source_access_grant`, `raw_record_policy`, `artifact_access_policy` —
  superseded by the generated `ObserveSessionRequest`
  (`observer/request.rs`): adapter, agent root, transcript path, optional
  native session id, `include_descendants`, queue bounds, poll interval.
- §6 `SessionObserver` with `capabilities()`, `poll()`, `ready()`, `resync()`,
  `readArtifact()` — superseded by the four-method native class and the SDK
  async iterator in §4.
- §6 contract-version negotiation and `IncompatibleObservationContract` —
  superseded by generated types plus `pnpm generate:types` checked in CI. One
  build, one shape.
- §7 `ObservationEnvelope` — superseded by `ObserverEvent` and `SemanticEvent`.
  Envelope-level `evidence`, `affiliations`, and `native_evidence` inline
  payloads are gone; `SourcePosition` carries digest and byte range instead.
- §8 event-id derivation — implemented in `observer/identity.rs`
  (`ObserverEventId`, an opaque 32-byte deterministic value as lowercase hex).
- §10 the receipt/acknowledgement protocol and the `ready()`/drain ordering
  rules — superseded by backpressure: one buffered batch, and iteration that
  cannot outrun application.
- §11 poll completion tickets — superseded by the watcher-directed dirty set
  and bounded sweep in `observer/runtime.rs`.
- §12–§13 the two-lane queue, `resync_required`/`resync_started`, and
  `ResyncBarrier` — superseded by one ordered stream with control events
  (`overflow` then a replacement epoch ending in `resync_complete`) and
  `ObserverBarrier` for both barriers.
- §16 bounded artifact retrieval — not implemented; §8.
- §19 performance targets — superseded by §6 and landing plan §6.

## 8. Not implemented

- **Artifact reads.** There is no `readArtifact`. A consumer that needs
  workflow definitions, journals, or team configuration reads them itself or
  waits for a later contract.
- **Consumer-requested resync.** `OverflowReason` declares
  `consumer_requested`, but nothing produces it: `queue_full` is the only path
  into a replacement epoch today. There is no public `resync()`.
- **`capabilities()`.** Per-family `Supported | Degraded | Unsupported`
  reporting does not exist on the observer. The `family_manifest` in a barrier
  is the closest thing: it says what was actually reduced.
- **Eight of eleven Claude families.** The wire carries all eleven, but the
  Claude adapter emits only `actor_run`, `actor_affiliation`, and `usage_v2`
  (RFC 012C §7). Lane L5 adds the rest.
- **Multi-observer isolation as a tested property.** Each attachment owns its
  queue, epoch, and cancellation state, and two attachments to one tree are
  tested — but "three roots, one slow enough to overflow, others keeping
  latency" from the draft's §22 is not a test that exists.

## 9. Compatibility

`watchSessionTranscript` still ships and still works. It is deprecated and is
removed one release after Chopsticks migrates — the landing plan gives it one
release of overlap. The migration table is in `packages/sdk/README.md` and the
step-by-step note is
[docs/integration/chopsticks-observe-session.md](../integration/chopsticks-observe-session.md).

## 10. Acceptance

RFC 012D is met for this landing when the observer creates no database and
touches no unrelated root; attach-before-create, watcher-before-scan,
reset-before-replay, partial-write buffering, and per-object error isolation
all hold; ids are deterministic and stable across attachments; a saturated
queue reports continuity loss and replaces the epoch rather than dropping
events; and a replacement snapshot equals a clean bootstrap at equal coverage.
All hold as of 2026-08-23 (landing plan §8, lanes L1 and L2). The gates that
remain open are the ones §8 names.

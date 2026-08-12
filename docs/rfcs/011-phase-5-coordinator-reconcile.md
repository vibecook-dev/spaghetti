# RFC 011 Phase 5: declared-object reconcile path

Status: manual common-engine reconcile implemented and validated on 2026-08-12

This slice turns the durable coordinator foundation into an executable Rust
observation path. It deliberately lands as a library-first reconcile operation
before native watcher scheduling: a watcher hint, a poll, overflow recovery,
and an explicit refresh must all converge through this same operation.

## Common coordinator ownership

`ObservationCoordinator` now:

- asks an adapter to discover instances and declare streams;
- reserves a new durable source-instance ID before adapters derive any entity
  or fact identity from it;
- validates stream declarations and component-aware include/exclude selectors;
- scans configured roots with bounded depth and entry counts without following
  symlinks;
- hydrates catalog objects, driver checkpoints, decoder state, and adapter
  object context through the read-only catalog boundary;
- dispatches `AppendDelimitedFile`, `ReplaceDocument`, and `PresenceObject`;
- decodes one bounded record batch at a time through `AgentAdapter`;
- commits facts, diagnostics, driver/decoder state, projection updates, source
  cursors, and the durable change log through the existing writer transaction;
- reports bounded reconcile counters and the final commit sequence.

The coordinator contains no Claude path literals, decoder selection, native
JSON parsing, projection SQL, or capability policy. Claude remains a host-side
adapter selection at the N-API boundary, while all observation mechanics are
adapter-neutral.

`DirectorySnapshot` remains a membership/change producer rather than an
adapter-record stream. It is rejected explicitly by this record coordinator;
the upcoming scheduler/discovery slice will consume it as dirty-object input
instead of fabricating source records.

## Resume, rewrite, and deletion semantics

Append objects resume from the durable common-driver checkpoint and preserve
adapter decoder state only while the source generation and decoder contract
remain compatible. Truncation, identity replacement, prefix rewrite, or a
decoder-contract replay rebases the generation and retracts prior projections
atomically before replacement facts become visible.

Replace and presence drivers now encode confirmed absence in their checkpoints.
The presence-aware replace checkpoint uses a new inner encoding version while
continuing to decode the previously shipped present-only checkpoint format.
With `MirrorSource`, deletion advances to a new absent generation and commits an
empty typed batch, so assertions disappear in the same transaction as the
catalog state. Re-creation advances the generation again. `PreserveHistory`
leaves the prior durable observation intact.

Append deletion follows the same generation rule. Ordinary append records do
not replace accumulated history: in particular, transcript file-history
metadata survives later non-artifact records and retracts only on generation
replacement.

Known ignored records advance only catalog/cursor state. Permanent decoder
diagnostics and driver quarantines are durable; transient dispositions leave
the cursor unchanged and increment the retry counter. Raw payload retention
follows the declared stream policy.

## Public boundary

The persistent N-API and TypeScript SDK expose:

```ts
engine.reconcileClaude({
  roots: ['/path/to/.claude'],
  reason: 'manual_reconcile',
});
```

The method runs off the JavaScript thread, supports the existing abort-signal
task boundary, and returns discovered/reconciled/changed/removed/decoded/
quarantined/retry/commit counters. It is intentionally explicit and manual;
this slice does not claim watcher ownership or remove the legacy live path.

## Conformance evidence

Tests cover:

- declared nested glob shapes and invalid recursive selectors;
- real Claude transcript decoding through discovery, common framing, adapter
  decode, typed projection, and the durable writer transaction;
- exact append resume after engine shutdown and restart;
- replace-document deletion and recreation retracting/restoring settings;
- append deletion and recreation with monotonic generations;
- transcript artifact metadata surviving unrelated later append records;
- SDK invocation against the rebuilt native debug addon.

The slice is gated by the full Rust library suite, clippy with warnings denied,
TypeScript typechecking, the production build, the RFC 011 ownership ratchet,
format/diff checks, and the focused persistent-engine SDK test. On the
validation machine, the separate real-`~/.claude` format probes currently
report unrelated unmodeled Claude fields/files (`promptSuggestionEnabled`, two
telemetry event names and environment fields, session key files,
`my-closed-issues.json`, and expanded subagent metadata); those corpus-drift
findings are not part of this coordinator slice.

## Next coordinator slice

Add the bounded dirty-instance scheduler and consolidated native watch roots.
Watcher overflow, lost hints, adaptive polling, and explicit refresh must all
enqueue the reconcile path above. Lifecycle status and health should expose
initial-scan, live, degraded, and retry state without creating another ingest
route.

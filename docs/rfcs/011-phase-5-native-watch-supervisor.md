# RFC 011 Phase 5: native watch-before-scan supervisor

Status: native Claude observation supervisor implemented and validated on 2026-08-12

This slice connects the engine-owned lifecycle and dirty-admission control plane
to a real cross-platform filesystem backend. Native notifications, polling,
manual refresh, overflow recovery, and initial backfill now all reuse the
declared-object coordinator committed in the preceding slices.

## Watch-before-scan ownership

`SpaghettiEngineCore` owns supervisor handles by adapter ID. Starting Claude
observation now performs this sequence entirely in Rust:

1. canonicalize and deduplicate configured adapter roots;
2. ask the Claude adapter to discover source instances and logical roots;
3. collapse aliases and overlapping logical roots to the shallowest physical
   native watch registrations;
4. register those roots with `notify`;
5. begin the common coordinator's initial scan;
6. reconcile every dirty marker admitted during the scan;
7. report `live` only after that boundary is clear.

The watcher callback admits dirty state synchronously into the bounded engine
map. Its separate bounded channel is only a coalesced worker wake signal. A
full wake channel therefore cannot lose an invalidation: the corresponding
instance or adapter-wide recovery marker already exists before the callback
returns. This also means a hint arriving after the scan lease begins is not
acknowledged by that lease.

## Routing, coalescing, and recovery

Native paths route to every logical source instance whose adapter-declared root
contains the path. Events without a trustworthy path conservatively request a
full adapter reconcile. Access-only notifications are ignored; create, modify,
remove, rename, and imprecise events are invalidation hints rather than domain
events.

Backend errors, native rescan flags, and internal dirty-capacity overflow
escalate to known-loss recovery. One 20 ms window coalesces callback wakes, and
each wake drains a bounded number of coordinator passes. If more work remains,
the dirty state stays visible and the polling backstop will continue repair.

The existing common polling policy now drives the supervisor timer:

- 50 ms while an append object has an incomplete trailing record;
- 500 ms for recently active instances and after repeated watcher failure;
- 5 seconds while idle.

Polling performs a full adapter reconcile, so dropped or unavailable native
events cannot become permanent divergence.

## Lifecycle and public host surface

The engine rejects duplicate supervisors for the same adapter and retains the
only owning handle. Shutdown stops watcher scheduling before observation,
query workers, writer, and database ownership are released. The supervisor
holds only a weak reference back to the engine, preventing a lifecycle cycle.

The N-API and SDK persistent handle expose:

```ts
await engine.startClaudeObservation({ roots: ['/path/to/.claude'] });
await engine.refreshClaudeObservation();
await engine.stopClaudeObservation();
```

Observation status now reports running supervisor count, discovered watched
instances, and consolidated physical watch roots in addition to the lifecycle,
dirty/recovery, retry, and commit fields from the prior slice.

This is an opt-in persistent-engine path. It does not yet remove the legacy
TypeScript live services or claim the RFC 011 production cutover gate.

## Conformance evidence

Tests prove:

- missing Claude subroots and overlapping logical roots consolidate to one
  physical registration;
- a native path marks only its owning source instance;
- native rescan and backend failures force adapter-wide recovery;
- a real filesystem rewrite is observed and reconciled after watch-before-scan;
- duplicate start is rejected, stop is idempotently observable, and status
  reports one supervisor/instance/root;
- SDK start, explicit refresh, stop, and post-stop error behavior work through
  the rebuilt native addon;
- engine shutdown owns watcher cleanup before database-worker teardown.

The slice passes 413 Rust tests, clippy with warnings denied, TypeScript
typechecking, the production build, the RFC 011 ownership ratchet, and the
focused five-test persistent-engine SDK suite.

## Next coordinator slice

Add deterministic callback/scan race injection and supervisor restart tests,
then begin the production cutover: route persistent-engine hosts through this
Rust supervisor, shadow parity against the legacy TypeScript live plane, and
retire source-specific watcher/writer services only after zero-diff acceptance.

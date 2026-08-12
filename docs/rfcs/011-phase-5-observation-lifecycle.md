# RFC 011 Phase 5: observation lifecycle and bounded dirty admission

Status: engine lifecycle foundation implemented and validated on 2026-08-12

This slice gives the persistent engine one observation-control plane around
the declared-object coordinator. It does not yet register an operating-system
watch backend or claim production live-observation ownership.

## Serialized coordinator ownership

Every full or declared-instance reconcile now acquires an engine-owned lease.
The lease exposes `scanning` and `reconciling` transitions, guarantees at most
one coordinator pass per engine, and records completion at a query-visible
commit sequence. A concurrent request is not silently dropped: its affected
scope is retained as dirty and the caller receives a typed busy error.

Shutdown stops admission and waits for the active observation lease before it
closes query workers, the writer, and the owner lock. An abandoned lease marks
recovery state through its drop guard rather than allowing the engine to claim
that observation is live.

## Bounded dirty-instance state

The engine retains a bounded map keyed by adapter and stable source-instance
identity. Duplicate hints coalesce and preserve the strongest recovery reason.
When the map reaches capacity, affected instance entries collapse to one
adapter-wide `InternalQueueOverflow` marker. The marker requires a full
discovery/reconcile pass and makes the lifecycle degraded; queue saturation
therefore cannot be mistaken for successful delivery.

Each dirty marker has an admission sequence. Successful reconcile acknowledges
only markers that existed before that pass began. Hints admitted during scan or
reconcile survive completion, preventing the initial-scan race from falsely
transitioning back to `live`. Retry dispositions and failed passes likewise
retain recovery state.

The runtime now distinguishes:

- `idle`, before any reconcile;
- `scanning` and `reconciling`, while a pass owns the lease;
- `live`, at a known commit with no retained invalidation;
- `dirty`, for ordinary pending hints;
- `degraded`, for retry, backend, identity, root, cursor, or overflow recovery;
- `stopped`, after observation admission closes.

## Status and health boundary

Rust, N-API, and the TypeScript SDK expose the nested observation snapshot on
the existing engine status. It includes pending instance/full-reconcile state,
in-flight state, an independent known-loss recovery bit, reconcile/failure/
retry/overflow counters, start/finish times, the last known commit sequence,
and a bounded last error. Engine health remains unhealthy while recovery is
required, including while a repair pass is actively scanning or reconciling,
independently of writer/query worker health.

No source payload or dirty stable key crosses this diagnostics boundary.

## Conformance evidence

Tests prove:

- successful full scan/reconcile reaches `live` at the durable commit;
- instance hints coalesce and bounded overflow escalates to full reconcile;
- hints arriving after a pass starts survive that pass;
- incomplete append records and coordinator failures retain degraded recovery;
- shutdown rejects new observation work and reports `stopped`;
- a real Claude coordinator pass exposes live, retry-degraded, and
  failure-degraded status;
- the SDK observes idle, live, and stopped lifecycle snapshots.

The slice passes 407 Rust tests, clippy with warnings denied, TypeScript
typechecking, the production build, the RFC 011 ownership ratchet, and the
focused persistent-engine SDK test.

## Next coordinator slice

Add consolidated native watch roots and the background supervisor that drains
this dirty state. Watch registration must precede initial scan; native events,
backend overflow, adaptive polling, incomplete-tail timers, and explicit
refresh must all feed this admission/control plane and reuse the committed
coordinator path.

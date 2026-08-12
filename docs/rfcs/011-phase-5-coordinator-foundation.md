# RFC 011 Phase 5: observation-coordinator foundation

Status: durable resume-state foundation implemented and validated on 2026-08-12

This slice establishes the storage and hydration boundary required by the
common Rust observation coordinator. It does not yet register native watchers
or cut production Claude live observation over from TypeScript.

## Durable state ownership

Schema version 31 adds two nullable columns to `source_objects`:

- `driver_checkpoint`;
- `driver_checkpoint_version`.

The common source driver owns these fields. Adapter-owned incremental state
continues to use the separately versioned `decoder_state` fields. Both kinds
of state advance in the same source cursor/projection/outbox transaction, but
they are never multiplexed into one opaque payload.

This separation is required for restart-safe append handling. A committed byte
cursor is insufficient to detect identity replacement, prefix rewrite, or
truncation safely; `AppendDelimitedFile` also needs its bounded identity and
prefix-anchor checkpoint. Snapshot and presence drivers likewise retain their
own versioned restart state without consuming adapter decoder storage.

Checkpoint and version presence must agree, the version must be nonzero, and
the encoded checkpoint is bounded to 64 MiB. Invalid pairs fail before any
source-instance, object, or ingest-commit row is written.

## Catalog hydration boundary

The persistent engine's bounded read-only query pool now supports one internal
snapshot query keyed by adapter ID and source-instance stable key. It returns:

- the durable source instance and adapter contract version;
- declared stream IDs, driver/decoder keys, state, and reconcile watermark;
- object IDs, generations, committed cursors, revisions, and source state;
- adapter object context;
- common driver checkpoint and version;
- adapter decoder state and version;
- bounded file metadata and last commit sequence.

The result is consumed through a crate-internal `SpaghettiEngineCore` method.
The future coordinator therefore does not receive a SQLite handle or issue
catalog SQL directly, and adapters remain entirely outside storage ownership.

## Conformance evidence

Tests prove:

- schema v31 creates both checkpoint columns;
- common driver and adapter decoder states persist distinctly in the same
  atomic source commit;
- missing, zero-versioned, and oversized checkpoint pairs fail before writes;
- a fresh query pool after writer shutdown hydrates the exact checkpoint,
  decoder state, cursor, IDs, state, and commit sequence;
- an unknown source instance returns a typed empty catalog snapshot;
- catalog reads stay on the existing read-only/query-only worker lane.

The validation gate passes with 391 Rust tests, clippy with warnings denied,
TypeScript typechecking, the production build, the RFC 011 ownership ratchet,
format/diff checks, and zero differences for the small Claude, medium Claude,
Codex, and Grok ingest matrices.

## Follow-on coordinator slice

The declared-object reconcile path described in
[the follow-on phase record](./011-phase-5-coordinator-reconcile.md) now
enumerates adapter-declared file objects, hydrates these checkpoints, dispatches
the common append/replace/presence drivers, decodes facts, and commits cursor,
projection, and outbox updates through the persistent writer. Native watcher
hints and polling remain the next layer and will reuse that reconcile path
rather than create a second ingest route.

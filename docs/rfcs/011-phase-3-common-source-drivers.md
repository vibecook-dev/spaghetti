# RFC 011 Phase 3: common append and snapshot drivers

Status: synthetic-driver exit gate met on 2026-08-11

Phase 3 adds the adapter-neutral source mechanics used by later Claude, Codex,
and Grok migrations. The implementation is library-first under
`crates/spaghetti-napi/src/source`; it does not parse an agent format, access
the Spaghetti database, publish public changes, or expose N-API types.

## Common provenance and cursor model

Every framed payload is a raw `SourceRecord` carrying source instance, stream,
object, generation, cursor range, batch ordinal, observation timestamps, media
type, bytes, and a BLAKE3 payload hash. Adapters receive those bytes and own
their semantic decoding.

Driver checkpoints and record cursors have explicit binary versions. Append
offsets use big-endian encoding so cursors retain unsigned offset ordering.
Snapshot cursors carry a BLAKE3 revision. Checkpoint decoders reject wrong
driver magic, unknown versions, truncation, trailing bytes, impossible ranges,
zero generations, duplicate directory keys, and inconsistent snapshot hashes.

Native identity is device/inode on Unix and volume/file ID on Windows when the
platform exposes it. The fallback is a binary-safe confined path key. Display
paths remain separate and may be lossy; identity keys retain non-UTF-8 bytes.

## Driver behavior

### `AppendDelimitedFile`

The v1 implementation uses a single-byte delimiter; JSONL configures `\n` and
optional CRLF normalization. It reads a fixed 64 KiB buffer and enforces
independent maximum record, batch-byte, and record-count bounds.

- Only delimiter-terminated records are emitted. An incomplete suffix remains
  after the committed byte offset and requests a short retry.
- Cursor endpoints include the delimiter while delivered payloads do not.
- A complete oversized record becomes bounded quarantine metadata and advances
  the cursor; an incomplete oversized suffix remains pending.
- File identity replacement, truncation below the committed offset, mismatch of
  the bounded verified prefix anchor, and adapter-contract replay each start a
  new generation from byte zero.
- Batch limits stop on a record boundary. Unconsumed bytes are reread from the
  durable committed offset, so provenance is neither split nor skipped.
- Reads use one opened-file identity and reject a concurrent replacement as a
  transient retry.

The older `core::jsonl` cold reader intentionally remains unchanged for legacy
parity. It emits an unterminated EOF line and therefore is not used by the new
live append driver.

### `ReplaceDocument`

A document is read from one handle with metadata checks before and after the
bounded whole-file read, followed by a same-path identity check. A changing or
atomically replaced path during that read is retried. The content revision,
not the write style, determines ordinary snapshot identity: an atomic rename
and an in-place write with the same bytes are both unchanged.

An adapter or fact contract can explicitly declare an incompatible replacement
to increment generation and replay the snapshot. Oversized stable documents
advance through quarantine metadata without retaining raw bytes. The bounded
`MalformedRevisionGuard` retries a newly failing stable revision across a
settle interval, then quarantines that same revision instead of retrying
forever; a changed revision resets the guard.

### `DirectorySnapshot`

The directory driver does not follow symlinks. It passes a binary-safe relative
path and cheap entry kind to selector/ignore policy before reading full
metadata, applies depth and entry bounds, and aborts rather than returning a
partial snapshot when a bound is exceeded.

Stable snapshots are sorted by path key and record names, native identities,
metadata revisions, and per-object generations. Reconciliation reports added,
modified, replaced, and removed objects even when no watcher hint survived.
Identity replacement increments the child generation. A missing source root is
`Unavailable`, not an empty authoritative snapshot, so temporary root loss
cannot retract every child. A root identity change increments the root
generation and treats matching child paths as replacements.

### `PresenceObject`

Initial absence, creation, update, removal, and recreation are explicit
observations. Removal stays in the current generation; creation after confirmed
absence and native identity replacement increment it. Content may be delivered
or omitted by policy, while stable content revisions still participate in
change detection. Oversized optional content is omitted without losing the
existence observation.

The driver deliberately has no expiration API. Any conclusion based on the
host's current clock is a transient assessment for a later reducer, not a
durable source fact.

## Watch, recovery, and bounded scheduling

`WatchBeforeScan` encodes the required startup order: discover, register the
watcher, scan, replay buffered hints, reconcile hints that arrive during
reconcile, then mark the stream live at a known commit sequence. Calling scan
before watcher registration is rejected by construction.

`DirtyCoalescer` deduplicates exact scopes and retains the strongest recovery
reason. On capacity overflow it replaces all detail with one
`Instance/InternalQueueOverflow` hint, ensuring loss affects latency rather
than convergence. Watcher and internal overflow reasons remain explicit.

`BoundedScheduler` has fixed queue and in-flight capacities, coalesces duplicate
object/generation work, promotes priority without duplicating work, serializes
the same object/generation, and uses a weighted work-conserving sequence across
interactive, foreground repair, backfill, and maintenance. Queue saturation
sets a reconcile requirement instead of silently dropping work. Projection
commits still belong to the engine's one ordered writer lane.

`PollingPolicy` gives active objects a faster cadence, switches to fallback
polling after repeated watcher failures, and gives incomplete append tails the
shortest retry even when no new native event arrives. It is a pure timer policy
and stores no wall-clock assessment as source truth.

## Conformance and boundaries

The focused pack covers cursor ordering and corruption, partial and oversized
records, CRLF framing, batch boundaries, truncate/rewrite, atomic replacement,
stable document retry/quarantine, directory add/modify/replace/remove,
temporary root loss, non-UTF-8 identity, presence delete/recreate, duplicate
hints, overflow collapse, startup races, scheduler serialization, fairness,
boundedness, and polling fallback.

A deterministic synthetic-adapter trace exercises all four drivers through an
initial watch-before-scan, buffered reconciliation, opaque-checkpoint restart,
partial-tail completion, atomic replacement, dropped-hint directory reconcile,
presence creation, internal queue overflow, and transcript rewrite.

The RFC 011 architecture ratchet now also rejects source-layer imports of
vendor adapters, engine/storage modules, N-API, SQLite, or JSON decoders. This
keeps the driver boundary mechanical as later adapters are ported.

Verification commands are:

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- --deny warnings
python3 scripts/architecture/check_rfc011_boundaries.py
pnpm typecheck
pnpm lint
pnpm format:check
pnpm build
```

Phase 4 can now declare Claude transcript streams against the shared append
driver and connect decoded facts to the Phase 2 atomic commit boundary. It must
not copy these mechanics into the Claude adapter, treat watcher events as
semantic records, or advance a durable checkpoint before decode and projection
commit succeed.

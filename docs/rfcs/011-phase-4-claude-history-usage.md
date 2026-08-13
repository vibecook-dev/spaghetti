# RFC 011 Phase 4: Claude history and usage in Rust

Status: fixture shadow exit gate met on 2026-08-11

Phase 4 connects the common source and transaction layers to the first real
agent adapter. Claude discovery and JSON meaning live in
`crates/spaghetti-napi/src/claude/adapter.rs`; generic adapter/fact contracts
live under `src/adapter`; storage projection remains common engine code under
`src/engine/projection.rs`.

## Adapter and fact boundary

Adapter identity is an open validated newtype rather than a closed source
enum. A registry rejects duplicate IDs without adding source-specific common
dispatch. Manifests independently version adapter implementation and emitted
semantics, declare source schema families/capabilities, and expose declarative
stream mechanics.

The storage-agnostic fact model provides deterministic namespaced entity keys,
fact IDs, source provenance, qualified timestamps, ordered message content,
run/activity evidence, scope/accounting/quality-aware usage, bounded decoder
state, diagnostics, and forward-compatible unknown records. Batches enforce
fact, diagnostic, and state-size limits before reaching the writer.

The Claude adapter declares parent and subagent transcript streams against the
Phase 3 append driver. Object bootstrap derives bounded stable context from
the catalogued relative path. Decode uses the existing Claude message
projection as its migration oracle while emitting richer common facts. It
does not implement file framing, watchers, cursor persistence, SQL, public
events, or query behavior.

The detailed native source and semantic claims are recorded in
`011-claude-adapter-source-map.md`.

## Schema 18 and common projections

Schema version 18 adds:

- `fact_records`, retaining every fact payload plus object/generation/cursor,
  hash, local ordinal, observation time, and commit provenance;
- `canonical_sessions`, `canonical_messages`, and `canonical_runs` as the
  Phase 4 shadow history projection;
- `run_evidence` and `observed_run_states` for deterministic durable state;
- `usage_contributions` and `usage_totals`, with exact and estimated buckets
  kept separate.

This section records the Phase 4 schema as it shipped. Schema v40 supersedes
the unconditional fact-payload retention described above: `fact_records` is
now the provenance and ownership ledger required for retraction, while only a
stream declaring `Full` retention keeps an additional compressed fact body.
`None` and `HashOnly` streams retain the deterministic fact identity, entity
key, source object/generation/cursor, record hash, ordinal, observation time,
and commit sequence without a duplicate semantic payload. `DiagnosticExcerpt`
does the same for ordinary facts and may retain only the already-redacted,
bounded shape of an unknown record. Canonical projections and source records
remain the rebuild inputs.

Rust remains the target schema authority. The TypeScript DDL is updated only
as a temporary migration mirror so the legacy differential oracle can still
open the same schema. The repository's existing wipe-on-version-change policy
applies when moving from schema 17 to 18.

Every canonical/evidence/contribution row points directly to its applying fact
and source ownership, so display order is no longer the idempotency key. A
generation replacement retracts old canonical rows and evidence, subtracts
old usage contributions, then deletes old audit facts. The replacement facts,
cursor, decoder state, projections, aggregate deltas, and durable changes all
commit in the Phase 2 `BEGIN IMMEDIATE` transaction.

The common fact projector—not the adapter—derives stable public topics.
Changes for the same topic/entity are coalesced to the transaction's final
operation before the ordered outbox is written. Typed commits reject any
caller-supplied change topic.

## Usage and runtime behavior

Claude's exact message deltas update totals in O(changed facts). Duplicate
fact application has delta zero; correction replaces the prior contribution;
generation replay subtracts the old generation before adding the new one.
The audit/repair path can rebuild totals from contribution rows and is tested
against the incremental result.

The common reducer stores native activity as `Active` and gives explicit
terminal evidence deterministic precedence when such adapters arrive later.
Silence never becomes `Completed`; volatile clock/process assessments do not
enter durable history or the outbox.

## Shadow and convergence evidence

The Phase 4 conformance tests prove:

- declarative Claude discovery, stream selection, and path bootstrap;
- shared-driver partial-line handling before decode;
- structured assistant content, exact usage, and unknown-record preservation;
- atomic fact/projection/usage/cursor/outbox commits;
- fresh cold backfill equals one-record live append;
- a forced generation replay equals both and retracts old ownership;
- hot-path usage deltas equal a full audit rebuild;
- all parent and subagent history/search/usage fields in the committed small
  corpus equal the legacy cold ingester after semantic JSON normalization;
- adapters cannot import SQLite, engine writer/event internals,
  orchestration, or N-API under the architecture ratchet.

Verification commands are:

```bash
cargo test --workspace --offline
cargo clippy --workspace --all-targets --offline -- --deny warnings
python3 scripts/architecture/check_rfc011_boundaries.py
pnpm typecheck
pnpm lint
pnpm format:check
pnpm build
```

Phase 5 can now add Claude delegation/runtime capability projections and an
observation coordinator that schedules the declared streams. The production
live path has not switched yet; legacy ingest remains the release path while
the Rust shadow projections accumulate capability-pack evidence.

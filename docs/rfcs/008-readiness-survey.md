# RFC 008 — Pre-Phase-0 Codebase Survey

**Companion to:** [RFC 008 — Rust Bulk Ingest Production Readiness](./008-rust-ingest-production-readiness.md)
**Surveyed:** 2026-08-08 at `13ac10e` (after RFC 007's removal)
**Status:** Findings only. No production behavior changed.

What the RFC asks for, measured against what is actually in the tree. This
exists so Phase 0 starts from facts rather than from the RFC's assumptions about
the code.

---

## 1. Findings that change the work

### 1.1 Fingerprints are keyed by path alone — in *both* engines

RFC 008 §P8 and Phase 1.2 call for migrating `source_files` to
`PRIMARY KEY(source_id, path)` and warn against recovering ownership with
`starts_with(root)`. The reality is adjacent but not identical, and worth
stating precisely before anyone writes the migration:

| Site | Query |
| --- | --- |
| `packages/sdk/src/data/schema.ts:226` | `source_files (path TEXT PRIMARY KEY, source_id TEXT NOT NULL DEFAULT 'claude-code', …)` |
| `crates/spaghetti-napi/src/claude/fingerprint.rs:229` | `SELECT … FROM source_files` — every row, into a path-keyed map, no `source_id` filter |
| `packages/sdk/src/data/ingest-service.ts:713` | `SELECT … WHERE path = ?` |
| `packages/sdk/src/data/ingest-service.ts:721` | `SELECT … FROM source_files` — all rows |
| `packages/sdk/src/data/ingest-service.ts:730` | `DELETE FROM source_files WHERE path = ?` |
| `packages/sdk/src/data/segment-store.ts:290` | `SELECT … WHERE path = ?` |

**There is no `starts_with(root)` ownership inference to remove** — the Rust
`starts_with` hits are all filename-prefix checks (`agent-`, `wf_`) and Codex
content sniffing, not root containment. The actual defect is simpler and
broader: `source_id` is *stored* but never *queried*. Two sources holding the
same absolute path share one fingerprint row, and a delete from one source
removes the other's.

This makes Phase 1.2 a larger change than "add a column to the key": every read,
write, and delete above needs a `source_id`, in both engines, plus the live
writer. `projects` already uses `PRIMARY KEY (source_id, slug)`, so there is
precedent to copy.

### 1.2 The error wire shape is two fields, not four

RFC 008 Phase 0 specifies `{ slug?, path, severity, message }` plus
`errorCount` and `errorsTruncated`. Today:

```rust
// crates/spaghetti-napi/src/orchestrate/ingest.rs:103
pub struct IngestError { pub slug: String, pub message: String }
```

```ts
// packages/sdk/src/native.ts:46
errors: Array<{ slug: string; message: string }>;
```

`slug` is required, so the RFC's "pre-identity failure has no slug" case cannot
be represented at all — which is presumably why such failures are currently
swallowed rather than reported (`claude/project_parser.rs:10` documents exactly
that). There is no `path`, no severity, and no truncation metadata.

Changing this touches the Rust struct, the generated `index.d.ts`, the
handwritten `native.ts`, the lifecycle owners, and CLI/TUI reporting — the RFC
lists them, and all five still exist.

### 1.3 There are no Codex fixtures

`crates/spaghetti-napi/fixtures/` contains `small/` and `medium/` (both
`.claude`) and `small-grok/` (`.grok`). **No Codex corpus exists.**

Phase 0 requires Codex fixtures covering official `token_count`, absent
`token_count`, total-only counts, mixed coverage, empty/internal-only records,
and live-growth tails — and Phase 3 is *entirely* a Codex token-attribution
decision that cannot begin without them. This is the single largest gap between
the RFC's assumed starting point and the tree.

There is also no fixture README; the RFC explicitly rejects "covered by medium"
as a mapping.

### 1.4 The contract marker already has a home

Phase 0 item 4 asks for a per-source ingest-contract marker. `schema.ts:445`
already defines exactly the right table:

```sql
CREATE TABLE source_materializations (
  source_id TEXT NOT NULL,
  projection TEXT NOT NULL,
  version INTEGER NOT NULL,
  completed_at INTEGER NOT NULL,
  PRIMARY KEY (source_id, projection)
);
```

A `(source_id, 'rust-ingest-contract')` row fits without a schema change. Note
the RFC's caution still applies: adding the row is not the same as marking a
source repaired.

### 1.5 musl targets are genuinely missing

`crates/spaghetti-napi/package.json` publishes six targets — darwin x64/arm64,
linux-gnu x64/arm64, windows-msvc x64/arm64. Phase 4 requires adding
`x86_64-unknown-linux-musl` and `aarch64-unknown-linux-musl`, and documenting
the glibc floor for the GNU builds. Windows-arm64 landed recently (`0e9a741`),
so the target-matrix machinery is warm.

`EngineUnavailableError` does not exist yet.

---

## 2. What is already in place

| RFC 008 need | State |
| --- | --- |
| Cross-engine diff harness | `scripts/ingest-diff.ts`, three variants wired into CI (`test:ingest-diff`, `:medium`, `:grok`) — all report zero diffs at `13ac10e` |
| Benchmark harness | `scripts/bench-ingest.ts`; CI has a "Rust ingest bench gate" job |
| Atomic source clear | `IngestEvent::ClearSourceData` with a rollback test (`writer.rs:1350`) |
| Engine selection + per-engine DBs | `resolveEngine`, `defaultDbPathForEngine`, `spag engine` — all retained, as the RFC requires |
| Schema versioning | `SCHEMA_VERSION = 15`, migration path exists |
| Claude fixtures | `small/`, `medium/` |
| Grok fixtures | `small-grok/` |

RFC 007 is done and does not block this: Plane 3 is gone, and the two RFCs never
shared code beyond `sources/types.ts`, which now carries only `sessionsDir` and
`settingsFile` alongside the Claude-specific subtrees.

---

## 3. Suggested Phase 0 order

The RFC lists Phase 0 as five items; two of them gate the rest.

1. **Codex fixtures first** (§1.3). Phase 3 is blocked without them, and Phase 0's
   baseline snapshots are not meaningful for Codex until they exist. This is
   also the item most likely to reveal that the attribution model needs a shape
   nobody predicted.
2. **Freeze the error wire shape** (§1.2) before touching parser behavior — the
   RFC is explicit that this is a contract freeze, not an implementation step.
3. Fixture README mapping each required behavior to a concrete file.
4. Baseline artifacts: diff-harness results, table/fingerprint/rollup snapshots,
   and cold/unchanged-warm/changed-warm timings on small, medium, and a real
   large corpus.
5. Add the `(source_id, 'rust-ingest-contract')` representation (§1.4) without
   marking anything repaired.

**Do not start Phase 1.2's key migration during Phase 0.** It changes production
behavior, which Phase 0's exit gate forbids ("No production behavior has
changed"), and §1.1 shows it is a wider change than the RFC's wording implies.

---

## 4. Open questions for the author

1. **Large-corpus baseline.** Phase 0 wants timings on "a real large corpus."
   The developer's `~/.claude` is ~525 MB indexed. Is that the reference corpus,
   and is its hardware the reference hardware for Phase 4's
   `max(2 × TS median, 3 s)` threshold? Those numbers are not reproducible
   across machines otherwise.
2. **Codex fixture provenance.** Codex sessions contain real prompts. Are
   fixtures synthesized, or captured and scrubbed? The RFC does not say, and it
   determines how much of Phase 0 is authoring versus collecting.
3. **`pnpm validate` drift.** Two of three validator suites fail at `211f4b1`
   and still fail — Claude Code has added on-disk fields the types do not cover
   (`settings.json` `model`, assistant `errorDetails` / `isAbortedMidStream`,
   system `choice` / `fallbackModel`, the `SendUserFile` tool, two
   `stats-cache.json` keys). CI does not run `validate`, so this is invisible
   there. Phase 0 freezes a contract against real data; worth fixing first so
   the baseline is not frozen against types known to be behind.

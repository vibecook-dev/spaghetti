# RFC 011 Claude Code adapter source map

Status: Phase 4 history/usage complete; Phase 5 delegation pack in progress on
2026-08-11

This survey defines the native inputs and semantic claims currently made by
the Rust `claude-code` adapter. It is narrower than the legacy Claude parser on
purpose: Phase 4 covers canonical transcript history, run lineage/activity,
and native usage. Sidecars and richer runtime packs remain Phase 5 work.

## Installation and source identity

The host supplies one or more Claude data roots, normally `~/.claude`. The
adapter canonicalizes each configured root and derives a binary-safe source
instance key from that canonical path. Secrets and transcript content are not
part of the instance key.

The instance declares two confined roots:

- `home`: the configured Claude data root;
- `projects`: `<home>/projects`.

Ordinary path spelling differences therefore resolve to one instance. A root
that is temporarily unavailable is a transient discovery error, not an empty
authoritative snapshot.

## Phase 4 streams

| Stream                 | Selector relative to `projects`  | Driver                                           | Authority | Scope   |
| ---------------------- | -------------------------------- | ------------------------------------------------ | --------- | ------- |
| `session-transcripts`  | `*/*.jsonl`                      | `AppendDelimitedFile` (`\n`, CRLF normalization) | Canonical | Session |
| `subagent-transcripts` | `*/*/subagents/**/agent-*.jsonl` | `AppendDelimitedFile` (`\n`, CRLF normalization) | Canonical | Run     |

Both streams use incremental byte cursors, interactive priority,
`MirrorSource` deletion, and full raw retention during shadow migration. The
common driver—not the adapter—owns framing, partial suffix retry, file
identity, prefix verification, generations, checkpoints, watcher recovery,
and scheduling.

Parent identity comes from `<project-slug>/<session-uuid>.jsonl`. Subagent
identity preserves the parent session, optional `workflows/<workflow-id>`
component, and `agent-<agent-id>.jsonl` name. Paths outside these shapes fail
object bootstrap rather than inventing an entity identity.

## Native record interpretation

Each complete JSONL record is decoded once into common facts:

- `SessionFact` with namespaced project/session identity and available cwd,
  branch, first-prompt, AI-title, and custom-title metadata;
- `MessageFact` with native type/UUID, role, timestamp quality, parent UUID,
  model, searchable text, verbatim raw JSON, and ordered structured content;
- `RunFact` for the root or subagent run, including the parent run key for a
  subagent;
- `DelegationFact` for a subagent, preserving the child run, layout-derived
  parent assertion, native child ID, execution cwd, and relation quality;
- `RunEvidenceFact::ActivityObserved` with `NativeActivity` strength;
- `UsageFact` when the record contains non-zero native usage.

Text, thinking, redacted thinking, tool calls, tool results, images, documents,
and unknown native blocks retain their order. Image/document base64 is reduced
to a BLAKE3 hash in the structured content projection; the full native record
remains available under the stream's current raw-retention policy.

A complete invalid UTF-8 or invalid JSON record becomes an `UnknownRecord`
plus a permanent diagnostic and may advance. An unterminated JSON fragment is
never passed to the decoder and cannot advance. A valid JSON record that no
longer fits the typed Claude model is still projected from its loose native
fields, with an explicit typed-projection diagnostic.

## Identity, time, lineage, and usage claims

Entity keys are namespaced by adapter ID, source instance, entity kind, and a
stable Claude-native key. A message UUID is used when present. Without one,
the adapter derives identity from session/workflow/agent plus source object,
generation, and cursor range; it never generates a random ingest UUID.

Claude's record timestamp is `NativeExact` when present. Observation and
commit times remain separate provenance. Source order is object generation
plus cursor range; callback order is not used as causality.

The adapter declares `runtime.subagents` as derived, run-granularity, and live.
The child identity is native, while the current parent assertion comes from
Claude's authoritative transcript layout and is therefore explicitly
`Layout` strength. A child can be committed before its parent exists. The
common delegation reducer retains the assertion as unresolved and resolves it
when the parent run arrives; equal-strength disagreement is preserved and
surfaced as a conflict rather than overwritten.

Adding the delegation assertion advances the Claude semantic contract to
version 2 because later facts in a subagent record receive new local ordinals.
Version-1 cursors require contract replay rather than append continuation.

The adapter declares activity only. It does not turn quiet files, missing
watch events, or filesystem nesting into completion. Subagent layout provides
parent-run lineage but no terminal evidence.

Claude assistant usage is emitted at `Message` scope with `Delta` accounting
and `NativeExact` quality. Input, output, cache-creation, and cache-read values
remain separate. The common projector stores one normalized contribution per
usage fact and updates exact/estimated aggregate buckets by
`new contribution - old contribution`. No session scan or final-message
attribution is used. `Cumulative` and `Snapshot` accounting require an
explicit counter-series/snapshot-owner contract and are rejected by the Phase
4 projector rather than being miscounted.

## Rewrite and deletion behavior

Append continuation stays in one generation. Truncation, native identity
replacement, verified-prefix mismatch, or adapter-contract replay starts a
new generation at byte zero. The common writer retracts old-generation
canonical rows, evidence, usage contributions, and audit facts in the same
transaction that projects the replacement generation and advances its cursor.

Confirmed source deletion is declared `MirrorSource`; the future observation
coordinator applies the same ownership retraction. Temporary source-root loss
does not confirm deletion.

## Remaining Phase 5 sources

The delegation pack currently uses transcript layout only. Subagent meta and
parent spawn records still need snapshot/dependency streams before they can
enrich task identity, labels, prompts, worktree paths, or stronger relation
evidence. The adapter also does not yet declare `sessions-index.json`, memory,
tool-result files, workflows, teams/config/inboxes, active PID presence,
todos, tasks, plans, file history, settings, or other sidecars. Those inputs
need replace-document, directory-snapshot, or presence streams and reviewed
capability semantics. Credentials, debug logs, telemetry, caches, and
arbitrary symlink escapes remain out of scope.

## Conformance evidence

The committed `fixtures/small/.claude` corpus is run through both the legacy
cold ingester and the new adapter/common-driver/common-projector path. The
test requires zero differences across all parent and subagent raw semantic
JSON, session IDs, native message types/IDs, timestamps, searchable text, and
all four usage fields. JSON object key order is normalized for comparison;
the new canonical row itself retains the original source bytes.

A second deterministic trace compares fresh cold backfill with one-record live
appends, forces a full generation replay, and requires identical semantic
sessions, messages, runs, observed state, and usage totals. It also checks
that old facts/contributions are retracted and that a full audit rebuild
produces the same totals as the hot-path deltas.

The Phase 5 delegation conformance trace additionally covers parent-first and
child-first arrival, late resolution, generation replay, equal-strength
conflicts, durable conflict diagnostics, and the invariant that activity plus
silence remains `Active` rather than becoming a fabricated completion.

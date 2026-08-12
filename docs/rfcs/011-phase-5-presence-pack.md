# RFC 011 Phase 5: Claude active-session presence capability pack

Status: implemented on 2026-08-11

This slice moves Claude's `~/.claude/sessions/<pid>.json` registry into the
common Rust observation engine. It records what the agent-owned presence file
proves while keeping host process probes and time-based freshness out of
durable history.

## Source and capability contract

The Claude instance now confines a `sessions` root at `<home>/sessions` and
declares one canonical `PresenceObject` stream:

- `active-sessions` selects `*.json`;
- content is included and bounded at 64 KiB;
- priority is interactive;
- consistency is snapshot replacement;
- deletion mirrors the source;
- raw retention remains full during migration.

Creation, content update, replacement, and confirmed removal are common-driver
observations. The driver now stamps a source record as `Present` or `Absent`,
so a removed file cannot be confused with a present zero-byte file. The latter
is malformed content and is preserved as unknown rather than interpreted as a
deletion.

Claude declares `runtime.presence` as native, process-presence granularity,
and live. The additive stream advances the adapter contract from 5 to 6 and
adds `claude-code-active-session-v1`; existing transcript, delegation, team,
and inbox fact identities do not shift.

## Typed fact and identity

A present, supported document emits one `PresenceFact` with:

- stable presence, session, and root-run keys;
- native session ID, PID, cwd, and exact start time;
- native kind, entrypoint, name, and status when supplied;
- update and status-update times;
- process-start marker, Claude version, peer protocol, and name source;
- bridge session and messaging socket path when supplied.

The path must be exactly `<positive-pid>.json`, and its PID must match the
payload. Required session ID and cwd values must be non-empty, and negative
epoch-millisecond values are rejected. Unsupported JSON or contract loss is
preserved as an unknown record with provenance.

Presence identity includes PID, native session ID, and the native process-start
marker. Older documents without that marker fall back to the native session
start time. A status or metadata update therefore replaces the same process
incarnation, while PID reuse or a new session start cannot silently overwrite
an older incarnation.

## Schema 23 and replacement projection

Schema 23 adds:

- `presence_assertions`, retaining each current provenance-bearing assertion;
- `canonical_presences`, exposing one deterministic current row per presence
  identity with resolution and conflict counts.

Each commit replaces the assertion owned by its source object even when the
driver generation is unchanged. Confirmed absence supplies an empty fact batch,
which retracts that object's assertion and its superseded audit fact in the
same cursor transaction. If no assertion remains, the canonical presence is
deleted.

Competing assertions are retained. Distinct payloads make the canonical row
`conflicting`, select a decisive fact by stable fact identity rather than
callback order, and publish `diagnostic.runtime.presence-conflict`. Removing
the competitor resolves the row and clears the diagnostic. Ordinary changes
publish `runtime.presence.changed` with the native status and resolution
summary.

Session and root-run references intentionally have no arrival-order foreign
key. Presence can be queried before transcript history and joins naturally
when the corresponding run appears later.

## Durable evidence versus transient assessment

The canonical row proves only that Claude's registry object was present at the
committed source revision. This slice deliberately does not:

- call `kill(0)` or otherwise probe the host process during projection;
- persist `ProcessMissing`, `LikelyActive`, `Stale`, or another assessment;
- translate native `idle` or `working` strings into a durable run lifecycle;
- create transcript history or `observed_run_states` rows;
- infer success, failure, cancellation, waiting, or completion from removal or
  silence.

A future optional liveness provider may combine the durable presence row with
`now` and a PID/process-start check. Per RFC 011, that result belongs in a
volatile assessment cache with an evaluation time and expiry; it never advances
the Claude cursor or rewrites terminal history.

## Conformance evidence

The tests cover:

- the confined root, selector, bounds, contract version, and capability
  quality;
- strict numeric path context and PID/payload agreement;
- complete native field preservation and stable identity across status edits;
- explicit distinction between confirmed absence and malformed empty content;
- same-generation replacement and audit-fact cleanup;
- presence-before-history late correlation;
- absence retraction while a quiet transcript/run remains non-terminal;
- retained competing assertions, deterministic reduction, durable conflict
  publication, and resolution after retraction;
- Rust/TypeScript schema parity and current TypeScript registry-field coverage.

The Rust crate suite contains 349 passing tests after this slice. Repository
type checks, builds, architecture ratchets, and ingest differential matrices
remain the release gate for the commit.

## Remaining Phase 5 work

Tasks, todos, plans, file-history artifacts, workflows, session-index metadata,
project memory, and persisted tool results are now implemented in adjacent
Phase 5 packs. Settings and other reviewed packs remain. The observation
coordinator, optional volatile process assessment API, and production live
cutover are also still required for the Phase 5 exit gate.

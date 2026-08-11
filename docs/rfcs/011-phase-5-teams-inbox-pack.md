# RFC 011 Phase 5: Claude teams and inbox capability pack

Status: implemented on 2026-08-11

This slice adds Claude's authoritative team configuration and per-recipient
inbox documents to the common Rust observation engine. It preserves their
useful native semantics without treating configuration, tmux layout, or quiet
files as execution lifecycle evidence.

## Source and capability contract

The Claude instance now confines a `teams` root at `<home>/teams` and declares
two canonical `ReplaceDocument` streams:

- `team-configs` selects `*/config.json`, bounds a document at 1 MiB and a
  decoded member list at 256;
- `team-inboxes` selects `*/inboxes/*.json`, bounds a document at 4 MiB and a
  decoded message list at 4,096.

Both streams use snapshot-replace consistency, foreground-repair priority,
`MirrorSource` deletion, and full raw retention during migration. Stable reads,
revision cursors, retries, quarantine, scheduling, and watcher recovery remain
common-driver responsibilities.

Claude declares `runtime.teams` at native/team/live quality and
`runtime.team_inbox` at native/message/live quality. The additive streams bump
the Claude adapter contract from 4 to 5 but do not shift any existing
transcript or metadata fact identity.

## Typed snapshots and identity

One config document emits one `TeamSnapshotFact`. It preserves native team
name/description/creation time, lead agent and session identity, and the full
ordered member snapshot. Member fields include native agent ID and name, agent
type, model, prompt, color, plan-mode requirement, join time, tmux pane, cwd,
subscriptions, and backend type.

The team key uses the native directory ID. A member key is scoped by native
team ID plus native member name, allowing inbox senders and recipients to join
before or without a config snapshot. The lead session uses the common session
key namespace, but config presence does not assert that session exists or is
active.

One recipient file emits one `TeamInboxSnapshotFact` with ordered message
children. Native `msg_id`, `msgV`, and `type` are retained when present. A
non-empty `msg_id` is the preferred message identity. Legacy entries derive a
deterministic key from team, recipient, sender, timestamp, text, and an
occurrence ordinal for exact duplicates. Read state is not part of identity,
so a read edit updates the same message.

Unsupported JSON, missing required native fields, duplicate member names, or
duplicate native message IDs become preserved unknown records rather than
partially trusted snapshots. No document is silently truncated at the decoded
cardinality bounds.

## Schema 22 and replacement projection

Schema 22 adds provenance-bearing snapshot and normalized child assertions:

- `team_snapshot_assertions` and `team_member_assertions`;
- `team_inbox_snapshot_assertions` and
  `team_inbox_message_assertions`.

The current query-facing projection is stored in:

- `canonical_teams` and `canonical_team_members`;
- `canonical_team_inboxes` and `canonical_team_inbox_messages`.

Every document commit replaces all prior assertions owned by that source
object even when its common generation is unchanged. The reducer retracts
removed children, selects a deterministic decisive fact, retains competing
assertions, records resolved/conflicting status and counts, and writes its
outbox changes in the same transaction as the source cursor.

Team and inbox parents publish `runtime.team.changed` and
`runtime.team-inbox.changed`. Normalized children publish
`runtime.team-member.changed` and `runtime.team-inbox-message.changed`.
Competing snapshots or children publish corresponding
`diagnostic.runtime.*-conflict` topics; removing the competitor clears the
diagnostic.

An inbox has no foreign-key dependency on a canonical config. Inbox-only team
directories therefore remain visible. An empty JSON array is an authoritative
inbox with zero messages, while confirmed source deletion retracts the inbox
itself. This distinction is preserved through audit facts, canonical rows,
and outbox operations.

## Explicit non-claims

Team membership proves configuration membership only. The projector does not:

- convert `tmuxPaneId` or `backendType` into process presence;
- infer activity from config or inbox traffic;
- infer waiting, success, failure, cancellation, or completion from silence;
- discard an inbox because its config is absent or malformed.

Those lifecycle claims require separate presence or native event evidence with
their own capability declarations. The adjacent presence pack now supplies
registry-object evidence without turning it into lifecycle completion.

## Conformance evidence

The tests cover:

- declarative roots, selectors, bounds, contract version, and capability
  quality;
- native config fields, lead/member joins, and absence of run evidence;
- native message IDs plus legacy identity stability across read edits;
- preserved-unknown behavior for unsupported documents;
- same-generation config replacement and removed-member retraction;
- orphan inbox projection, read updates, removed messages, empty arrays, and
  file deletion;
- retained competing config assertions, deterministic canonical status,
  durable conflict publication, and resolution after retraction;
- Rust/TypeScript schema parity and wipe/rebuild coverage.

The Rust crate suite contains 343 passing tests after this slice. Repository
type checks, builds, architecture ratchets, and ingest differential matrices
remain the release gate for the commit.

## Remaining Phase 5 work

Active-session presence is now implemented in the adjacent Phase 5 pack. The
remaining source families are reviewed tasks, plans, todos, file-history,
workflow, settings, and artifacts. The observation coordinator and production
live cutover are still required for the Phase 5 exit gate.

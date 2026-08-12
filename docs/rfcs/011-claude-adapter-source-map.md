# RFC 011 Claude Code adapter source map

Status: Phase 4 history/usage complete; Phase 5 delegation, native metadata,
parent-spawn correlation, team/config/inbox snapshots, active-session
presence, task/todo/plan snapshots, and file-history artifacts implemented on
2026-08-11

This survey defines the native inputs and semantic claims currently made by
the Rust `claude-code` adapter. It is narrower than the legacy Claude parser on
purpose: Phase 4 covers canonical transcript history, run lineage/activity,
and native usage. Phase 5 now adds delegation joins, authoritative team and
inbox snapshots, native active-session registry presence, replaceable task,
todo, and plan documents, and joined file-history metadata/content while
richer runtime packs remain in progress.

## Installation and source identity

The host supplies one or more Claude data roots, normally `~/.claude`. The
adapter canonicalizes each configured root and derives a binary-safe source
instance key from that canonical path. Secrets and transcript content are not
part of the instance key.

The instance declares four confined roots:

- `home`: the configured Claude data root;
- `projects`: `<home>/projects`;
- `teams`: `<home>/teams`;
- `sessions`: `<home>/sessions`.

Ordinary path spelling differences therefore resolve to one instance. A root
that is temporarily unavailable is a transient discovery error, not an empty
authoritative snapshot.

## Implemented streams

| Stream                 | Root       | Selector                             | Driver                                           | Authority    | Scope      |
| ---------------------- | ---------- | ------------------------------------ | ------------------------------------------------ | ------------ | ---------- |
| `session-transcripts`  | `projects` | `*/*.jsonl`                          | `AppendDelimitedFile` (`\n`, CRLF normalization) | Canonical    | Session    |
| `subagent-transcripts` | `projects` | `*/*/subagents/**/agent-*.jsonl`     | `AppendDelimitedFile` (`\n`, CRLF normalization) | Canonical    | Run        |
| `subagent-metadata`    | `projects` | `*/*/subagents/**/agent-*.meta.json` | `ReplaceDocument` (64 KiB bound)                 | Supplemental | Run        |
| `team-configs`         | `teams`    | `*/config.json`                      | `ReplaceDocument` (1 MiB bound)                  | Canonical    | Team       |
| `team-inboxes`         | `teams`    | `*/inboxes/*.json`                   | `ReplaceDocument` (4 MiB bound)                  | Canonical    | Team inbox |
| `active-sessions`      | `sessions` | `*.json`                             | `PresenceObject` (64 KiB content bound)          | Canonical    | Presence   |
| `todo-snapshots`       | `home`     | `todos/*-agent-*.json`               | `ReplaceDocument` (1 MiB bound)                  | Canonical    | Task list  |
| `task-items`           | `home`     | `tasks/*/*.json`                     | `ReplaceDocument` (256 KiB bound)                | Canonical    | Task       |
| `plan-documents`       | `home`     | `plans/*.md`                         | `ReplaceDocument` (4 MiB bound)                  | Canonical    | Plan       |
| `file-history-blobs`   | `home`     | `file-history/*/*@v*`                | `ReplaceDocument` (1 MiB bound)                  | Canonical    | Artifact   |

Transcript streams use incremental byte cursors and interactive priority. The
team/metadata and artifact document streams use snapshot-replace consistency
and foreground-repair priority. The presence, task, todo, and plan streams use
snapshot replacement and interactive priority. All ten use `MirrorSource`
deletion and full raw retention during shadow migration. Common drivers—not
the adapter—own framing or stable reads, file identity,
generations/revisions, checkpoints, watcher recovery, and scheduling.

Parent identity comes from `<project-slug>/<session-uuid>.jsonl`. Subagent
identity preserves the parent session, optional `workflows/<workflow-id>`
component, and `agent-<agent-id>.jsonl` name. Paths outside these shapes fail
object bootstrap rather than inventing an entity identity.

The metadata stream derives the identical child key from the sibling
`agent-<agent-id>.meta.json` path, including workflow scope when present.
Team config identity comes from `<team>/config.json`; inbox identity comes from
`<team>/inboxes/<recipient>.json`. Paths outside these exact shapes fail object
bootstrap. Active-session identity starts from exactly `<positive-pid>.json`;
nested, non-numeric, or zero-PID names fail bootstrap.

Todo, task, and plan paths are rooted at `home` but confined to exact
`todos/<session>-agent-<agent>.json`,
`tasks/<collection>/<canonical-positive-id>.json`, and `plans/<slug>.md`
shapes. A task payload ID must match its path ID.

Artifact-content paths are confined to exact
`file-history/<session-uuid>/<lowercase-hex>@v<canonical-positive-version>`
shapes. Session, native backup name, hash, and version come from that path;
paths that only resemble the shape fail bootstrap.

## Native record interpretation

Each complete native record or document is decoded once into common facts:

- `SessionFact` with namespaced project/session identity and available cwd,
  branch, first-prompt, AI-title, and custom-title metadata;
- `MessageFact` with native type/UUID, role, timestamp quality, parent UUID,
  model, searchable text, verbatim raw JSON, and ordered structured content;
- `RunFact` for the root or subagent run, including the parent run key for a
  subagent;
- `DelegationFact` for a subagent, preserving the child run, layout-derived
  parent assertion, native child ID, execution cwd, and relation quality;
- `DelegationMetadataFact` for a metadata sidecar, preserving native free-form
  agent type, description, name, spawn depth, worktree path, and optional
  spawning tool-use ID without making a parent assertion;
- `DelegationSpawnFact` for each parent `Task` or `Agent` tool call, preserving
  the parent run/message, session, native tool-use ID, requested label, prompt,
  and agent type without asserting that a child exists;
- `TeamSnapshotFact` for each config, preserving native team/lead/session
  identity plus the complete bounded member snapshot and native member fields;
- `TeamInboxSnapshotFact` for each recipient file, preserving the complete
  ordered message snapshot, native message IDs/versions when present, read
  state, and qualified native timestamps;
- `PresenceFact` for each present active-session registry object, preserving
  session/run relation, process-incarnation identity, PID, cwd, native status,
  timestamps, version/protocol, bridge, and messaging-socket fields;
- `TaskSnapshotFact` for each complete todo list or numbered task item,
  preserving explicit coverage, scope only when native, status, ownership,
  dependency fields, and stable item identity;
- `PlanSnapshotFact` for each plan markdown document, preserving native slug,
  heading-derived title, exact content, and byte size;
- `ArtifactMetadataSnapshotFact` for each transcript file-history checkpoint or
  delta, preserving session/message identity, observation kind, tracked paths,
  optional real parent directories, versions, backup times, and explicit
  content-expected versus not-captured state;
- `ArtifactContentFact` for each present backup blob, preserving the native
  backup name, file hash, version, exact bytes, and byte size;
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
The child identity is native. Transcript layout supplies a durable `Layout`
parent assertion, while matching a metadata sidecar to a parent spawn by the
same non-empty native tool-use ID and session supplies a `NativeExplicit`
candidate. A child, sidecar, or spawn can be committed first. The common
delegation reducer retains unresolved evidence, re-runs on either half's
arrival or deletion, and preserves equal-strength disagreement as a conflict
rather than overwriting it.

Adding the delegation assertion advances the Claude semantic contract to
version 2 because later facts in a subagent record receive new local ordinals.
Version-1 cursors require contract replay rather than append continuation.
Adding the declared metadata stream and fact advances the adapter contract to
version 3. Unlike the version-2 change, this additive stream does not alter
existing transcript fact identities or meanings. The coordinator's future
per-object decoder-version wiring should therefore avoid replaying transcript
objects solely because the metadata stream was added.

Parent `Task`/`Agent` spawn output advances the adapter contract to version 4.
These facts are appended after all pre-existing transcript facts, preserving
their identities, but historical parent transcript objects require targeted
contract replay to materialize the new spawn assertions.

The additive team config and inbox streams advance the adapter contract to
version 5. They do not alter transcript or metadata fact identity, so existing
objects in those streams do not require replay solely for this addition.

The additive active-session stream advances the adapter contract to version 6
and declares `claude-code-active-session-v1`. Existing transcript,
delegation, team, and inbox fact identities are unchanged.

The additive todo, task-item, and plan streams advance the adapter contract to
version 7 and declare `claude-code-todo-v1`,
`claude-code-task-item-v1`, and `claude-code-plan-v1`. Existing facts in all
earlier streams keep their identity and meaning.

The file-history capability advances the adapter contract to version 8 and
declares `claude-code-file-history-v1`. The blob stream is additive. Metadata
facts are appended after earlier facts in matching transcript records, so
existing fact IDs remain stable; historical file-history records require
targeted replay to materialize the new assertions.

Team identity comes from the native directory name. Member identity is scoped
by team plus native member name, and an inbox is scoped by team plus recipient.
Inbox messages prefer non-empty native `msg_id`. Older messages derive a
deterministic fingerprint from sender, timestamp, and text plus an occurrence
ordinal for exact duplicates. `read` is intentionally excluded, so marking a
legacy message read updates the same entity instead of inventing a new one.

Claude declares `runtime.teams` and `runtime.team_inbox` as native and live.
Configuration membership is not activity evidence: tmux pane IDs, backend
type, and config presence never create run state or completion. An inbox may
remain canonical without a matching config, preserving orphan or partially
written native state.

Claude declares `runtime.presence` as native and live at process-presence
granularity. The payload PID must equal the path PID. Presence identity combines
PID, native session ID, and native process-start marker, falling back to native
session start time for older documents. Updates therefore replace one process
incarnation while PID reuse remains distinct. Session and root-run keys permit
late joins when the transcript has not arrived yet.

The common driver stamps records as present or absent. Confirmed absence emits
no presence fact and retracts the prior assertion; a present zero-byte file is
instead preserved as malformed unknown content. The durable row proves registry
presence only. Native status strings are retained, but neither they nor a host
PID probe create durable run state, process-liveness history, or completion.

Claude declares `runtime.tasks` as native and live at task granularity. A todo
file is a complete collection snapshot; numbered task JSON is one independently
replaceable item document. Todo item identity excludes status so ordinary
status edits update the same item. Numbered tasks use their native collection
and item IDs. A task-directory name is not assumed to be a session or team
because the native corpus uses both and other scopes. Plans use their native
slug and remain standalone until separate evidence supplies a trustworthy
relation. Task completion is never run completion evidence.

Claude declares `runtime.artifacts` as native and live at artifact
granularity. Transcript metadata and backup blobs join only by native session
and backup name. A null backup filename is explicit non-capture, distinct from
a named backup whose blob is missing. Content remains arbitrary bytes. The
native formats prove session scope but do not prove that any run produced,
created, or edited the tracked file.

The transcript decoder declares activity only. Neither it nor the presence
pack turns quiet files, missing watch events, or filesystem nesting into
completion. Subagent layout provides parent-run lineage but no terminal
evidence.

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

Each committed metadata revision replaces every prior metadata assertion
owned by that source object, even when the file generation is unchanged. An
empty confirmed replacement retracts the assertion, audit fact, canonical
metadata row, and publishes a delete change atomically. Duplicate sidecars
with different native values remain as competing assertions and produce a
durable conflict diagnostic.

Spawn assertions follow transcript generation ownership. Append continuation
accumulates native calls, while truncation, rewrite, or contract replay
retracts the replaced generation. If either a matching metadata snapshot or
spawn assertion disappears, canonical lineage falls back to the strongest
remaining durable relation in the same transaction.

Every committed team config or inbox revision replaces all assertions owned
by that source object, including same-generation edits. Removed members or
messages retract in the same commit. An empty inbox array retains the inbox
with zero messages; confirmed file deletion removes the inbox itself.
Duplicate authoritative objects remain competing assertions, use deterministic
fact identity rather than callback order to select a canonical view, and
publish durable conflict diagnostics.

Every committed active-session revision replaces the assertion owned by that
source object, including same-generation status changes. Confirmed removal
retracts the canonical row without changing a quiet transcript/run to a
terminal state. Competing presence assertions are retained and diagnosed;
removing the competitor resolves the deterministic canonical view.

Every committed todo, task-item, or plan revision replaces the assertion owned
by that source object. Complete todo replacements retract missing children;
one task-item removal affects only that document's item. Agreeing item
documents merge into one collection, while distinct assertions for the same
task or plan remain conflicting and diagnosed. Superseded audit facts retract
only after canonical foreign keys move to the current decisive assertions.

File-history checkpoints and deltas accumulate as historical metadata within
one transcript append generation; a later checkpoint does not retract older
artifact observations. Transcript generation replacement retracts the old
metadata generation. Each backup blob is independently replaceable within its
current generation, and confirmed deletion retracts its content assertion.
Metadata-first and content-first arrival converge to `captured`,
`not_captured`, `missing_content`, or `orphan_content`. Competing assertions or
cross-half identity disagreement remain durable and diagnosed.

## Remaining Phase 5 sources

Legacy tool-result text that only mentions an agent ID is not used for native
correlation. A future compatibility fallback must be classified
`NativeIndirect`, not explicit. The adapter also does not yet declare
`sessions-index.json`, memory, tool-result files, workflows, settings, or other
sidecars. Those inputs need replace-document,
directory-snapshot, or other reviewed stream and capability semantics.
Credentials, debug logs, telemetry, caches, and arbitrary symlink escapes
remain out of scope.

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

The metadata trace covers sidecar-first and transcript-first arrival,
same-generation replacement, confirmed deletion and recreation, stable child
identity across workflow paths, and competing native sidecars whose ambiguity
is retained and diagnosed.

The native spawn trace covers parent `Task`/`Agent` decoding, metadata-first
and transcript-first joins, two-fact decisive provenance, sidecar deletion,
spawn generation retraction, layout fallback, and conflicting explicit parent
matches. It also requires that spawn correlation creates no observed terminal
state.

The team/inbox trace covers native path/context decoding, bounded document
preservation, stable native and legacy message identity, same-generation
replacement, removed-child retraction, the distinction between `[]` and file
deletion, orphan inboxes, competing config assertions, diagnostic clearing,
and the invariant that config membership creates no observed run state.

The active-presence trace covers strict path/payload PID identity, full native
field preservation, present-empty versus absent distinction, same-generation
replacement, late transcript/run correlation, confirmed removal, competing
assertions, diagnostic clearing, and the invariant that registry removal does
not invent terminal run state.

The task/todo/plan trace covers strict paths and payload identity, complete
versus item-document coverage, status-stable identity, future native status
preservation, ambiguous-scope non-attribution, Markdown title fallback,
same-generation replacement, removed-child and file retraction, item merging,
late session correlation, deterministic conflicts and clearing, audit cleanup,
and the invariant that completed tasks do not create terminal run state.

The file-history trace covers strict backup paths, checkpoint and delta
metadata, explicit non-capture, arbitrary blob bytes, metadata-first and
content-first joins, all four content states, same-generation blob replacement,
transcript-generation and confirmed-deletion retraction, competing assertions,
cross-half conflicts and clearing, audit cleanup, and the invariant that
artifact facts add no lifecycle evidence beyond a transcript record's existing
generic activity observation.

# RFC 011 Claude Code adapter source map

Status: Phase 4 history/usage complete; Phase 5 delegation, native metadata,
parent-spawn correlation, team/config/inbox snapshots, active-session
presence, task/todo/plan snapshots, file-history artifacts, workflow
summaries/journals, session-index metadata, project-memory documents,
persisted tool-result text, and redacted interpretation settings implemented
on 2026-08-11

This survey defines the native inputs and semantic claims currently made by
the Rust `claude-code` adapter. It is narrower than the legacy Claude parser on
purpose: Phase 4 covers canonical transcript history, run lineage/activity,
and native usage. Phase 5 now adds delegation joins, authoritative team and
inbox snapshots, native active-session registry presence, replaceable task,
todo, and plan documents, joined file-history metadata/content, and native
workflow containers/member events. Phase 5 also preserves replaceable
session-index metadata without treating the index as transcript history, while
preserving each native project-memory Markdown document independently. It also
retains persisted tool-result text as supplemental output and correlates it to
typed transcript blocks without fabricating history. Root global/local
settings now provide redacted configuration evidence without exporting
environment values or executable command bodies. Richer runtime packs remain
in progress.

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

| Stream                     | Root       | Selector                                  | Driver                                           | Authority    | Scope       |
| -------------------------- | ---------- | ----------------------------------------- | ------------------------------------------------ | ------------ | ----------- |
| `session-transcripts`      | `projects` | `*/*.jsonl`                               | `AppendDelimitedFile` (`\n`, CRLF normalization) | Canonical    | Session     |
| `subagent-transcripts`     | `projects` | `*/*/subagents/**/agent-*.jsonl`          | `AppendDelimitedFile` (`\n`, CRLF normalization) | Canonical    | Run         |
| `subagent-metadata`        | `projects` | `*/*/subagents/**/agent-*.meta.json`      | `ReplaceDocument` (64 KiB bound)                 | Supplemental | Run         |
| `team-configs`             | `teams`    | `*/config.json`                           | `ReplaceDocument` (1 MiB bound)                  | Canonical    | Team        |
| `team-inboxes`             | `teams`    | `*/inboxes/*.json`                        | `ReplaceDocument` (4 MiB bound)                  | Canonical    | Team inbox  |
| `active-sessions`          | `sessions` | `*.json`                                  | `PresenceObject` (64 KiB content bound)          | Canonical    | Presence    |
| `todo-snapshots`           | `home`     | `todos/*-agent-*.json`                    | `ReplaceDocument` (1 MiB bound)                  | Canonical    | Task list   |
| `task-items`               | `home`     | `tasks/*/*.json`                          | `ReplaceDocument` (256 KiB bound)                | Canonical    | Task        |
| `plan-documents`           | `home`     | `plans/*.md`                              | `ReplaceDocument` (4 MiB bound)                  | Canonical    | Plan        |
| `file-history-blobs`       | `home`     | `file-history/*/*@v*`                     | `ReplaceDocument` (1 MiB bound)                  | Canonical    | Artifact    |
| `workflow-runs`            | `projects` | `*/*/workflows/wf_*.json`                 | `ReplaceDocument` (1 MiB bound)                  | Canonical    | Workflow    |
| `workflow-journals`        | `projects` | `*/*/subagents/workflows/*/journal.jsonl` | `AppendDelimitedFile` (`\n`, CRLF normalization) | Canonical    | Member      |
| `session-indexes`          | `projects` | `*/sessions-index.json`                   | `ReplaceDocument` (1 MiB bound)                  | Supplemental | Project     |
| `project-memory-documents` | `projects` | `*/memory/*.md`                           | `ReplaceDocument` (1 MiB bound)                  | Canonical    | Project     |
| `persisted-tool-results`   | `projects` | `*/*/tool-results/*.txt`                  | `ReplaceDocument` (16 MiB bound)                 | Supplemental | Tool result |
| `interpretation-settings`  | `home`     | `settings.json`, `settings.local.json`    | `ReplaceDocument` (1 MiB bound)                  | Canonical    | Instance    |

Transcript streams use incremental byte cursors and interactive priority. The
team/metadata, artifact, and workflow-run document streams use snapshot-replace
consistency and foreground-repair priority. The presence, task, todo, and plan
streams use snapshot replacement and interactive priority. Workflow journals
use incremental byte cursors and interactive priority. Session indexes use
snapshot replacement and interactive priority. Project-memory documents also
use snapshot replacement and interactive priority. Persisted tool results and
interpretation settings use snapshot replacement and interactive priority.
All sixteen use `MirrorSource` deletion. The settings stream uses hash-only raw
retention; the other streams retain full raw input during shadow migration.
Common drivers—not the adapter—own framing or stable reads, file identity,
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

Workflow paths are confined to exact
`<project>/<session-uuid>/workflows/<wf_id>.json` and
`<project>/<session-uuid>/subagents/workflows/<wf_id>/journal.jsonl` shapes.
Run IDs must start with `wf_`, and a run payload ID must match its filename.

Session-index paths are confined to exact
`<project>/sessions-index.json` shapes. The project directory supplies the
source object identity; the document retains its native `projectPath` and each
entry's transcript path as metadata rather than using either to escape the
configured root.

Project-memory paths are confined to exact
`<project>/memory/<non-empty-name>.md` shapes. Each immediate Markdown file is
an independent source object. `MEMORY.md` is classified as the native index,
but sibling topic documents have equal content authority; nested and
non-Markdown lookalikes fail bootstrap.

Persisted tool-result paths are confined to exact
`<project>/<session-uuid>/tool-results/<non-empty-id>.txt` shapes. Filename
stems are opaque native identifiers: model-style `toolu_*`, short generated,
and hook stdout IDs are all valid. Immediate JSON/PDF siblings and nested
rendered descendants are excluded for separate binary-artifact semantics.

Interpretation-settings paths are confined to exactly root `settings.json`
and `settings.local.json`. Backup, renamed, nested, and managed-policy
lookalikes fail bootstrap. The adapter does not search project directories or
infer command-line/managed layers from these two source-instance documents.

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
- `WorkflowSnapshotFact` for each run summary, preserving normalized and
  native container status, script/model/count/timing fields, and the complete
  forward-compatible JSON snapshot;
- `WorkflowMemberEventFact` for each valid journal start or result, preserving
  workflow/member/event identity, result JSON, and the workflow-aware child
  run key used by nested subagent transcripts;
- `SessionIndexSnapshotFact` for each complete index, preserving its native
  version, project path, ordered entries, optional summary/original path,
  timestamps, counts, sidechain marker, and complete forward-compatible JSON;
- `ProjectMemoryDocumentFact` for each present Markdown file, preserving
  project/document identity, native path, index classification, heading-derived
  title, exact content, and byte size;
- `PersistedToolResultFact` for each valid immediate text sidecar, preserving
  result/session/project identity, native tool ID and path, exact content, and
  byte size without creating transcript or runtime facts;
- `InterpretationSettingsFact` for each global/local root document, preserving
  current document health plus allowlisted model, behavior, permission,
  plugin, and redacted hook-declaration metadata;
- `RunEvidenceFact::ActivityObserved` with `NativeActivity` strength;
- `UsageFact` when the record contains non-zero native usage.

Text, thinking, redacted thinking, tool calls, tool results, images, documents,
and unknown native blocks retain their order. Image/document base64 is reduced
to a BLAKE3 hash in the structured content projection; the full native record
remains available under the stream's current raw-retention policy.

The settings fact intentionally excludes environment values, hook matcher
text and executable bodies, status-line commands, marketplace paths, UI
preferences, and unknown fields. Invalid settings emit a redacted invalid
health fact rather than copying raw bytes into the typed audit store. Global
and local scalar values use local precedence; array values concatenate and
de-duplicate; plugin booleans override per key; hook declaration counts add by
event. Invalid or conflicting current documents keep the effective view
explicitly unhealthy rather than silently serving stale configuration.

For content-bearing streams, a complete invalid UTF-8 or invalid JSON record
becomes an `UnknownRecord` plus a permanent diagnostic and may advance. The
interpretation-settings stream instead emits its redacted invalid health fact,
so secret settings bytes cannot enter the typed audit payload. An unterminated
JSON fragment is never passed to a line decoder and cannot advance. A valid
JSON record that no longer fits the typed Claude model is still projected from
its loose native fields, with an explicit typed-projection diagnostic.

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

The additive workflow run and journal streams advance the adapter contract to
version 9 and declare `claude-code-workflow-v1`. Earlier stream fact identities
and meanings remain unchanged.

The additive session-index stream advances the adapter contract to version 10
and declares `claude-code-session-index-v1`. Earlier stream fact identities and
meanings remain unchanged.

The additive project-memory stream advances the adapter contract to version 11
and declares `claude-code-project-memory-v1`. Earlier stream fact identities and
meanings remain unchanged.

The additive persisted-tool-result stream advances the adapter contract to
version 12 and declares `claude-code-persisted-tool-result-v1`. Earlier stream
fact identities and meanings remain unchanged.

The additive interpretation-settings stream advances the adapter contract to
version 13 and declares `claude-code-interpretation-settings-v1`. Earlier
stream fact identities and meanings remain unchanged.

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

Claude declares `runtime.workflows` as native and eventually live at workflow
granularity. Workflow summaries may settle separately from append journals.
Container status applies only to the workflow. Journal starts/results prove
membership and native event observation; neither workflow completion nor a
result payload determines child-run terminal state.

Claude declares session-index data as supplemental project metadata. A valid
entry can join a canonical transcript by native session identity, but it never
creates a `Session`, `Message`, `Run`, activity observation, or lifecycle
evidence. Index files can outlive their referenced transcripts, and native
`created`/`modified` values are retained even when their ordering is surprising.

Claude declares `context.project_memory` as native and live at memory-document
granularity. `MEMORY.md` is a native index convention, not a complete snapshot
of the directory. Sibling topic files remain independently queryable. Markdown
links are retained as content and do not assert joins, sessions, runs, activity,
or lifecycle evidence.

Claude declares `history.persisted_tool_results` as native and live at
persisted-tool-result granularity. The document is supplemental full output,
not a replacement for an inline transcript block. The common projector joins
only on the stable session key plus typed native tool ID and records
`unlinked`, `call_only`, `result_only`, `linked`, or `ambiguous` state. Text
that happens to mention an agent ID is not delegation evidence.

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

Each workflow run summary is independently replaceable within its source
object. Workflow journal events accumulate within one append generation, while
a generation rewrite retracts the old event history. Summary-first and
journal-first arrival converge. Membership count disagreement remains explicit
but is not a conflict; competing snapshots/events and cross-half identity
disagreement are retained and diagnosed.

Each committed session-index revision replaces the complete assertion owned by
that source object. Missing entries retract in the same commit, while an empty
entry array retains the project index itself. Multiple complete snapshots for
one project compete deterministically; omission from a competing snapshot is
also disagreement. Index-first and transcript-first arrival converge, and a
transcript found under a different project remains an explicit join conflict.
Confirmed index deletion retracts its assertions without deleting transcript
history.

Every project-memory file is independently replaceable. Same-generation edits
replace that source object's assertion, and confirmed deletion retracts only
that document. Duplicate objects for the same project/path remain competing
assertions with deterministic selection and durable diagnostics. A present
zero-byte document is distinct from confirmed absence. Superseded audit facts
retract after canonical foreign keys move to the current assertion.

Every persisted tool-result file is independently replaceable. Same-generation
edits replace that source object's assertion, and confirmed deletion retracts
only that output. Agreeing duplicate objects increase assertion count without
a content conflict; byte-different outputs remain competing and diagnosed.
Transcript-first and sidecar-first arrival converge through an indexed common
tool-reference projection. Transcript generation replacement refreshes all
affected joins in the same transaction.

Every global/local settings revision replaces the assertion owned by that
source object. Confirmed deletion retracts only that layer. Agreeing duplicate
assertions increase provenance count, while normalized disagreement, validity
disagreement, and byte-distinct invalid payloads remain conflicting and
diagnosed. The effective instance row re-reduces in the same transaction.

## Remaining Phase 5 sources

Persisted tool-result text that only mentions an agent ID is not used for
native delegation correlation. A future compatibility fallback must be
classified `NativeIndirect`, not explicit. Other remaining sidecars need
replace-document, directory-snapshot, or other reviewed stream and capability
semantics.
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

The workflow trace covers strict run/journal paths, full snapshot preservation,
container-status normalization, exact nested-child correlation, journal-first
late joins, started/result accumulation, summary and journal retraction,
membership count mismatch, deterministic conflicts and clearing, audit cleanup,
and the invariant that workflow completion does not complete a child run.

The session-index trace covers strict paths and version/UUID bounds, complete
native snapshot preservation, absent optional summaries and paths, index-first
and transcript-first joins, empty replacement and confirmed deletion,
competing complete snapshots including omission disagreement, cross-project
identity/join conflicts and clearing, audit cleanup, and the invariant that
index metadata never fabricates transcript history or lifecycle evidence.

The project-memory trace covers exact immediate Markdown paths, index/topic
classification, UTF-8 and zero-byte semantics, exact content/title retention,
independent same-generation replacement and deletion, deterministic conflicts
and clearing, audit cleanup, and the invariant that memory content creates no
history or runtime evidence.

The persisted-tool-result trace covers exact immediate text paths across all
observed ID families, UTF-8 and empty/absent semantics, exact content retention,
sidecar-first and transcript-first correlation, every explicit join state,
same-generation and transcript-generation replacement, agreeing and competing
assertions, duplicate-block ambiguity, diagnostic clearing, audit cleanup, and
the invariant that a sidecar creates no transcript or lifecycle evidence.

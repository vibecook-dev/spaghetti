# RFC 011 Phase 5: Claude tasks, todos, and plans capability pack

Status: implemented on 2026-08-11

This slice moves Claude's replaceable todo lists, numbered task documents, and
plan markdown files into the common Rust observation engine. It preserves the
different ownership boundaries of those formats and avoids treating task
completion as execution completion.

## Source and capability contract

The existing confined `home` root now declares three canonical
`ReplaceDocument` streams:

- `todo-snapshots` selects `todos/*-agent-*.json` and bounds each document at
  1 MiB and 4,096 decoded items;
- `task-items` selects `tasks/*/*.json` and bounds each numbered item document
  at 256 KiB;
- `plan-documents` selects `plans/*.md` and bounds each document at 4 MiB.

All three use interactive priority, snapshot-replace consistency,
`MirrorSource` deletion, and full raw retention during migration. Stable
reads, revision cursors, retry/quarantine behavior, scheduling, watcher
recovery, and confirmed deletion remain common-driver responsibilities.

Claude declares `runtime.tasks` as native, task-granularity, and live. The
additive streams advance the adapter contract from 6 to 7 and declare
`claude-code-todo-v1`, `claude-code-task-item-v1`, and
`claude-code-plan-v1`. Existing transcript, delegation, team, inbox, and
presence fact identities are unchanged.

## Typed facts and identity

Todo files emit one `TaskSnapshotFact` with `Complete` coverage. The path must
be exactly `todos/<session>-agent-<agent>.json`. It supplies a stable session
relation; a root-run relation is supplied only when the native agent ID equals
the native session ID. A non-root agent ID is retained without fabricating a
workflow-scoped subagent run.

Todo items have no native item ID. Their task identity uses the collection,
content fingerprint, and an occurrence ordinal for exact duplicate content.
Status is excluded from identity, so a pending/in-progress/completed edit
updates the same task. Content is retained as the subject, `activeForm` is
preserved, and future non-empty status strings remain queryable as `Other`
rather than invalidating the document.

Numbered task files emit one `TaskSnapshotFact` with `ItemDocument` coverage
and one `TaskItemSnapshot`. The path must be exactly
`tasks/<collection>/<canonical-positive-id>.json`, and the payload `id` must
match the file name. Native subject, description, active form, owner, status,
`blocks`, and `blockedBy` fields are retained. Task identity uses collection
plus native item ID, so edits replace the same item.

A task-directory name can be a session UUID, team name, or another Claude
scope. This pack deliberately leaves its session/run/team relation unset
instead of guessing from spelling. A later native relation fact can promote
that association without changing task identity.

Plan files emit one `PlanSnapshotFact` keyed by native slug. The first Markdown
level-one heading is the title; a headless plan falls back to the slug. Content
and exact byte size are retained. Plans remain independently queryable until a
separate transcript or native metadata fact supplies a trustworthy relation.
Invalid JSON, contract loss, payload/path ID disagreement, invalid UTF-8, and
decoded bound violations become preserved unknown records with provenance.

## Schema 24 and replacement projection

Schema 24 adds provenance-bearing assertions:

- `task_snapshot_assertions` for collection metadata and snapshot coverage;
- `task_item_assertions` for normalized task children;
- `plan_assertions` for plan documents.

The current query-facing projection is stored in:

- `canonical_task_collections` and `canonical_tasks`;
- `canonical_plans`.

Every document commit replaces all assertions owned by that source object,
including same-generation edits. A complete todo replacement therefore
retracts every removed child. Replacing or deleting one numbered task document
retracts only that item; other item documents in the collection remain.

Collections combine agreeing item-document fragments rather than treating
them as competing whole snapshots. Equal task identities with distinct native
values remain as competing assertions. Plans use the same deterministic
assertion reduction. Canonical views choose a decisive fact by stable fact ID,
record resolved/conflicting state and counts, and retain every current
assertion for audit.

Ordinary changes publish:

- `runtime.task-collection.changed`;
- `runtime.task.changed`;
- `runtime.plan.changed`.

Ambiguity publishes the corresponding
`diagnostic.runtime.task-collection-conflict`,
`diagnostic.runtime.task-conflict`, or `diagnostic.runtime.plan-conflict`
topic. Removing a competitor resolves the canonical row and clears its
diagnostic in the same transaction as the source cursor.

## Explicit non-claims

This pack does not:

- turn a completed task or todo into terminal run evidence;
- infer activity, waiting, success, failure, or cancellation from task state;
- infer a session or team from a task-directory name;
- infer that a plan is active or belongs to the newest transcript;
- use `.lock` presence or `.highwatermark` as item completion evidence.

The legacy `.lock`/`.highwatermark` convenience row is not the native task
item model. Those control files need a separate reviewed metadata contract if
they become part of the public task query pack.

## Conformance evidence

The tests cover:

- declarative selectors, byte/item bounds, contract version, and capability
  quality;
- strict todo/task/plan path contexts and task payload/path ID agreement;
- todo scope, native fields, future status preservation, and identity stability
  across status edits;
- numbered task fields, dependencies, item coverage, and deliberately absent
  guessed scope;
- Markdown title extraction, slug fallback, exact content/size, and invalid
  UTF-8 preservation;
- complete-list replacement, removed-child retraction, audit cleanup, and
  confirmed file deletion;
- item-document merging, per-object replacement, competing task assertions,
  deterministic conflict publication, and resolution after retraction;
- plan replacement, competing assertions, diagnostic clearing, and deletion;
- task-before-session late joins and the invariant that completed tasks create
  no `observed_run_states` entry;
- Rust/TypeScript schema parity and ingest-differential table coverage.

The Rust crate suite contains 357 passing tests after this slice. Repository
type checks, builds, architecture ratchets, and ingest differential matrices
remain the release gate for the commit.

## Remaining Phase 5 work

File-history/artifacts are now implemented in the adjacent Phase 5 pack.
Workflow, sessions-index, memory, tool-result, and settings sources still need
reviewed semantics. The observation coordinator and production Rust live
cutover are also required for the Phase 5 exit gate.

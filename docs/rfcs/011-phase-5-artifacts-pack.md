# RFC 011 Phase 5: Claude file-history artifact capability pack

Status: implemented on 2026-08-11

This slice moves Claude's transcript file-history metadata and
`~/.claude/file-history` backup blobs into the common Rust observation engine.
It joins independently arriving metadata and content without claiming that a
run produced a tracked file.

## Corpus map and source contract

The reviewed local corpus contained 9,557 `file-history-snapshot` records,
including 3,940 snapshot updates, and 2,309 `file-history-delta` records.
Snapshots contained 291,072 tracked-file entries and no snapshot exceeded 253
entries. Of the delta records, 1,188 named a backup and 1,121 explicitly used a
null backup name.

The corresponding native store contained 377 UUID session directories and
21,966 blobs. Every path matched
`file-history/<session-uuid>/<lowercase-hex>@v<canonical-positive-version>`;
the largest observed blob was 523,018 bytes. All 260,274 named metadata
references resolved to a blob and every metadata version agreed with its blob
name. Some blobs were not valid UTF-8, so content is retained as arbitrary
bytes rather than text.

Transcript metadata remains on the two existing append streams. The confined
`home` root now also declares one canonical `ReplaceDocument` stream:

- `file-history-blobs` selects `file-history/*/*@v*`;
- each blob is bounded at 1 MiB;
- priority is foreground repair;
- consistency is snapshot replacement;
- confirmed deletion mirrors the source;
- raw retention remains full during migration.

Claude declares `runtime.artifacts` as native, artifact-granularity, and live.
The additive stream and transcript fact output advance the adapter contract
from 7 to 8 and declare `claude-code-file-history-v1`. Existing facts retain
their identities because metadata facts are appended after all earlier facts
for a transcript record. Historical file-history transcript records still
need targeted contract replay to materialize the new metadata assertions.

## Typed facts and identity

A `file-history-snapshot` or `file-history-delta` record emits one
`ArtifactMetadataSnapshotFact`. It retains:

- session, native message, and native snapshot-message identity;
- checkpoint versus delta provenance and the native snapshot-update flag;
- qualified source time;
- tracked path, optional real parent directory, version, and exact backup
  time for each entry;
- whether content is expected or was explicitly not captured.

A null backup filename is positive `NotCaptured` evidence for a newly created
path. It is not treated as a missing blob. Named backups use stable identity
from session plus native backup name. Not-captured entries use session, tracked
path, version, and backup time so distinct native observations do not collapse.

Each present backup document emits an `ArtifactContentFact` with session,
native backup name, native file hash, version, exact bytes, byte size, and the
same named artifact key used by transcript metadata. Empty blobs remain
present artifacts. Arbitrary bytes are stored as SQLite BLOBs and encoded as
base64 only inside the JSON audit fact. Confirmed absence emits no content fact
and retracts the prior document assertion.

Malformed paths, noncanonical names, zero versions, metadata/name version
disagreement, empty required fields, duplicate artifact identities, and
decoded metadata beyond 512 entries are rejected or diagnosed. A malformed
file-history metadata projection does not discard the ordinary transcript
message fact.

## Schema 25 and two-sided reduction

Schema 25 adds provenance-bearing storage:

- `artifact_snapshot_assertions` for checkpoint/delta parent facts;
- `artifact_metadata_assertions` for tracked-file children;
- `artifact_content_assertions` for independently replaceable blobs;
- `canonical_artifacts` for the deterministic query-facing join.

Metadata and content may arrive in either order. The canonical row exposes one
of four content states:

- `captured`: metadata and content are both present; resolution separately
  reports whether their identities agree;
- `not_captured`: metadata explicitly says no backup was captured;
- `missing_content`: metadata expects a named blob that is absent;
- `orphan_content`: a blob exists before or without matching metadata.

`captured` and `not_captured` are resolved states. Missing or orphan content is
incomplete. Session, native name, version, and expected-capture disagreement
between the two halves is an explicit join conflict. The reducer retains all
current assertions, chooses decisive facts by stable fact ID rather than
arrival order, and records assertion and competing-value counts.

Ordinary changes publish `runtime.artifact.changed`. Competing metadata,
competing content, or a cross-half mismatch publishes
`diagnostic.runtime.artifact-conflict`; retracting the conflicting assertion
clears the diagnostic in the same transaction.

## Rewrite, replacement, and non-claims

Transcript checkpoints and deltas are historical observations. They
accumulate within one append generation; a checkpoint does not erase older
artifact history merely because a path is absent. Transcript truncation,
rewrite, or contract replay starts a new generation and retracts the replaced
generation's metadata assertions.

Each blob is a replaceable document. A same-generation content edit replaces
that source object's assertion, and confirmed deletion retracts it. Canonical
foreign keys move before superseded audit facts are deleted, so replacement
and retraction remain atomic and foreign-key safe.

This pack deliberately does not:

- attribute a tracked file or backup to a run;
- interpret a backup as proof that Claude created or edited the file;
- add artifact-specific run lifecycle evidence; transcript records retain
  their pre-existing generic activity observation, while blob arrival and
  deletion add none;
- treat a missing expected blob as an empty file or explicit non-capture;
- infer current workspace contents from a historical backup.

The only artifact relation proven by the native formats is session scope.
Artifact facts themselves create no run relation or run-state evidence;
transcript commits continue to project their generic run and activity facts
independently.

## Conformance evidence

The tests cover:

- selector, byte/item bounds, contract version, schema version, and capability
  quality;
- strict session/path/name parsing and metadata/name version agreement;
- checkpoint, update, delta, named-backup, and explicit non-capture decoding;
- stable cross-stream identity and preservation of the ordinary message when
  artifact metadata is malformed;
- exact text, empty, and non-UTF-8 blob bytes plus JSON audit round-trip;
- metadata-first and content-first convergence across all four content states;
- repeated equal checkpoint assertions without manufactured conflict;
- same-generation blob replacement, confirmed deletion, transcript-generation
  rewrite, and audit cleanup;
- competing metadata/content assertions, join mismatches, deterministic
  diagnostics, and resolution after retraction;
- the invariant that isolated artifact facts add no run state;
- Rust/TypeScript schema parity and ingest-differential table coverage.

The Rust crate suite contains 365 passing tests after this slice. Repository
type checks, builds, clippy, architecture ratchets, and the Claude small,
Claude medium, Codex, and Grok ingest-differential matrices are the release
gate for the commit.

## Remaining Phase 5 work

Workflows are now implemented in the adjacent Phase 5 pack. Sessions-index,
memory, tool-result, settings, and other reviewed sidecar packs remain. The
observation coordinator and production Rust live cutover are also required for
the Phase 5 exit gate.

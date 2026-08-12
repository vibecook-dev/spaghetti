# RFC 011 Phase 5: Claude session-index metadata pack

Status: implemented and validated on 2026-08-11

This slice moves Claude's project-level `sessions-index.json` metadata behind
the RFC 011 adapter, common replace-document driver, transactional fact store,
and deterministic projector. It deliberately does not use the index as proof
that a transcript or any messages still exist.

## Corpus survey

The reviewed corpus combines the current Claude home with the committed small
and medium fixtures:

- 28 version-1 documents containing 287 globally distinct UUID entries;
- 0 duplicate session IDs within one document;
- 2 documents without `originalPath` and 6 entries without `summary`;
- 1 sidechain entry and 108 entries with an empty (but present) branch;
- message counts from 0 through 87;
- a largest document of 59,313 bytes and at most 84 entries;
- native timestamps with millisecond precision, including small cases where
  `created` is later than `modified`, which are retained rather than repaired;
- current-home indexes whose 242 entries can outlive their sibling transcript
  JSONL files, proving that index presence is not history availability.

Every observed entry contains `sessionId`, `fullPath`, `fileMtime`,
`firstPrompt`, `messageCount`, `created`, `modified`, `gitBranch`,
`projectPath`, and `isSidechain`. Unknown root fields remain in the retained
native snapshot.

## Adapter contract

Adapter contract version 10 declares source schema
`claude-code-session-index-v1` and one additive stream:

| Stream            | Selector                                 | Driver                          | Authority    | Scope   | Priority    |
| ----------------- | ---------------------------------------- | ------------------------------- | ------------ | ------- | ----------- |
| `session-indexes` | `*/sessions-index.json` under `projects` | `ReplaceDocument` (1 MiB bound) | Supplemental | Project | Interactive |

The stream uses snapshot-replace consistency, mirror-source deletion, and full
raw retention. The exact relative path is
`<project>/sessions-index.json`; nested or renamed lookalikes fail object
bootstrap.

The decoder accepts the reviewed version-1 shape, bounds a document to 4,096
entries, requires unique UUID session IDs, and rejects empty identity/time/path
fields needed by the projection. Malformed, unsupported-version, duplicate,
or contract-losing documents become `UnknownRecord` facts plus permanent
diagnostics. Confirmed absence emits no assertion so the projector can retract
the prior source-owned snapshot.

`SessionIndexSnapshotFact` retains the complete native JSON and normalizes its
project identity, version, optional original path, and ordered entries. Each
entry preserves native session identity, path, file mtime, first prompt,
optional summary, message count, created/modified times, branch, project path,
and sidechain flag. It emits no `SessionFact`, `MessageFact`, `RunFact`, or
runtime evidence.

## Schema 27 and projection

Schema version 27 adds:

- `session_index_snapshot_assertions` and
  `session_index_entry_assertions` for complete source-owned project snapshots;
- `canonical_session_indexes` for the deterministic project view;
- `canonical_session_index_entries` for normalized metadata and transcript
  join state;
- project, source-object, session, ordinal, and join-status indexes.

Every same-generation revision replaces the complete assertion owned by that
source object. Missing entries retract in the same transaction. Confirmed file
deletion removes the snapshot; an empty native `entries` array retains a
resolved project index with zero entries.

Multiple project snapshots are retained. Byte-different snapshots produce a
project conflict. Child entries count both byte disagreement and omission by a
competing complete snapshot. Deterministic fact identity chooses a visible row
without discarding the competing assertions; retracting a competitor clears
the diagnostic.

An index entry has explicit transcript state:

- `missing`: no canonical transcript session exists;
- `present`: the independently committed session exists under the same project;
- `different_project`: the stable session identity exists under a conflicting
  project relation.

Index-first and transcript-first commits converge. Transcript arrival or
generation retraction refreshes only index entries that assert the affected
session. Metadata-only entries never manufacture canonical session history,
message rows, runs, or lifecycle state. Superseded audit facts retract only
after canonical foreign keys move to the surviving assertion.

The common writer derives these durable topics:

- `history.session-index.changed`;
- `history.session-index-entry.changed`;
- `diagnostic.history.session-index-conflict`;
- `diagnostic.history.session-index-entry-conflict`.

## Conformance evidence

Tests cover:

- declarative selector, byte bound, authority, priority, capability, and exact
  path validation;
- full native snapshot retention, absent optional summary, and the invariant
  that decoding creates only metadata facts;
- malformed JSON, unsupported versions, invalid IDs, and duplicate IDs as
  preserved unknown records;
- index-first and transcript-first convergence;
- metadata visibility without transcript or message fabrication;
- same-generation complete replacement, empty snapshots, source deletion
  semantics, and superseded audit cleanup;
- competing complete snapshots, entry byte/omission conflicts, cross-project
  transcript joins, durable diagnostics, and resolution after retraction;
- Rust/TypeScript schema parity and ingest-differential table coverage.

The Rust workspace contains 377 passing tests after this slice. TypeScript type
checks, release builds, Rust clippy, and the architecture ownership ratchet all
pass. The Claude small/medium, Codex, and Grok ingest-differential matrices each
report zero differences.

## Remaining Phase 5 work

Memory, standalone tool-result, settings, and other reviewed sidecar packs
remain. The observation coordinator and production Rust live cutover are also
required for the Phase 5 exit gate.

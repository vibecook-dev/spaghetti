# RFC 011 Phase 5: Claude project-memory pack

Status: implemented and validated on 2026-08-11

This slice moves Claude's project `memory/*.md` context behind the RFC 011
adapter, common replace-document driver, transactional fact store, and
deterministic projector. Memory remains project context: it does not create
transcript history, run state, or lifecycle evidence.

## Corpus survey

The reviewed corpus combines the current Claude home with the committed small
and medium fixtures. The current home contains:

- 150 immediate Markdown files across 21 project memory directories;
- 21 `MEMORY.md` index documents and 129 sibling topic documents;
- 826,299 total bytes, with a largest document of 118,222 bytes;
- no empty documents and no invalid UTF-8 documents;
- index styles ranging from link lists with no heading to long standalone
  project summaries;
- topic files with Markdown extensions whose detected content can include HTML
  or binary-like bytes, so validity is based on UTF-8 and path contract rather
  than MIME guessing.

The legacy reader indexed only `memory/MEMORY.md`. The native corpus shows that
topic files hold most of the durable context, so the Rust pack treats every
immediate `memory/*.md` file as an independently owned document. It does not
parse Markdown links into asserted relationships.

## Adapter contract

Adapter contract version 11 declares source schema
`claude-code-project-memory-v1`, capability `context.project_memory`, and one
additive stream:

| Stream                     | Selector                         | Driver                          | Authority | Scope   | Priority    |
| -------------------------- | -------------------------------- | ------------------------------- | --------- | ------- | ----------- |
| `project-memory-documents` | `*/memory/*.md` under `projects` | `ReplaceDocument` (1 MiB bound) | Canonical | Project | Interactive |

The stream uses snapshot-replace consistency, mirror-source deletion, and full
raw retention. Exact paths are
`<project>/memory/<non-empty-name>.md`; nested or non-Markdown lookalikes fail
object bootstrap. The common driver owns stable reads, revisions, retries, and
confirmed absence.

`ProjectMemoryDocumentFact` preserves stable project/document identity, native
project key and relative document path, exact UTF-8 content, byte size,
heading-derived title with filename fallback, and whether the filename is
exactly `MEMORY.md`. Invalid UTF-8 is retained as an `UnknownRecord` with a
permanent diagnostic. A valid zero-byte document remains a present document;
confirmed absence emits no assertion and retracts the previous source-owned
document.

The capability is native and live at `memory_document` granularity. It is a
product-level project-context pack rather than a runtime pack. The adapter
emits no session, message, run, usage, or evidence fact from memory content.

## Schema 28 and projection

Schema version 28 adds:

- `project_memory_document_assertions` for source-owned document claims;
- `canonical_project_memory_documents` for deterministic current views;
- document, source-object, project/path, and canonical project-order indexes.

Every same-generation revision replaces the assertion owned by that source
object. Each native file is independent: editing or deleting one topic does not
replace its siblings or `MEMORY.md`. A deterministic fact ID selects the
visible document when duplicate source objects assert the same stable
project/path identity. Byte-different documents remain competing assertions
with a durable conflict diagnostic; removing the competitor resolves the view.

The common writer derives these durable topics:

- `context.project-memory-document.changed`;
- `diagnostic.context.project-memory-document-conflict`.

Superseded audit facts retract only after the canonical foreign key moves to
the surviving assertion. Memory projection never changes canonical sessions,
messages, runs, run evidence, observed state, or usage.

## Conformance evidence

Tests cover:

- declarative selector, byte bound, authority, priority, capability, and exact
  path validation;
- index/topic classification, exact content and size retention, heading and
  filename title behavior, and absent-file handling;
- invalid UTF-8 preservation without partial semantic projection;
- the invariant that memory decoding creates no history or runtime facts;
- independent index/topic projection, same-generation replacement, confirmed
  deletion, and superseded audit cleanup;
- deterministic duplicate conflicts, durable diagnostics, and resolution after
  retraction;
- Rust/TypeScript schema parity and ingest-differential table coverage.

The Rust workspace contains 380 passing tests after this slice. TypeScript type
checks, the release build, Rust clippy, and the architecture ownership ratchet
all pass. The Claude small/medium, Codex, and Grok ingest-differential matrices
each report zero differences.

## Remaining Phase 5 work

Persisted tool results and interpretation settings are now implemented in
adjacent Phase 5 packs. Other reviewed sidecars may remain. The observation
coordinator and production Rust live cutover are also required for the Phase 5
exit gate.

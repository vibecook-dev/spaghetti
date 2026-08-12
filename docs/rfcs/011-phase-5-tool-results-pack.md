# RFC 011 Phase 5: Claude persisted tool-results pack

Status: implemented and validated on 2026-08-11

This slice moves Claude's immediate `tool-results/*.txt` sidecars behind the
RFC 011 adapter, common replace-document driver, transactional fact store, and
deterministic projector. A sidecar supplements transcript content; it never
creates a message, session, run, activity observation, or lifecycle claim.

## Corpus survey

The reviewed corpus combines the current Claude home with the committed small
and medium fixtures. The current home contains:

- 1,597 immediate `.txt` sidecars under UUID session directories;
- 138,842,771 total bytes, with a largest text sidecar of 13,823,312 bytes;
- no zero-byte text sidecars;
- 1,193 `toolu_*` stems, 318 short alphanumeric stems, and 86
  `hook-*-stdout` stems;
- 174 files that are not valid UTF-8 and were silently skipped by the legacy
  `read_to_string` reader;
- 4 immediate JSON files, 2 immediate PDFs, and 100 nested rendered-image or
  other non-text files that belong to separate future artifact semantics.

The committed fixtures contain 23 valid UTF-8 text sidecars. Fixture stems
often match transcript tool calls, but the full corpus proves that filename
shape alone does not establish a model call. Inline transcript results can be
compact summaries while the sidecar retains fuller output, and either half
can exist without the other.

## Adapter contract

Adapter contract version 12 declares source schema
`claude-code-persisted-tool-result-v1`, capability
`history.persisted_tool_results`, and one additive stream:

| Stream                   | Selector                                  | Driver                           | Authority    | Scope                 | Priority    |
| ------------------------ | ----------------------------------------- | -------------------------------- | ------------ | --------------------- | ----------- |
| `persisted-tool-results` | `*/*/tool-results/*.txt` under `projects` | `ReplaceDocument` (16 MiB bound) | Supplemental | Persisted tool result | Interactive |

The stream uses snapshot-replace consistency, mirror-source deletion, and full
raw retention. Exact paths are
`<project>/<session-uuid>/tool-results/<non-empty-id>.txt`; renamed, nested,
non-text, or non-UUID-session lookalikes fail object bootstrap. This narrow
boundary retains legacy product scope while leaving JSON, PDF, and rendered
descendants for a separately reviewed binary-artifact pack.

`PersistedToolResultFact` preserves stable result/session/project identity,
native project/session/tool IDs, relative document path, exact UTF-8 content,
and byte size. Stems are accepted as opaque non-empty native identifiers;
`toolu_*`, short generated IDs, and hook stdout IDs have equal path validity.
Invalid UTF-8 becomes an `UnknownRecord` with a permanent diagnostic. A valid
zero-byte document remains present, while confirmed absence emits no assertion
and retracts the previous source-owned result.

The decoder emits no transcript or runtime facts. It does not interpret text
that mentions an agent ID as delegation evidence.

## Schema 29 and projection

Schema version 29 adds:

- `persisted_tool_result_assertions` for source-owned text claims;
- `canonical_persisted_tool_results` for deterministic current output and
  correlation state;
- `message_tool_references`, an indexed normalization of common typed
  `ToolCall` and `ToolResult` message blocks;
- result, source-object, native session/tool ID, correlation, and message
  reference indexes.

Every same-generation file revision replaces the assertion owned by that
source object. Deterministic fact identity selects the visible result when
duplicate objects assert the same stable session/tool identity. Equal content
increases assertion count without a content conflict; byte-different content
remains competing and diagnosed. Superseded audit facts retract only after the
canonical foreign key moves to a surviving assertion.

Transcript correlation is session-scoped and based only on typed common block
IDs. It is explicitly classified as:

- `unlinked`: no matching transcript call or result;
- `call_only`: exactly one matching tool call;
- `result_only`: exactly one matching inline tool result;
- `linked`: exactly one of each;
- `ambiguous`: duplicate matching blocks or messages on either side.

The canonical row stores the unique call/result message keys when available,
match counts, and explicit join-conflict state. Transcript-first and
sidecar-first commits converge. Transcript generation replacement captures old
reference keys before retraction, indexes the new generation, and refreshes
affected sidecars in the same transaction. Sidecar presence never repairs or
overwrites inline transcript content.

The common writer derives these durable topics:

- `history.persisted-tool-result.changed`;
- `diagnostic.history.persisted-tool-result-conflict`.

## Conformance evidence

Tests cover:

- declarative selector, 16 MiB bound, authority, priority, capability, and
  exact path validation across all observed filename families;
- exact content and size retention, empty-present behavior, invalid UTF-8
  preservation, and confirmed absence;
- the invariant that decoding creates no session, message, run, activity, or
  lifecycle facts;
- sidecar-first and transcript-first late joins through every correlation
  status;
- same-generation replacement, transcript-generation retraction, confirmed
  sidecar deletion/recreation, and superseded audit cleanup;
- agreeing duplicates, competing output content, duplicate-block ambiguity,
  durable diagnostics, and conflict resolution;
- Rust/TypeScript schema parity and ingest-differential table coverage.

The Rust workspace contains 384 passing tests after this slice. TypeScript
type checks, release builds, Rust clippy, and the architecture ownership
ratchet also pass. The small, medium, Codex, and Grok ingest-differential
matrices each report zero differences.

## Remaining Phase 5 work

Interpretation settings are implemented in an adjacent Phase 5 pack. Other
reviewed sidecars may remain. The observation coordinator and production Rust
live cutover are also required for the Phase 5 exit gate.

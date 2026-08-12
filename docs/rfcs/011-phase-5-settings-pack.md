# RFC 011 Phase 5: Claude interpretation-settings pack

Status: implemented and validated on 2026-08-11

This slice moves Claude's root `settings.json` and `settings.local.json`
documents behind the RFC 011 adapter, common replace-document driver,
transactional fact store, and deterministic projector. It exposes only
configuration evidence that affects source interpretation. It is not a
general user-settings export.

## Corpus and legacy survey

The reviewed live Claude home contains both root documents. Together they are
5,795 bytes. The global file currently contains model/effort and behavior
flags, permission rules, enabled-plugin state, hook declarations, environment
values, status-line commands, marketplace locations, and UI preferences. The
local file contains a distinct permission-rule list. Values were not copied
into this survey.

The legacy cold parser reads both files, defaults missing or malformed global
settings to an artificial empty permission object, and represents a missing
or malformed local file as `null`. The legacy live handler emits both files
but refreshes only the global in-memory cache; malformed writes are discarded
until another watcher event. These paths therefore disagree on local live
state and cannot distinguish confirmed absence from invalid content.

Claude's documented scope semantics distinguish scalar and collection values:
higher-precedence local scalars override global scalars, while array-valued
settings—including permission lists—are concatenated and de-duplicated.
Enabled-plugin booleans override per plugin key. This pack applies those rules
only across the two configured root documents; it does not invent managed,
command-line, project-directory, or host-process layers.

## Adapter and privacy contract

Adapter contract version 13 declares source schema
`claude-code-interpretation-settings-v1`, capability
`configuration.interpretation_settings`, and one additive stream:

| Stream                    | Selector                                            | Driver                          | Authority | Scope    | Priority    |
| ------------------------- | --------------------------------------------------- | ------------------------------- | --------- | -------- | ----------- |
| `interpretation-settings` | root `settings.json` and `settings.local.json` only | `ReplaceDocument` (1 MiB bound) | Canonical | Instance | Interactive |

The stream uses snapshot-replace consistency, mirror-source deletion, and
hash-only raw retention. Nested, renamed, backup, managed, and lookalike files
fail object bootstrap.

`InterpretationSettingsFact` retains document/scope identity, global or local
layer, path, current `valid` or `invalid` health, size, and an allowlisted
normalized snapshot. The snapshot may contain:

- agent, model, effort, and plan-directory selectors;
- thinking, compaction, and permission-prompt behavior flags;
- permission mode/restriction scalars and allow/ask/deny rule identifiers;
- enabled-plugin identifiers and booleans;
- hook event names plus declaration counts.

It intentionally excludes environment values, hook matcher text and hook
bodies, status-line commands, marketplace sources/paths, UI preferences, and
arbitrary unknown keys. Malformed input emits one invalid settings fact and a
permanent redacted diagnostic; it never copies the source bytes into the typed
fact audit. Confirmed absence emits no assertion. The decoder emits no session,
message, run, activity, or lifecycle facts.

## Schema 30 and projection

Schema version 30 adds:

- `interpretation_settings_assertions` for source-owned, redacted document
  claims;
- `canonical_interpretation_settings_documents` for deterministic per-file
  health and conflict reduction;
- `canonical_effective_interpretation_settings` for the source-instance
  global/local merge;
- document, source-object, scope, and layer indexes.

Every same-generation revision replaces the assertion owned by that source
object. Agreeing duplicate assertions increase the assertion count without a
conflict. Different normalized values, metadata, validity, or invalid-payload
hashes remain competing and diagnosed. Callback order never decides which
permission state is healthy.

The effective reducer:

- takes local scalar/boolean/plugin-key values over global values;
- concatenates global then local permission arrays and removes exact
  duplicates while preserving first occurrence order;
- adds redacted hook declaration counts per event;
- reports each layer as `absent`, `valid`, `invalid`, or `conflicting`;
- reports the scope as `resolved`, `invalid`, or `conflicting`.

An invalid current layer is not treated as empty or last-known-good. Healthy
layers can still be inspected in the effective payload, but the row remains
explicitly `invalid` so consumers cannot present stale permission state as a
current trustworthy configuration. Deleting the invalid document clears only
that layer; deleting both documents retracts the effective row.

The common writer derives:

- `configuration.interpretation-settings-document.changed`;
- `configuration.interpretation-settings.changed`;
- `diagnostic.configuration.interpretation-settings-conflict`;
- `diagnostic.configuration.interpretation-settings-health`.

## Conformance evidence

Focused tests cover:

- exact root path selection, bounds, authority, priority, hash-only retention,
  capability declaration, and confirmed absence;
- normalized scalar, permission, plugin, and hook metadata decoding;
- exclusion of secret-like environment, hook-command, status-command,
  marketplace, and unknown values from serialized fact audit payloads;
- malformed JSON and typed-shape loss as redacted invalid health facts;
- scalar override, collection union/de-duplication, keyed plugin override, and
  additive hook metadata;
- same-generation valid-to-invalid replacement, local/global deletion,
  superseded audit cleanup, and absence of fabricated history/runtime rows;
- agreeing duplicates, competing settings, durable diagnostics, and conflict
  clearing;
- Rust/TypeScript schema parity and ingest-differential table inventory.

The validation gate passes with 389 Rust tests, TypeScript typechecking, the
full production build, clippy with warnings denied, the RFC 011 architecture
ratchet, Rust and changed-file Markdown/TypeScript formatting checks, and zero
differences for the small Claude, medium Claude, Codex, and Grok ingest
matrices.

## Remaining Phase 5 work

Other reviewed sidecar packs may remain. The observation coordinator and
production Rust live cutover are still required for the Phase 5 exit gate.

# RFC 012A: Agent adaptation and common-engine boundaries

- **Status:** Ratified semantic contract; base-wire and support-selection v1
  slices frozen, remaining exact APIs provisional until their cross-language
  fixtures pass
- **Created:** 2026-08-15
- **Ratified:** 2026-08-15
- **Parent:** [RFC 012 umbrella](./012-evidence-backed-adapters-and-progressive-readiness.md)
- **Program plan:** [RFC 012 implementation plan](./012-implementation-plan.md)
- **Evidence:** [Phase 0 catalog census](./012-phase-0-census-2026-08-15.md)
  and [Phase 0B runtime census](./012-runtime-observation-census-2026-08-15.md)
- **Owns:** common/adapter dependency law, qualified-value/base identity
  contracts, stable source-instance/external entity references,
  source-record/fact/semantic-revision identity, common source/family coverage,
  tier/view compositionality, source and scope declarations, Agent Data Surface
  documents, fixtures, support releases, version policy, conformance,
  promotion, and drift
- **Does not own:** catalog reducers/readiness, runtime fact semantics, durable
  queries, or the scoped-observer lifecycle

## 1. Summary

Spaghetti standardizes agent support as a versioned, evidence-backed process.
An adapter interprets native evidence and declares how bounded common source
mechanics reach that evidence. It does not own watchers, cursors, storage,
queues, public queries, readiness, or presentation.

This RFC also turns “the common engine” into a set of dependency-constrained
logical subsystems rather than one undifferentiated object. The subsystem
boundaries are normative even when the initial implementation shares a crate.
Physical crate names and the timing of extraction remain implementation
details.

The intended result is:

```text
pinned native artifact
  -> evidence and Agent Data Surface model
  -> bounded source/scope declarations
  -> adapter decoder
  -> independent fixtures and simulator
  -> conformance
  -> promoted support release
```

## 2. Decisions

1. Agent support is identified by a promoted support release, not merely the
   Spaghetti package version.
2. Every promoted native-surface claim resolves to private evidence,
   deterministic sanitized fixtures, or an explicit unsupported/degraded
   declaration.
3. Common source runtimes produce `SourceRecord`; adapters return `FactBatch`
   plus a mapping disposition.
4. Adapters select and parameterize a restricted set of source and scope
   primitives. They cannot implement private watcher, retry, cursor, queue,
   bootstrap, artifact-read, or database paths.
5. Adapter-native joins operate only on bounded, authorized inputs supplied by
   the common runtime. They cannot perform undeclared filesystem or database
   enumeration.
6. Expected fixture results and corpus transitions remain independent of the
   adapter implementation under test.
7. Unknown vendor versions follow the conservative runtime policy in section
   10; version drift never silently inherits full support.
8. Logical dependency laws are enforced before physical crate extraction.
9. Persistable public entity references contain only stable semantic identity,
   never an in-memory handle or database row ID.
10. `SemanticRevisionRef` is the mandatory durable/scoped join identity for a
    typed native-derived revision; topology-specific delivery IDs do not replace
    it.
11. Source/family coverage uses common driver-aware positions and comparison;
    database commits, observer sequences, and native positions remain distinct
    clocks.

## 3. Contract maturity

| Element                                                      | Classification          |
| ------------------------------------------------------------ | ----------------------- |
| Common/adapter ownership boundary                            | Architecture invariant  |
| Logical dependency and forbidden-edge laws                   | Architecture invariant  |
| `SourceRecord -> FactBatch` seam                             | Semantic contract       |
| Entity/source-record/fact identity                           | Semantic contract       |
| External entity/native-identity/semantic-revision references | Semantic contract       |
| Common source/family coverage                                | Semantic contract       |
| Tier/view/topology compositionality                          | Semantic contract       |
| Mapping dispositions                                         | Semantic contract       |
| Source and scoped-relation primitive semantics               | Semantic contract       |
| RFC 012A v1 opaque reference encoding                        | Frozen contract fixture |
| RFC 012A v1 coverage wire/comparator                         | Frozen contract fixture |
| RFC 012A v1 runtime support classification                   | Frozen contract fixture |
| RFC 012A v1 public contract-version selection                | Frozen contract fixture |
| Exact Rust trait and serialized manifest shapes              | Proposed API            |
| Physical crate names                                         | Implementation detail   |
| Support-ledger storage format                                | Proposed API            |
| New-agent implementation duration                            | Experiment target       |

Approving this RFC does not freeze Rust module names or a public dynamic-plugin
ABI.

### 3.1 Frozen RFC 012A v1 base-wire slice

The first A1 implementation slice freezes these cross-language details:

- canonical source-instance, entity, source-record, fact, and fact-revision
  keys use domain-separated, length-prefixed BLAKE3 derivation;
- their public opaque representation is `v1:` followed by the unpadded
  URL-safe base64 encoding of the 32-byte digest;
- external entity and semantic revision wrappers use contract major `1` and
  reject unknown majors rather than guessing compatibility;
- qualified values, adjacent native-identity claims, coverage points, coverage
  sets, and comparison outcomes use the frozen
  [`rfc012a-v1.json`](../../crates/spaghetti-napi/fixtures/contracts/rfc012a-v1.json)
  fixture; and
- opaque coverage positions are equal only by opaque identity. Ordering is
  allowed only when the common source driver supplies an explicit monotonic
  order; consumers never compare opaque bytes lexically.

RFC 011's existing numeric-source-instance-based `EntityKey` and `FactId` are
legacy internal identities during migration. They are not valid RFC 012A
external or semantic references. The Rust implementation keeps the new model
parallel to those types so each fact family can migrate through a versioned
shadow path.

This slice does not freeze complete N-API host requests, adapter manifests,
support-ledger serialization, or child-RFC payloads. Those remain proposed
until their own cross-language fixtures pass.

### 3.2 Frozen RFC 012A v1 support-selection slice

The first A2 implementation slice freezes the shared
[`rfc012a-support-v1.json`](../../crates/spaghetti-napi/fixtures/contracts/rfc012a-support-v1.json)
fixture and these rules:

- `support_selection_contract_version` and
  `selection_contract_version` are both `1`;
- candidate and retired releases never confer runtime support, including a
  forward-catalog permission;
- exact versions are opaque canonical strings, while range endpoints and range
  comparison are dotted numeric values;
- more than one matching promoted release fails closed as
  `ambiguous_promoted_release`; release-list order is never a tie-breaker;
- every decision publishes a class, optional promoted support-release ID,
  structured reason, and the five operation permissions from section 10.3;
- catalog, durable, and scoped typed access require a private authorization
  issued from that decision plus an explicit compatible contract selection;
  serialized decisions do not contain an access capability; and
- support authorization runs before contract negotiation, so a candidate or
  incompatible artifact cannot use malformed requests to inspect the offered
  typed contract surface.

Rust is the runtime authority. The independent Python tool and portable
TypeScript SDK execute the same fixture as conformance oracles; neither can
mint the Rust access capability. The exact N-API/IPC host wrapper that carries
the request remains provisional until Rust-issued support authorization is
carried through the host into source access.

The Rust authority verifies an in-memory support package before it can enter a
runtime catalog: the ledger and each ADS, source declaration, scope program,
evidence manifest, and conformance manifest are bounded; reference paths are
canonical confined relative paths; every reference has an exact SHA-256
binding; and adapter/conformance identities agree with the ledger. A compiled
adapter independently binds its package version, decoder contract, and
ADS/source/scope digests. The strict registry accepts that adapter only when at
least one matching release is promoted. Candidate packages may be verified for
conformance but cannot satisfy strict registration or mint typed access.

There are currently zero promoted built-in support releases. The existing
public N-API engine therefore names and uses an explicit legacy compatibility
registry, which cannot authorize the new typed contract. Moving that host to
the strict registry is a promotion/integration gate, not an implicit upgrade
of the current candidate packages.

The initial common access-ledger implementation reserves declared worst-case
bytes and rows before native access, counts distinct hashed objects and
per-parent fan-out, and commits actual usage afterward. Failed, panicked, or
otherwise abandoned reads consume their reservation conservatively. Its trace
is bounded and contains relation IDs, opaque object tokens, operation/phase,
bounds, counts, outcomes, and limit names rather than native paths or payloads.
Adapters may declare bounds but cannot mint tokens or reservations. This
implementation shape remains provisional until promoted scope declarations and
the scoped observer execute it through cross-topology fixtures.

The next executable A2 slice makes the declaration-to-ledger boundary
concrete:

- support-package verification strictly parses the referenced scope document
  and retains that parsed declaration beside its verified digest;
- a strict adapter registration and every typed-access authorization require
  the adapter's compiled scope declaration to equal the selected verified
  support package, not merely to repeat its digest string;
- one `ScopeAccessPlan` selects exactly one declared program and instantiates
  one common access budget per relation with the exact declared fan-out, depth,
  object, byte, and row limits;
- an access request supplies only a relation ID, permitted operation, phase,
  parent token, exact named identity inputs, and worst-case reservation. The
  access root, locator template, statement ID, unavailable behavior, and other
  executable relation data come back from the verified declaration and cannot
  be substituted by the caller; and
- unknown relations, incompatible operations, malformed identity inputs, and
  invalid reservations fail before entering a source driver. Declared bound
  violations enter the bounded relation trace as denied access.

Mechanical compilation is not authority. Incomplete and candidate declarations
may compile for conformance, but only a promoted support decision plus compatible
public-contract selection may authorize native access. The current durable and
scoped hosts do not yet carry that authorization into this plan, so this slice
does not promote any built-in support release.

The authorization-to-plan seam is also explicit. A scoped typed authorization
is issued only when an observation contract version was negotiated. It embeds
the selected adapter, verified support-release digest, scope-program digest,
and the exact promoted parsed declaration. The common runtime may select only a
program present in that embedded declaration and may construct an
`AuthorizedScopeAccessPlan` only from that borrowed selection. A durable or
catalog authorization, an authorization without observation semantics, an
unknown program ID, or an incomplete/candidate declaration cannot construct
that runtime plan.

An authorized plan exposes a bounded v1 access report containing the adapter,
support release and declaration bindings, selected contract/program versions,
per-relation bounds/counters, and bounded opaque-token traces. The report
contains no locator, native path, payload, or native identity value. A canonical
SHA-256 integrity digest covers all report content fields and detects mutation;
it is not a signature or a transferable source-access capability. The Rust
report shape, canonical v1 content encoding, and digest are frozen by the
shared `rfc012a-access-report-v1.json` fixture, which is evaluated by
independent Rust, Python, and TypeScript implementations. The exact IPC wrapper
and retrieval API remain provisional.

## 4. Logical subsystem dependency law

### 4.1 Logical subsystems

The common side is divided into these logical responsibilities:

| Subsystem                | Owns                                                                      |
| ------------------------ | ------------------------------------------------------------------------- |
| `model`                  | IDs, provenance, qualified values, facts, revisions, capability types     |
| `adapter-api`            | source/scope declarations, decoder traits, dispositions, support metadata |
| `source-runtime`         | authorized access, framing, discovery, cursors, generations, watching     |
| `decode-runtime`         | decoder invocation, decoder state, deterministic emission ordering        |
| `semantic-reducers`      | agent-independent fact/revision reduction                                 |
| `durable-projection`     | transactional projection intents and durable readiness inputs             |
| `observation-projection` | ephemeral typed revisions and actor/scope correlation                     |
| `store-query`            | migrations, one writer, read lanes, canonical queries, durable outbox     |
| `observer-api`           | database-free scope lifecycle, delivery, barriers, artifact mediation     |
| composition roots        | adapter registry plus durable-host or scoped-observer assembly            |

These names describe dependency roles, not required crates.

### 4.2 Allowed dependencies

The following directions are allowed:

| Consumer                 | May depend on                                                         |
| ------------------------ | --------------------------------------------------------------------- |
| Concrete adapter         | `model`, `adapter-api`                                                |
| `adapter-api`            | `model`                                                               |
| `source-runtime`         | `model`, declarative portions of `adapter-api`                        |
| `decode-runtime`         | `model`, `adapter-api`, `source-runtime`                              |
| `semantic-reducers`      | `model`                                                               |
| `durable-projection`     | `model`, `semantic-reducers`, a narrow storage port it owns           |
| `observation-projection` | `model`, `semantic-reducers`                                          |
| `store-query`            | `model`, `durable-projection`                                         |
| `observer-api`           | `model`, `source-runtime`, `decode-runtime`, `observation-projection` |
| Durable composition root | common subsystems, store/query, compiled adapter registry             |
| Scoped composition root  | common subsystems, observer API, compiled adapter registry            |

### 4.3 Forbidden dependencies

Architecture checks reject:

- `source-runtime -> store-query` or SQLite-specific code;
- concrete adapter imports from store, public query, durable projection, or
  observation delivery modules;
- `adapter-api` exposing database handles, SQL, writer transactions, public
  event queues, or query services;
- `observation-projection` or `observer-api` reading durable query state;
- `store-query` matching on concrete agent IDs or importing concrete adapters;
- `semantic-reducers` importing agent-native structs or source paths;
- public queries invoking adapter decoders, source access, hydration, or
  projection repair;
- physical-crate cycles hidden behind feature flags; and
- a reusable subsystem importing a composition root.

Only composition roots may know both a concrete adapter registry and a chosen
execution topology. This prevents the logical engine from becoming a God
Object while allowing incremental extraction from the current crate.

The durable projection storage port is dependency inversion: it is declared by
the projection boundary and implemented by `store-query`. It is not an import
from the store implementation, so the allowed edges do not form a cycle.

### 4.4 Common qualified values

The base model represents value knowledge as:

```text
QualifiedValue<T> {
  value?
  quality: Exact | NativeClaimed | Derived | Estimated | Unknown
  authority
  completeness: Complete | Partial | Unknown
  unknown_reason?: Missing | Unsupported | Withheld | NotYetObserved | Ambiguous | Malformed
  effective_at?
  provenance
}
```

The following well-formedness rules are normative across Rust, N-API, and
TypeScript representations:

1. `quality = Unknown` if and only if `value` is absent.
2. An unknown value carries an `unknown_reason`; a known value does not.
3. `completeness` describes coverage of the relevant domain, not confidence in
   the scalar. `Exact + Partial` is therefore valid for an exact subtotal over
   incomplete coverage.
4. `Complete` does not promote `NativeClaimed`, `Derived`, or `Estimated` to
   `Exact`.
5. `authority` is interpreted only by the versioned fact-family contract. It is
   not a universal ordering that unrelated families may compare.
6. `effective_at` is absent when native or derived evidence cannot establish an
   effective boundary.

An exact numeric zero is represented as `{value: 0, quality: Exact}` only when
native semantics prove zero. Missing, unsupported, withheld, and not-yet-seen
cannot be normalized to zero, empty text, or another ordinary value. Child RFCs
may further restrict combinations for a fact family but cannot redefine these
meanings.

### 4.5 Canonical identity contracts

RFC 012A owns identity that must be shared before either storage or observer
delivery is selected. RFC 012B owns catalog reconciliation and presentation
canonicalization; RFC 012C owns actor/run identity built on these base keys.

Canonical entity keys are semantic equivalents of:

```text
SourceInstanceKey {
  namespace_version
  stable_instance_discriminator
}

EntityKey {
  adapter_id
  source_instance_key
  entity_kind
  native_or_declared_fallback_key
}

SessionKey = EntityKey<Session>
ProjectKey = EntityKey<Project>
```

Rules:

1. `SourceInstanceKey` is stable across process restart and ordinary path
   spelling differences. It is derived from a declared source-owned
   installation/account identity, canonical local root identity, or a
   fixture-backed tuple—not a database row ID, registration order, process ID,
   or secret. Its public encoding is opaque and policy-safe; a raw canonical
   path is a separately authorized locator claim, not key material exposed to
   downstream clients.
2. A source instance is local evidence scope. Copying or observing similar data
   on another device does not imply cross-device sameness; a downstream system
   adds its device/source-replica identity. The ADS declares move, clone, and
   replacement behavior.
3. A support release declares one versioned derivation for every emitted entity
   kind. Raw vendor IDs are never assumed globally unique.
4. Evidence for the same native entity from transcripts, indexes, summaries,
   sidecars, durable ingestion, or scoped observation derives the same base key.
5. A fallback uses only stable, declared native attributes and has fixture-tested
   collision and replacement behavior. Random ingest-time IDs are prohibited.
6. An alias, `SameEntity`, move, or supersession relation never mutates a base
   key. A catalog may choose a canonical presentation representative while
   retaining every member key and relation.
7. A scoped attach must receive enough declared identity input to derive its
   final root `SessionKey` before installing watches. If it cannot, attachment
   fails with `InvalidRootIdentity`; provisional keys that later change are not
   allowed.
8. A tombstoned key cannot later identify a demonstrably different native
   entity. A source that reuses native IDs must include a declared stable
   incarnation discriminator or emit a replacement relation to a new key.
9. Changing a derivation is a semantic-version change with an identity migration
   and replay plan.

The persistable public reference is semantically equivalent to:

```text
ExternalEntityRef {
  external_entity_reference_version
  entity_key
}

NativeIdentityClaim {
  entity_ref: ExternalEntityRef
  identity: QualifiedValue<{
    native_namespace
    native_id
  }>
}
```

It survives engine restart and deterministic database rebuild/replay while the
declared native identity is unchanged, and it is accepted by the owning
versioned query/scope API. Its `entity_key` already includes adapter,
source-instance, and entity-kind namespace; a wire representation may also
expose those components for routing or diagnostics but cannot disagree with the
key.

`NativeIdentityClaim` is adjacent evidence, not part of external-reference
identity or equality. Its value is known only when evidence proves it and is
never the sole cross-provider or cross-device namespace. A policy-filtered
boundary may instead return a well-formed `Unknown/Withheld` qualified claim;
absence means that response makes no native-ID assertion. Resolution state
such as live, tombstoned, superseded, or unknown is owned by the relevant query
contract; resolution cannot silently retarget the reference.

Reference equality is semantic under one selected compatible reference
contract, not raw byte equality across arbitrary versions. A client combining
topologies negotiates a common version or uses an explicit versioned conversion;
it cannot guess across incompatible majors.

Source and fact identities are semantic equivalents of:

```text
SourceRecordId {
  adapter_id
  source_instance_key
  stream_key
  object_key
  generation
  logical_record_range_or_document_revision
  framing_contract_version
}

FactId {
  fact_contract_version
  adapter_id
  source_instance_key
  fact_kind
  fact_key:
    Native(stable_native_fact_key)
    | Derived(SourceRecordId + deterministic_semantic_subkey)
}

FactRevisionId {
  fact_id
  source_revision_or_semantic_revision
}
```

`observed_at`, scheduling order, startup tier, topology, queue sequence, and
projection destination never participate in these identities. When an adapter
emits several same-kind facts from one record, the semantic subkey or ordinal is
part of its versioned decoder contract and cannot depend on map iteration or
thread timing. A generation reset creates new record ownership; family-specific
reducers define whether it corrects, retracts, or supersedes prior facts.

`framing_contract_version` identifies the logical native-record framing rule,
not the selected primitive implementation. `DelimitedHead`, `DelimitedPrefix`,
and `AppendDelimited` views of the same logical record therefore use the same
framing contract and `SourceRecordId`.

### 4.6 Cross-topology semantic references and coverage

The public join reference for a typed native-derived semantic revision is:

```text
SemanticRevisionRef {
  semantic_reference_contract_version
  fact_revision_id: FactRevisionId
}
```

The wrapper permits a versioned public encoding without creating another
identity. Every durable query item and scoped observation event representing
the same typed revision carries an equal `SemanticRevisionRef`. A database row
ID, commit sequence, observer event ID, attachment ID, phase, or delivery
sequence cannot substitute for it. Engine-only lifecycle controls that do not
represent a semantic fact use their source/control identity and do not fabricate
a semantic revision reference. A consumer combining topologies negotiates one
compatible semantic-reference contract or uses an explicit conversion; it does
not compare incompatible serialized versions heuristically.

Coverage through native evidence uses semantic equivalents of:

```text
SourceCoveragePoint {
  coverage_contract_version
  coverage_domain: Decode | FactFamily(version) | ProjectionPack(version)
  adapter_id
  source_instance_key
  stream_key
  object_key
  generation
  position?:
    AppendCursor(opaque)
    | DocumentRevision(opaque)
    | SnapshotRevision(opaque)
    | DatabaseWatermark(opaque)
    | KeyRangeToken(opaque)
  status: CompleteThrough | ExactSnapshot | Partial | Unavailable(reason)
  provenance
}

SourceCoverageSet {
  coverage_set_contract_version
  coverage_domain
  scope: {
    adapter_id
    source_instance_key
    root_entity_key?
    support_release_id
    source_or_scope_declaration_digest
  }
  membership_revision
  points: SourceCoveragePoint[]
  explicit_absence_or_deletion[]
  explicit_errors[]
  completeness: Complete | Partial | Unavailable
}
```

Rules:

1. One point proves coverage only for its named object/domain. A
   `SourceCoverageSet` is required to claim scope/family completeness.
2. `Complete` means every object required or reachable under the declared scope
   at `membership_revision` is represented by sufficient complete coverage or
   an explicit proven deletion/absence. Errors remain listed, and an unavailable
   required object forces `Partial`/`Unavailable`; neither state can turn an
   omitted object into absence.
3. A common source/scope runtime defines membership revision and whether a newer
   complete set accounts for membership additions/removals. Registration or
   callback order cannot affect the set identity or completeness.
4. A common driver contract defines whether two positions are `Equal`,
   `Dominates`, `Behind`, or `Incomparable`; consumers cannot compare opaque
   values lexically or numerically.
5. Positions are comparable only within compatible coverage contract/domain,
   adapter, source instance, stream, object, and generation. A new generation
   does not automatically dominate the old one; reset/retraction or full
   replacement must establish that transition.
6. `CompleteThrough` applies to ordered append/database domains;
   `ExactSnapshot` applies to a complete replaceable snapshot. `Partial` and
   `Unavailable` cannot prove absence or subsumption. A complete status requires
   a position; partial/unavailable coverage may carry the last proven position
   but must label it non-current.
7. A durable `at_commit_seq` says when Spaghetti committed a result. An observer
   sequence says delivery order. Neither is a native coverage position, and no
   global scalar may be synthesized across unrelated objects.
8. Durable query and observer barrier contracts use this same coverage meaning,
   including family/projection version. They may serialize an opaque aggregate
   token or digest in addition, but not instead of the inspectable coverage and
   completeness required by the public contract.
9. Object keys and positions exposed outside the engine are opaque and
   policy-safe. Raw paths, database keys, or other sensitive locators require a
   separately authorized evidence field.

The common versioned model/SDK supplies a semantic operation equivalent to:

```text
compareCoverage(candidate_set, baseline_set)
  -> Equal | Dominates | Behind | Incomparable
```

The exact method shape is provisional. Its result must be deterministic across
Rust and supported client facades for the selected coverage contract. A
consumer cannot import adapter/driver code or decode the opaque position to
reimplement this comparison. Sets with incompatible scope, declaration,
support-release, family, membership, or position contracts return
`Incomparable`; an unsupported coverage version is incompatible, not
best-effort comparable.

## 5. Authoritative ingest seam

### 5.1 `SourceRecord`

A common source runtime emits a native evidence record with semantic
equivalents of:

```text
SourceRecord {
  record_id: SourceRecordId
  source_instance_key
  stream_key
  object_key
  generation
  cursor_start
  cursor_end
  record_index?
  revision?
  native_identity?
  observed_at
  media_type
  payload
  payload_hash
}
```

The common runtime owns framing, bounds, object identity, generation, cursor,
retry, and observation provenance. It does not interpret the native payload.

### 5.2 `FactBatch`

An adapter decoder receives one authorized `SourceRecord` plus versioned
decoder state and returns:

```text
FactBatch {
  facts[]                 # each has FactId, FactRevisionId, and ownership
  diagnostics[]
  retained_native_evidence?
  next_decoder_state?
  disposition
}
```

Facts use common semantic contracts owned by RFC 012B and RFC 012C. Both the
durable and scoped topologies invoke the same decoder revision for a given
record family. The decoder receives no startup-tier, projection-destination,
database-presence, or observer-delivery flag that could change emitted facts.

### 5.3 Mapping dispositions

Every complete record receives exactly one top-level disposition:

- `Mapped {fact_count}`;
- `IgnoredKnown {reason_code}`;
- `RetainedUnknown {family_hint, bounded_evidence}`;
- `BufferedIncomplete`;
- `Malformed {reason_code, bounded_diagnostic}`; or
- `UnsupportedVersion {observed_version}`.

Known ignored records are not reported as drift. Unknown families and fields
remain measurable without producing one unbounded diagnostic row per record.

### 5.4 Tier, view, and topology compositionality

Catalog head/prefix reads, later full-history reads, durable replay, and scoped
observation may overlap the same native bytes. That overlap cannot create a new
semantic identity or a different decode result.

For one support release and native watermark:

```text
decode(head_or_prefix) + decode(continuation)
  == decode(full_source)

durable FactBatch digest
  == scoped FactBatch digest
```

Equality is over ordered `SourceRecordId`, disposition, fact/revision identity,
semantic payload, qualified provenance, and final decoder state. It excludes
observation time and delivery/storage metadata.

A conforming durable composition uses one of these explicit strategies:

1. commit every fact from catalog-read records and build later projection packs
   from those committed facts;
2. reread overlap later using the same `SourceRecordId`/fact identities and
   idempotent projection effects; or
3. use a catalog-specific native record family with disjoint ownership proven
   by its ADS and fixtures.

The selected strategy is declared per stream/support release. A catalog cursor
cannot advance a shared cursor domain in a way that causes a later pack to skip
required facts. Decoder state is versioned per source object/generation and is
either continued exactly or deterministically reconstructed at the declared
safe boundary. Tier, urgency, and topology affect scheduling only.

## 6. Common source primitives

The first supported primitive set is:

| Primitive             | Common semantics                                                |
| --------------------- | --------------------------------------------------------------- |
| `AppendDelimited`     | complete logical records, partial suffix, cursor, generation    |
| `ReplaceDocument`     | bounded whole revision, replacement, deletion, stable hash      |
| `DirectoryMembership` | bounded child identity, add/change/remove, no symlink traversal |
| `PresenceObject`      | absence/create/update/remove/recreate                           |
| `SQLiteSnapshot`      | read-only bounded table/query snapshot and revision             |
| `KeyValueSnapshot`    | bounded key namespace and value revisions                       |
| `DelimitedHead`       | first complete record under a declared logical-record bound     |
| `DelimitedPrefix`     | bounded record/byte prefix when evidence requires more than one |

All primitives provide cancellation, access telemetry, object-level errors,
and explicit limits. Notifications are hints; reconciliation against source
identity and cursor/revision is authoritative.

An adapter may choose different primitive compositions for catalog, durable
background ingestion, and scoped observation. It cannot replace their
lifecycle semantics.

## 7. Restricted scoped-relation declarations

### 7.1 Rule

A scoped observer receives a declarative `ScopeProgram`; it does not call an
adapter function that can freely walk the filesystem or query a database.
Every relation declares its access root, fan-out/depth/byte bounds, identity
inputs, and unavailable behavior.

One bounded common-runtime execution selects one program; it does not merge
relations from several programs. Relation IDs are unique across the declaration
so every access and denial has one unambiguous budget and trace owner. Named
identity inputs must match the declared names exactly before their values are
hashed into an opaque object token. Native identity values never enter access
telemetry.

The initial relation vocabulary is:

| Relation primitive            | Meaning                                                      |
| ----------------------------- | ------------------------------------------------------------ |
| `KnownObject`                 | one explicitly supplied and canonicalized locator            |
| `SiblingObject`               | fixed relative object under an authorized parent             |
| `ChildDirectoryByNativeId`    | bounded child membership selected by a known native identity |
| `ReferencedObjectFromField`   | locator/key read from an already authorized decoded record   |
| `BoundedIndexLookup`          | bounded lookup in a declared index object                    |
| `ParameterizedSQLiteRows`     | fixed statement shape parameterized by known identity values |
| `KeyNamespace`                | bounded prefix/range beneath an authorized key namespace     |
| `ArtifactLocatorFromEvidence` | bounded artifact identity discovered from an in-scope fact   |

Globally recursive discovery, arbitrary SQL, unconstrained globs, dynamic
absolute paths from untrusted payloads, and “search until found” are not
primitives.

The initial operation mapping is closed:

| Relation primitive                         | Permitted common access operation              |
| ------------------------------------------ | ---------------------------------------------- |
| `ParameterizedSQLiteRows`                  | parameterized read-only query                  |
| `ChildDirectoryByNativeId`, `KeyNamespace` | bounded object listing and bounded object read |
| every other initial relation primitive     | bounded object read                            |

Adding another operation to a primitive changes this contract and requires an
RFC 012A amendment plus cross-language conformance evidence.

### 7.2 Agent-specific joins

Some relationships require native interpretation. Adapter join code may:

- consume only records and fact identities already supplied by the common
  runtime;
- return candidate native identities or relation parameters, not opened files
  or database rows;
- use deterministic, fixture-tested logic;
- declare maximum output fan-out; and
- report ambiguous/unavailable rather than broadening scope.

The common access guard canonicalizes and authorizes every resulting locator
before a driver opens it.

### 7.3 Auditability

Conformance records every object opened, directory entry enumerated, SQLite
statement shape and row count, KV key range, bytes read, relation that
authorized the access, and bound consumption. A scope fails conformance if it
touches an unrelated object even when its emitted facts happen to be correct.

One `ScopeAccessPlan` instance is the budget ledger for one bounded
reconciliation pass. `Initial` and `Revalidation` reservations in that pass
consume the same declared totals; a phase label does not refill a budget.
Creating a fresh plan is a common-runtime lifecycle action for a later bounded
pass, never an adapter retry escape hatch. A declaration that requires a
worst-case second content read during revalidation must budget both reads or use
a proven cheaper common primitive. Per-observer scheduling/rate and total
resource ceilings remain separate RFC 012D runtime limits.

Raw relation-ledger snapshots are not a host diagnostic API. Runtime retrieval
uses the authorized bounded access report, which binds the trace to the selected
support release, scope declaration, program, and observation contract. A future
wire/IPC representation must preserve that binding and verify the report digest
before treating it as conformance evidence.

## 8. Agent Data Surface

Each support candidate has a versioned Agent Data Surface document containing:

- native artifact identity, version markers, platforms, and relevant flags;
- stable source-instance derivation, canonical roots, move/clone/replacement
  behavior, and access policy;
- native object families and source primitive composition;
- record framing, partial-write, append, replacement, rotation, deletion, and
  compaction semantics;
- project, session, actor, response, task, interaction, team, workflow, and
  artifact identity rules where applicable;
- native session/project association and locator bases where applicable;
- cross-object joins and precedence evidence;
- scope-program declarations and access bounds;
- maximum observed and supported logical-record/document sizes;
- timestamp/value quality and missing-versus-zero meaning;
- unknown-family and version-drift detection;
- privacy, retention, and sanitization requirements; and
- claim-addressable links to captures, probes, and fixtures.

The ADS describes observed native behavior. It does not redefine public facts,
queries, or observer delivery semantics.

## 9. Standard adaptation workflow

### 9.1 Stage 0: scope and identities

Define supported platforms, native artifact/version target, canonical roots,
entity identities, expected capabilities, privacy policy, and explicit
non-goals before implementing a decoder.

### 9.2 Stage 1: acquire and census

Pin or hash the distributable/native artifact. Inventory object families,
record discriminants, identifier/timestamp fields, rewrite behavior, size
distributions, and unknown families using read-only tooling.

### 9.3 Stage 2: capture and sanitize

Produce controlled native scenarios for creation, append, partial write,
replacement, deletion, descendant creation, tasks, interactions, teams,
workflows, and failure cases. Raw captures remain private. Sanitization is
deterministic, reviewable, and checked for prohibited data.

### 9.4 Stage 3: model the ADS

Write the source, identity, join, scope, bounds, quality, version, and drift
claims before implementing the adapter.

### 9.5 Stage 4: build an independent simulator

The simulator creates native source states and time-ordered transitions without
calling adapter decoders or importing adapter structs. Static final files alone
cannot prove cursor, generation, watcher, scope, or observer behavior.

### 9.6 Stage 5: implement

Compose common primitives, register bounded declarations, decode records, and
return explicit dispositions. The adapter does not implement surrounding
runtime mechanics.

### 9.7 Stage 6: conform

Run source, decoder, identity, version, cold/live/restart, malformed/unknown,
scope-access, deterministic-digest, and performance conformance against the
independent corpus.

### 9.8 Stage 7: promote and monitor

Promote only after protected evidence, sanitizer, ADS, fixture, conformance,
and performance review. Later drift creates a new candidate; it cannot mutate a
promoted release in place.

## 10. Version and runtime compatibility policy

### 10.1 Independent version axes

A support release binds five independent axes:

1. vendor artifact/version range and platform;
2. ADS version and digest;
3. adapter decoder and decoder-state versions;
4. Spaghetti durable fact/projection/query versions; and
5. observation scope/envelope/event/lifecycle versions.

### 10.2 Runtime classes

The installed native artifact is classified before semantic decoding:

| Class                   | Definition                                                        |
| ----------------------- | ----------------------------------------------------------------- |
| `ExactSupported`        | exact promoted artifact/version and required markers              |
| `RangeSupported`        | inside a promoted, fixture-backed compatibility range             |
| `RecognizedUnverified`  | recognizable family/version outside every promoted range          |
| `UnknownOrIncompatible` | missing/contradictory markers or known incompatible native layout |

### 10.3 Required behavior

| Operation                | Exact/range supported   | Recognized unverified                                       | Unknown/incompatible          |
| ------------------------ | ----------------------- | ----------------------------------------------------------- | ----------------------------- |
| Bounded version probe    | Allowed                 | Allowed                                                     | Allowed                       |
| Catalog discovery        | Supported as declared   | Only an explicitly declared forward-compatible catalog path | Unavailable                   |
| Durable history/runtime  | Supported as declared   | Disabled                                                    | Disabled                      |
| Scoped typed observation | Supported as declared   | Disabled                                                    | Disabled                      |
| Bounded drift evidence   | Per retention policy    | Hash/count/field-family diagnostics; no durable raw payload | Version/root diagnostic only  |
| Compatibility output     | supported class/release | unverified class, observed version, and reason              | incompatible class and reason |

A support release may declare a forward-compatible catalog decoder only when
the ADS identifies stable version markers, strict bounds, and fixtures proving
that the catalog identity surface is tolerant. Runtime flags cannot upgrade an
unverified artifact to full semantic support. Support requires a new promoted
release or range expansion.

RFC 012A publishes compatibility class, selected support release, permitted
source plans, and structured reasons. It does not publish RFC 012B readiness or
RFC 012C/012D capability state. Those owners map this compatibility output into
their own state machines without upgrading the permitted behavior above.

### 10.4 Public contract compatibility

Every Rust/N-API/IPC/TypeScript boundary selects an explicit compatible set of
base-model and fact-family versions before returning typed data. Version `1`
uses three distinct shapes:

```text
ContractVersionRequest {
  selection_contract_version
  model_major
  external_entity_reference_version
  semantic_revision_reference_version
  coverage_contract_versions[]
  fact_family_versions{family -> preferred_versions[]}
  query_pack_versions[]?
  observation_contract_versions[]?
}

ContractVersionOffer {
  selection_contract_version
  model_major
  external_entity_reference_versions[]
  semantic_revision_reference_versions[]
  coverage_contract_versions[]
  fact_family_versions{family -> offered_versions[]}
  query_pack_versions[]
  observation_contract_versions[]
}

ContractVersionSelection {
  selection_contract_version
  model_major
  external_entity_reference_version
  semantic_revision_reference_version
  coverage_contract_version
  fact_family_versions{family -> version}
  query_pack_version?
  observation_contract_version?
}
```

Request arrays are ordered consumer preferences. Selection takes the first
offered value, requires every requested fact family, rejects zero/duplicate or
empty required version sets, and never silently drops a requested family.
An incompatible semantic major fails before source access or typed delivery.
An additive minor may be selected only when the older side can preserve unknown
variants as a bounded typed-unknown value rather than silently dropping them.
Native unknown evidence and an unknown transport/schema variant are distinct.
Child query and observer contracts embed or reference this selection rather
than redefining it. Silent best-effort decoding across incompatible versions
is forbidden.

## 11. Support ledger

Each candidate/current/retired entry records at least:

- adapter ID and support-release ID;
- vendor artifact/range/platform digests;
- ADS and sanitized-fixture digests;
- decoder and decoder-state versions;
- entity/source-record/fact identity contract versions;
- external-entity-reference, semantic-revision-reference, and coverage contract
  versions;
- durable and observation contract versions;
- supported/degraded/unsupported capabilities by topology;
- source/scope declaration digests and bounds;
- per-stream tier-overlap strategy and safe decoder-state boundary;
- conformance and performance report digests;
- sanitizer review;
- promotion/retirement time and superseding release; and
- known limitations and drift signatures.

The ledger representation may evolve, but these meanings and independent
version axes are normative.

## 12. Conformance and gates

Release-blocking tests cover:

- valid and invalid `QualifiedValue` combinations across Rust, N-API, and
  TypeScript;
- exact, ranged, unverified, and incompatible version classification;
- compatible selection and incompatible rejection at public contract
  boundaries;
- every non-empty native family receiving one disposition;
- deterministic fact/revision IDs independent of map iteration or scheduling;
- stable source-instance and external entity references across restart,
  deterministic rebuild/replay, registration reorder, and ordinary path
  spelling changes;
- declared source-instance move/clone/replacement and cross-device non-merge
  behavior;
- non-reuse of tombstoned entity references and explicit supersession;
- authorized versus withheld native identity claims in external references;
- identical `SemanticRevisionRef` values in durable and scoped representations;
- coverage comparison for equal/dominating/behind/incomparable positions,
  generation changes, partial state, unavailable objects, and Rust/client
  facade parity;
- coverage-set membership addition/removal, omitted required objects, explicit
  deletion, declaration/support-release mismatch, and callback reorder;
- identical entity/source-record/fact identities across catalog, full-history,
  durable, and scoped views;
- head/prefix plus continuation versus full-only ordered record, fact,
  decoder-state, and final reducer digests;
- partial writes, replacement, truncation, deletion, and recreation;
- bounded source and scoped-relation access, including denial of traversal and
  unrelated-root enumeration;
- source bounds at, below, and above their declared limits;
- identity collisions and ambiguous joins;
- cross-version fixtures and decoder-state upgrades;
- independent expected outputs and final identity digests;
- raw-capture prohibition and sanitizer checks; and
- architecture checks for every forbidden dependency edge.

The new-agent proof is complete when an additional adapter can implement its
ADS, declarations, decoder, fixtures, and support entry without modifying
source runtime, store/query, observer lifecycle, or existing agent switches.

## 13. Logical repository structure

The repository should make these ownership boundaries visible, but this RFC
does not require final crate names. A conforming structure has identifiable
locations for:

```text
common model and adapter API
common source/decode runtime
common semantic reducers
durable projection and store/query
observation projection and observer API
agent support packages
  ADS
  source/scope declarations
  decoder
  sanitized fixtures
  support ledger entry
independent conformance tooling
```

The initial repository realization uses `agent-support/<adapter>/<candidate>/`
for the five independently versioned contract/evidence documents and sanitized
fixtures, `agent-support/schemas/` for their strict schemas, and
`scripts/agent_support/` for independent validation, sanitization, and
access-budget conformance. Runtime support classification, contract selection,
and typed-access authorization live in the common Rust adapter boundary;
Python and the portable SDK verify the shared conformance fixture. Directory
presence never promotes support: only a digest-bound ledger entry with
`status: promoted` is selectable.

Physical extraction follows dependency stability. Moving files without
enforcing the logical dependency checks does not satisfy this RFC.

## 14. Security and privacy

- Raw production captures are never committed.
- Source access is canonicalized, bounded, and attributable to a declaration.
- Symlink/path escape and arbitrary database-query construction are rejected.
- Experiment reports contain aggregates and hashes, not native paths, IDs,
  prompts, titles, questions, or payloads.
- External references and coverage expose opaque policy-safe identities; raw
  local paths remain separately authorized evidence.
- Support-ledger drift diagnostics do not retain unverified native payloads by
  default.
- Artifact content policy is enforced by the common observer contract in RFC
  012D.

## 15. Rejected alternatives

### 15.1 Unrestricted adapter discovery functions

Rejected because a function returning arbitrary paths or SQL results cannot
prove bounded scope, prevent global search, or produce mechanical access
conformance.

### 15.2 Adapter-owned watchers and cursors

Rejected because agents would diverge on partial writes, replacements, retry,
overflow, reset, and cancellation.

### 15.3 One monolithic common-engine module

Rejected because agent independence alone does not prevent store, source,
reducer, observer, and query coupling.

### 15.4 Treat package version as support version

Rejected because vendor artifact, ADS, decoder, durable semantics, and
observation semantics evolve independently.

### 15.5 Optimistically decode unknown vendor versions

Rejected because silent forward compatibility can corrupt canonical facts and
live operational state. Limited forward-compatible catalog discovery must be
explicitly declared and fixture-backed.

## 16. Acceptance criteria

Ratifying the A1/A2 foundation contracts may unblock initial RFC 012B/C/D
vertical slices. Full RFC 012A completion additionally requires the later A4
new-agent proof after those public semantic seams stabilize.

RFC 012A is complete when:

- logical dependency checks enforce every forbidden edge;
- source and scope declarations are bounded and mechanically auditable;
- all current agents have ADS documents, sanitized fixtures, decoder contracts,
  and candidate support-ledger entries;
- independent simulators cover time-ordered source changes;
- runtime version classification publishes the required degraded/unavailable
  behavior;
- no adapter owns watchers, cursors, queues, SQL, public queries, readiness, or
  artifact reads;
- every native family has a deterministic mapping disposition;
- base entity, source-record, fact, and revision identities are stable across
  tiers and topologies;
- external entity references survive restart without silent reuse, and durable
  and scoped typed revisions expose the same semantic revision references;
- common coverage points/sets prevent commit, delivery, and native-source
  clocks from being compared as if they were one ordinal;
- every declared tier-overlap strategy passes the compositionality law;
- the current-agent conformance matrix passes; and
- the new-agent proof requires no common-runtime, public-query, or observer
  lifecycle modification.

# RFC 012 parallel-work handoff

- **Status:** Ready to assign
- **Written:** 2026-08-16
- **Audience:** A second implementation agent working concurrently with the RFC
  012D scoped-observation lane
- **Primary references:** [implementation plan](./012-implementation-plan.md),
  [RFC 012A](./012a-agent-adaptation-and-engine-boundaries.md),
  [RFC 012C](./012c-runtime-semantics-and-usage-v2.md), and the
  [RFC 011 delta ledger](../../scripts/architecture/rfc012-rfc011-delta.json)

## 1. Recommended assignment

Assign **P1: portable runtime value-contract fixtures** first. It is a bounded
slice of implementation-plan workstream C1. It advances the portable contract
and catches Rust/TypeScript drift, but it does not block or overlap the current
RFC 012D kernel/control-lane work.

If P1 is accepted and committed early, the same agent may continue with **P2:
RFC 011 delta evidence audit**. P2 is deliberately read-only with respect to
the executable ledger: it reports evidence and gaps for later owner review but
does not promote any release gate.

Do not assign the parallel agent observer envelopes, epochs, overflow/resync,
replacement snapshots, queueing, or barrier semantics. Those contracts are on
the active critical path and are changing together.

## 2. Concurrency and repository rules

At the time this packet was written, the last shared implementation commit was
`433d98b` (`feat: observe scoped source lifecycle`). The primary agent may have
advanced `HEAD` or left unrelated working-tree edits by the time this packet is
started.

Before editing, the parallel agent must:

1. Read this document completely.
2. Run `git status --short` and `git rev-parse HEAD`; include both in the
   handback report.
3. Determine whether it is in the same working tree as the primary agent.
4. If the tree is shared, **do not** switch branches, rebase, stash, clean, or
   restore files. Existing modifications are somebody else's work.
5. If it has an isolated worktree, branch from the latest shared commit named
   by the user and report that base. Do not independently merge or rebase while
   the task is active.

The following paths are owned by the primary agent and are forbidden in both
packets:

- `crates/spaghetti-napi/src/scoped_observation.rs`
- `crates/spaghetti-napi/src/adapter/registry.rs`
- `docs/rfcs/012-implementation-plan.md`
- `docs/rfcs/012d-session-scoped-observation.md`
- `scripts/architecture/rfc012-rfc011-delta.json`
- everything under `draft/`

Do not use `git add -A`, `git add .`, or a broad formatter. Stage only the
explicitly owned files for one packet, inspect `git diff --cached`, and make a
separate commit per packet. Never stage a pre-existing modification or
untracked file.

If completing a packet appears to require a forbidden file, a schema or
database migration, a native decoder change, or a new observation wire
decision, stop and return the issue as a proposed follow-up. Do not cross the
ownership boundary.

## 3. P1 — Portable runtime value-contract fixtures

### 3.1 Objective

Create one deterministic, sanitized RFC 012C v1 fixture that Rust produces and
validates and the portable TypeScript SDK independently parses and validates.
Cover only these already-landed value families:

- `runtime.actor-run` through `ActorRunRevisionFact`;
- `runtime.actor-affiliation` through
  `ActorAffiliationRevisionFact`; and
- `runtime.usage-v2` through `UsageRevisionV2Fact`.

The fixture is a portability and validation slice, not the complete C1 exit
gate. In particular, it must not define observer envelopes, lifecycle control
events, scope epochs, complete replacement snapshots, reducer ownership, or
durable query pages.

### 3.2 File ownership

P1 may create or edit only these files:

- new
  `crates/spaghetti-napi/fixtures/contracts/rfc012c-runtime-v1.json`;
- new
  `crates/spaghetti-napi/src/adapter/runtime_contract_fixture.rs`;
- `crates/spaghetti-napi/src/adapter/mod.rs`, solely to add
  `#[cfg(test)] mod runtime_contract_fixture;`;
- new `packages/sdk/src/contracts/rfc012c.ts`;
- new `packages/sdk/src/contracts/__tests__/rfc012c.test.ts`;
- `packages/sdk/src/index.ts`, solely to export the new RFC 012C contract
  module; and
- optionally `crates/spaghetti-napi/fixtures/README.md`, solely to add one
  short entry for the new generated contract fixture.

No other file is in scope. If a filename already exists when the packet starts,
stop and report the collision before modifying it.

### 3.3 Contract boundary

The fixture must have a top-level `fixture_contract_version: 1` and identify
each covered family and family version explicitly. Its examples must include:

- one root actor and one child actor with valid parent linkage;
- the same child actor simultaneously affiliated with a team and a workflow,
  proving that affiliation dimensions are orthogonal;
- at least one `removed` affiliation revision;
- one usage revision with a native message ID as the response identity;
- one source-record fallback response with no native message ID;
- exact-zero usage, a known non-zero value, and an
  `unknown`/`missing` bucket;
- model and effort values with their authority, quality, completeness, and
  provenance;
- an A -> B -> A sequence for one response, with distinct semantic revision
  identity for A and B and the same semantic revision identity for both A
  occurrences; and
- the canonical semantic revision metadata needed to show that opaque
  references, not database IDs or paths, cross the boundary.

All names and payload text must be synthetic. The fixture must contain no home
directory, absolute path, prompt, transcript text, real native identifier, or
captured user data.

Use existing RFC 012A portable primitives from
`packages/sdk/src/contracts/rfc012a.ts`, including
`parseOpaqueContractReference`, `parseSemanticRevisionRef`, and
`parseQualifiedValue`. Do not add a second opaque-reference format or decode an
opaque reference.

Use the Rust implementations as the semantic source of truth. The Rust test
must construct the canonical keys and usage semantic revision keys through the
existing constructors/methods and compare them with the committed fixture.
Opaque references and expected digests must not be invented manually.

For this packet, token values may use the existing JSON number representation
only when they are non-negative JavaScript-safe integers. The TypeScript parser
must reject unsafe integers instead of rounding them. If full `u64` transport
is required, return that as a contract-design follow-up; do not silently choose
decimal strings, bigint tags, or a new N-API representation in this packet.

### 3.4 TypeScript API

`rfc012c.ts` should expose typed v1 wire values and focused parsers for the
three families. Keep the API value-oriented. A suitable shape is:

```ts
export const RUNTIME_SEMANTIC_CONTRACT_VERSION = 1 as const;

export function parseActorRunRevision(value: unknown): ActorRunRevision;
export function parseActorAffiliationRevision(value: unknown): ActorAffiliationRevision;
export function parseUsageRevisionV2(value: unknown): UsageRevisionV2;
export function parseRuntimeContractFixture(value: unknown): RuntimeContractFixture;
```

Names may follow established repository conventions, but do not expose a
session observer or event-stream API from this packet.

Parsing must return newly validated values rather than relying on a TypeScript
cast. It must reject at least:

- an unknown contract major/version;
- malformed or wrong-version opaque references;
- a root actor with a parent;
- a child actor without a parent or an actor parented to itself;
- an unsupported actor role, affiliation dimension, or affiliation state;
- empty optional native IDs when present;
- a native-message response whose `response_key` does not equal
  `native_message_id`;
- a fallback response that claims `native_message_id`;
- a response key that is empty, malformed/non-canonical padded standard
  base64, or encoded as the legacy byte-array form;
- an empty request ID;
- a negative, fractional, non-number, or unsafe token count;
- a usage authority outside `native_response | adapter_derived`;
- empty provenance `native_field` or a zero/non-integer normalization contract
  version;
- an `unknown` qualified value with data or without an unknown reason; and
- an unknown value claiming complete coverage, or a known qualified value
  without data or with an unknown reason.

Do not reject unknown object members merely because this v1 parser does not
interpret them; forward-compatible raw retention is owned elsewhere. Do reject
invalid values for fields that this contract understands.

### 3.5 Rust fixture test

The new Rust file is a test-only sibling module. It must:

1. deterministically construct the expected actor, affiliation, usage, and
   semantic-reference values with existing public or crate-private APIs;
2. deserialize the committed JSON fixture into an explicit local fixture wire
   struct;
3. validate the payloads through the existing fact/batch contract rather than
   serde shape alone;
4. compare the deserialized fixture with the Rust-constructed expected value;
5. serialize and deserialize again and assert semantic equality;
6. recompute every committed usage semantic revision key and compare it with
   the fixed expected reference/digest; and
7. explicitly prove the A -> B -> A revision-identity behavior.

Do not make `validate()` methods public merely to support the test. Keep the
test inside the sibling module or validate by passing values through the
existing `FactBatch` boundary.

### 3.6 P1 exit gate

P1 is complete only when all of the following are true:

- Rust and TypeScript consume the exact same committed fixture.
- Rust derives every opaque key and expected usage semantic revision identity.
- TypeScript independently validates every accepted field; the test is not a
  fixture cast or snapshot-only assertion.
- The positive fixture covers every bullet in section 3.3.
- Focused negative tests cover every rejection class in section 3.4.
- No native path, database/runtime ID, or agent-specific actor kind enters the
  portable values.
- No observer envelope, queue, epoch, barrier, or replacement protocol is
  introduced.
- No existing decoder, database, schema, query, observer, or reducer behavior
  changes.
- Only P1-owned paths are staged and committed.
- These commands pass from the repository root:

```bash
cargo test -p spaghetti-napi runtime_contract_fixture
cargo fmt --all -- --check
pnpm --filter @vibecook/spaghetti-sdk test
pnpm --filter @vibecook/spaghetti-sdk typecheck
pnpm validate
git diff --check
```

If `pnpm validate` fails solely because of a known pre-existing concurrent edit,
the agent must still run and report every focused command, identify the exact
unrelated failure, and leave the packet unclaimed as complete until the primary
agent can reproduce it on the combined tree.

## 4. P2 — RFC 011 delta evidence audit

### 4.1 Objective

Audit every evidence item still marked `planned` in
`scripts/architecture/rfc012-rfc011-delta.json`. Produce an evidence map that
lets the owning agent decide which items can be promoted, which need stronger
tests, and which remain unimplemented.

This is an audit, not authorization to change the executable ledger or program
status. A test that happens to resemble a claim is not enough: the audit must
show that the test exercises the exact retained, strengthened, amended,
refined, or superseded behavior.

### 4.2 File ownership

P2 may create exactly one file:

- `docs/rfcs/012-rfc011-delta-evidence-audit-2026-08-16.md`

It may not edit code, tests, RFCs, the implementation plan, or the JSON ledger.

### 4.3 Required audit format

The audit must list every `planned` evidence entry present at the recorded base
commit exactly once. For each entry, record:

At the authored base there are 12 planned evidence entries. Recompute and
record the count at task start rather than assuming it stayed at 12.

| Field             | Requirement                                             |
| ----------------- | ------------------------------------------------------- |
| Ledger ID         | Exact `X0-*` ID and disposition                         |
| Planned claim     | Exact claim summarized without changing its meaning     |
| Classification    | `implemented-and-executable`, `partial`, or `not-found` |
| Evidence          | Repository path plus exact test/check/symbol name       |
| Reproduction      | Smallest exact command that exercises the evidence      |
| Semantic gap      | What the evidence does not prove                        |
| Recommended owner | A/B/C/D/X workstream and concrete next action           |

Claims based only on prose, type presence, or a passing broad test suite must
be classified `partial` unless an executable assertion enforces the target
behavior. If multiple tests collectively establish one claim, spell out each
part of the composition.

End the document with:

- a count reconciliation: planned entries audited = implemented-and-executable
  - partial + not-found;
- a proposed ledger patch list, written as recommendations only;
- the exact commands run and their outcomes; and
- any claim whose disposition or owner appears internally inconsistent.

### 4.4 P2 exit gate

P2 is complete only when:

- every planned evidence entry at the recorded base appears exactly once;
- every `implemented-and-executable` classification cites a focused executable
  assertion, not only an implementation file;
- every `partial` classification names the missing assertion;
- every `not-found` classification lists the searches performed;
- the ledger and all implementation status remain unchanged;
- only the single P2-owned document is staged and committed; and
- these commands pass:

```bash
python3 scripts/architecture/check_rfc012_delta.py
pnpm validate
git diff --check
```

P2 must not run `check_rfc012_delta.py --require-complete` as an expected-pass
gate: planned items intentionally make release mode fail until their owners
accept executable evidence.

## 5. Handback contract

For each packet, return one concise report with:

```text
Packet: P1 or P2
Base commit: <sha>
Result commit: <sha>
Changed files: <exact list>
Focused commands: <command and pass/fail result>
Full validation: <command and pass/fail result>
Exit gate: PASS or NOT MET
Unresolved design questions: <none or bounded list>
Pre-existing/concurrent files left untouched: <exact list from git status>
```

Do not update the central implementation plan, promote X0 evidence, merge the
commit, or begin another workstream. The primary agent will review the commit,
rerun the relevant tests on the combined tree, and make any status/ledger
change after semantic acceptance.

## 6. Primary-agent review gate

When the packet returns, the primary agent will not accept it from a green test
summary alone. Review will verify:

1. commit and file-scope isolation;
2. no accidental inclusion of concurrent work or `draft/`;
3. Rust-derived opaque identities and A -> B -> A usage semantics;
4. independent TypeScript parsing and all required negative cases;
5. no premature observer/replacement wire commitment;
6. sanitizer safety and absence of real captured data;
7. focused and repository-wide validation on the combined tree; and
8. whether any implementation-plan or X0 status change is actually justified.

## 7. Copy/paste kickoff prompt

```text
Work only on P1 in docs/rfcs/012-parallel-work-handoff.md. Read the full
handoff before editing, obey its shared-worktree and file-ownership rules, and
stop at the P1 handback contract. Do not touch the implementation plan, RFC
012D, scoped_observation.rs, the executable X0 ledger, or draft/. Commit only
P1-owned files and return the exact exit-gate report. If P1 is already present
or requires a forbidden contract decision, stop and report the collision.
```

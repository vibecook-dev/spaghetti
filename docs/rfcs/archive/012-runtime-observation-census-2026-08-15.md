# RFC 012 Phase 0B: runtime observation and usage census — 2026-08-15

- **Status:** Static semantic census complete; dynamic scoped-observer
  conformance remains an implementation gate
- **Architecture:**
  [RFC 012](../012-evidence-backed-adapters-and-progressive-readiness.md)
- **Plan:** [RFC 012 implementation plan](./012-implementation-plan.md)
- **Contract consumers:** [RFC 012C](../012c-runtime-semantics-and-usage-v2.md)
  and [RFC 012D](../012d-session-scoped-observation.md)
- **Related evidence:**
  [catalog and diagnostic census](./012-phase-0-census-2026-08-15.md)

## 1. Questions

This experiment and code audit answer five questions raised by the Chopsticks
runtime-consumer review:

1. Does the current Spaghetti API provide a database-free, session-scoped
   replacement for `watchSessionTranscript`?
2. Are Claude usage-bearing assistant rows independent additive usage, or
   revisions of one native response?
3. Can `requestId` identify a usage response by itself?
4. Does native transcript evidence contain model, effort, mode, plan/task, and
   structured user-input signals useful to a runtime observer?
5. Is the root/descendant/team/workflow source topology small and explicit
   enough to scope without scanning every configured Claude project?

It does not claim that watcher ordering, bootstrap barriers, reset delivery,
backpressure, or latency are correct. Those require the production common
observer and the dynamic conformance matrix in RFC 012.

## 2. Method

### 2.1 Independent native census

`scripts/runtime_observation_census/` reads the Claude JSONL objects selected
by the current parent and subagent transcript declarations:

- `projects/*/*.jsonl`; and
- `projects/*/*/subagents/**/agent-*.jsonl`.

It does not import Spaghetti's SDK, adapter, facts, or projectors. Every
complete JSONL record is decoded with the standard JSON library. A partial
final record is held rather than parsed.

Usage is grouped by `(source object, message.id)`. The source object is part of
the key so a vendor identifier reused in another transcript does not collide.
For every group the experiment retains the first and last four-bucket usage
snapshot, row count, changed-counter revisions, exact repeats, downward
corrections, model/effort presence, and `requestId` relationships. It also
computes the current per-row-delta total for comparison.

The experiment counts relevant tool and result shapes, but never emits tool
inputs, results, questions, answers, identifiers, model values, or transcript
paths. The report contains a SHA-256 digest of relative file metadata so a
later run can detect corpus drift without publishing the file set.

### 2.2 API and projector audit

The audit checked:

- `packages/sdk/src/sources/claude-code/live/session-tail.ts` and its tests;
- `packages/sdk/src/observation-host.ts`;
- the compatibility subscription in
  `packages/sdk/src/observation-service.ts`;
- Claude message/usage decoding in
  `crates/spaghetti-napi/src/claude/adapter.rs`;
- usage contribution projection in
  `crates/spaghetti-napi/src/engine/projection.rs`; and
- Chopsticks' pinned SDK dependency and transcript observer.

### 2.3 Reproduction

Focused tests:

```sh
python3 scripts/runtime_observation_census/test_census.py
```

Live read-only census:

```sh
python3 scripts/runtime_observation_census/census.py \
  --out /private/tmp/spaghetti-runtime-observation-census-live.json
```

Equivalent package commands are `pnpm test:runtime-observation-census` and
`pnpm experiment:runtime-observation-census`.

## 3. API audit result

The regression risk in the downstream review is confirmed:

1. `watchSessionTranscript` is intentionally a lightweight, single-file,
   no-SQLite tail and supports attaching before the file exists.
2. Its first cold bootstrap is reported as `rewrite: true`; cold bootstrap and
   a real source reset are therefore ambiguous.
3. `openObservationHost` requires a database path plus configured source
   roots and runs the persistent whole-source topology.
4. The compatibility subscription emits an empty change set as a global
   invalidation, not decoded semantic runtime events.
5. Chopsticks currently wraps `watchSessionTranscript`, pokes `poll()` from
   native hook arrival, and pins `@vibecook/spaghetti-sdk` 0.5.16.

RFC 012 before this amendment could therefore be implemented while deleting
the only database-free API used by Chopsticks. Sharing a decoder did not imply
sharing a usable execution topology.

## 4. Selected live-corpus result

The selected run used the live corpus on the RFC 012 reference machine. The
source-set digest was
`45734781b8645be0e3d2b3bc7bf4b664b65af7366fa846a314c17a4e03c9cd89`.
No file changed during the scan.

### 4.1 Input and actor topology

| Metric                               |        Result |
| ------------------------------------ | ------------: |
| Declared transcript objects          |         5,178 |
| Root transcript objects              |           382 |
| Standard child transcript objects    |         4,043 |
| Workflow child transcript objects    |           753 |
| Complete records                     |     1,092,690 |
| Root-path records                    |       663,308 |
| Child-path records                   |       429,382 |
| Bytes read                           | 2,740,414,987 |
| Malformed complete / partial records |         0 / 0 |
| Wall time, warm filesystem cache     |      16.652 s |

Related bounded objects visible under the same native surface were:

| Object family           | Count |
| ----------------------- | ----: |
| Subagent metadata       | 2,533 |
| Todo documents          | 2,459 |
| Workflow run documents  |    47 |
| Workflow journals       |    47 |
| Team configurations     |    26 |
| Team inbox documents    |    36 |
| Task item documents     |    44 |
| Plan documents          |    36 |
| Active-session presence |     5 |

This does not prove every native join, but it confirms that a known root can
expand through declared session relationships. The scoped observer does not
need to enumerate unrelated project transcripts.

### 4.2 Usage revisions

| Metric                                           |  Result |
| ------------------------------------------------ | ------: |
| Usage-bearing assistant rows                     | 342,861 |
| File-scoped `message.id` response groups         | 149,077 |
| Repeated rows beyond the first                   | 193,784 |
| Groups with more than one row                    | 106,451 |
| Groups with changed counters                     |  57,150 |
| Exact repeated rows beyond the first             | 136,414 |
| Groups with a downward correction                |     111 |
| Rows without `message.id`                        |       0 |
| Rows without `requestId`                         |     268 |
| File-scoped request IDs mapping to >1 message ID |       8 |

Repeated rows are 56.52% of all usage-bearing rows. More than 38% of response
groups contain an actual counter revision, rather than only a byte-identical
repeat. Downward corrections exist, so taking the maximum counter is also
incorrect.

The downstream review's earlier live snapshot reported approximately 334,379
rows and 145,032 groups. The corpus grew before this selected run; both runs
show the same semantic pattern.

### 4.3 Token-total effect

| Usage bucket         | Current per-row delta | Latest response snapshots | Current / snapshot |
| -------------------- | --------------------: | ------------------------: | -----------------: |
| Input                |            46,256,703 |                14,391,658 |             3.214x |
| Output               |           254,650,919 |               126,850,471 |             2.007x |
| Cache creation input |         4,911,592,055 |             1,743,579,851 |             2.817x |
| Cache read input     |        67,658,183,639 |            32,203,809,672 |             2.101x |

These are corpus semantics, not a billing assertion. They prove that the
current Claude `Delta` interpretation cannot be the usage-v2 oracle.

The code path explains the result: the Claude adapter keys `MessageFact` from
the transcript row's top-level UUID and emits each non-zero usage row as
`UsageAccounting::Delta`. The projector stores one additive contribution per
fact ID. It has no response-level revision key, so every repeated row is
summed.

`requestId` cannot repair this alone: it is absent on 268 usage rows in this
run and eight request IDs correspond to multiple `message.id` values.

### 4.4 Typed runtime evidence

| Evidence                                          |            Result |
| ------------------------------------------------- | ----------------: |
| Usage rows with model                             |           342,861 |
| Usage rows with native top-level effort           |            93,572 |
| Records with native mode                          |            14,683 |
| Records with native permission mode               |            25,831 |
| Tool calls / results                              | 185,204 / 185,327 |
| `TaskCreate` / `TaskUpdate` calls                 |     1,373 / 2,824 |
| `EnterPlanMode` / `ExitPlanMode` calls            |           55 / 68 |
| `Agent` calls                                     |             2,107 |
| `AskUserQuestion` calls with structured questions |               135 |
| Matched successful / error question results       |          114 / 20 |
| Question calls pending at end of their file       |                 1 |

Model is observable on the next response, which is not the same as an
instantaneous model-change event. Effort is present for only 27.29% of usage
rows. The observation contract must therefore expose both value and evidence
quality/timing, and report unsupported or unknown state honestly.

The `AskUserQuestion` result shows that a common interaction lifecycle is
feasible from existing tool-call/result content. It does not justify treating
all tool results as questions or merging questions with permission requests.

## 5. Accepted conclusions

1. A database-free, session-scoped common observer is a release-blocking
   requirement, not an optional wrapper around the durable host.
2. The observer and durable host must share source drivers, adapter decoders,
   provenance, actor identities, and semantic reducers while using different
   sinks.
3. Claude usage must migrate to response-level snapshot revisions keyed first
   by source instance/object/generation and `message.id`.
4. `requestId` is optional correlation metadata, never the sole identity.
5. Each new revision replaces the prior contribution for that response;
   downward corrections are valid.
6. Model, effort, mode, plan/task, actor, workflow/team, and interaction
   evidence require typed observations plus raw native retention and explicit
   evidence quality.
7. Team and workflow affiliation are orthogonal to root/child actor identity.
8. Workflow/team files require a bounded artifact read contract, not arbitrary
   reads mixed into the transcript stream.
9. `watchSessionTranscript` must remain supported until the replacement passes
   conformance and Chopsticks migrates.

## 6. Still unproven and release-blocking

The static census does not accept the production observer. The implementation
must still prove:

- attach before root creation and watcher-before-bootstrap ordering;
- a truthful bootstrap barrier across existing root and descendant objects;
- discovery of future standard, workflow, and team descendants;
- partial-record buffering and explicit reset-before-replay ordering;
- idempotent concurrent `poll()` calls;
- per-object errors without scope termination;
- bounded queues with a never-dropped overflow/resync control signal;
- cancellation without leaked watches or tasks;
- no SQLite creation and no unrelated global-root scan;
- root, child, workflow member, and team member attribution;
- usage revision parity with the independent response-group oracle;
- model/effort/mode, plan/task, and interaction delivery;
- unknown-record delivery with raw evidence; and
- hook-triggered `poll()` to event-offer p99 on the reference host.

These gates are specified in the amended RFC 012 implementation plan. Until
they pass, the new API is an architectural contract, not a production claim.

# RFC 012 parallel execution runbook

- **Status:** Ready to assign
- **Written:** 2026-08-19
- **Current product-code base:** `67a1ae9`
- **Assignment base:** the primary-integrator commit containing this runbook;
  use the exact SHA announced as `RUNBOOK READY — BASE <sha>`
- **Audience:** repository owner, primary integrator, and parallel implementation
  agents
- **Primary references:** [implementation plan](./012-implementation-plan.md),
  [RFC 012A](./012a-agent-adaptation-and-engine-boundaries.md),
  [RFC 012B](./012b-catalog-readiness-and-progressive-startup.md),
  [RFC 012C](./012c-runtime-semantics-and-usage-v2.md), and
  [RFC 012D](./012d-session-scoped-observation.md)

This runbook supersedes the 2026-08-16 single-agent handoff formerly stored at
this path. It defines the parallel order for the still-open A1-A3, B1-B3, and
C1-C3 work, the primary integrator's own work, review and merge checkpoints,
and an external-SSD worktree layout.

## 1. Current program position

The `In progress` labels describe exit gates, not workstreams starting from
zero:

- **A1-A3:** the base model, support verifier, typed authorization, and three
  Candidate packages exist. Remaining work includes promotion-minimum family
  parity, the strict access request/report lifecycle, artifact pins, complete
  candidate evidence, real compositions, and promotion review. No current
  adapter release is promoted.
- **B1-B3:** B1 is open only at its public exposure gate. B3 already includes
  initial publication, retained pages, identity resolution, refresh
  successors, logical retirement, and independently-safe refresh failure.
  B2 still lacks a real promoted adapter composition and production source
  access/coverage producer. B3 still lacks the remaining readiness variants
  and caller-authorized public policy transport.
- **C1-C3:** usage-v2, source-scoped selection, rollback, coverage, and durable
  migration are substantially implemented. C3's immediate remaining exit gate
  is collection and review of the representative external compatibility
  report. C1/C2 still have promotion-minimum portable and scoped/durable family
  parity work.

The critical dependency is:

```text
C3 evidence -------------------+
A1/C1 contract parity ---------+--> A3 candidate completion --> promotion
A2 authority lifecycle --------+              |
                                               +--> B2 real composition
B3 private durability ----------------------------> B1/B3 public exposure
C2 runtime parity --------------------------------> promotion and D runtime
```

Promotion and public exposure are intentionally not parallelized with their
prerequisites.

## 2. Concurrency model

There are four useful active slots. Use them as follows:

1. Agent C3: compatibility-window report.
2. Agent A1/C1: portable/N-API semantic parity.
3. Agent A2: strict access request/report boundary.
4. Primary integrator (`/root`): checkpoint review, independent validation,
   merge ownership, and a read-only Wave 2 Claude promotion-gate audit.

Do **not** spawn Agent B3 at time zero. Spawn it when Agent C3 has produced a
reviewed commit and that slot is free. This keeps the primary integrator active
and avoids five simultaneous owners.

Agents work in separate Git worktrees and branches. The primary repository at
`/Users/jamesyong/Projects/project100/p008/spaghetti` remains the integration
tree. Only the primary integrator may update
`docs/rfcs/012-implementation-plan.md`, stage in the integration tree, merge or
cherry-pick lane commits, or prepare a promotion commit.

## 3. External SSD layout

At the time this runbook was written:

- the internal data volume had about 16 GiB available;
- `/Volumes/SamsungRed` was mounted as APFS with about 931 GiB available; and
- the repository had previously accumulated a very large Cargo target tree.

The portable SSD should hold worktrees and build artifacts. The main `.git`
object database remains in the primary repository, but that database is small;
the checked-out files, lane-local `node_modules`, and configured Cargo target
tree live on the SSD.

### 3.1 One-time setup by the repository owner

Run these commands in a normal Terminal session while the SSD is mounted and
after the primary integrator announces `RUNBOOK READY — BASE <sha>`. An agent
sandbox may require explicit permission before it can write under `/Volumes`,
so owner-created worktrees are the least surprising setup. Each agent session
must then be launched with its SSD worktree as an authorized workspace root;
changing directory inside a session whose sandbox authorizes only the internal
repository may still leave the SSD read-only.

```bash
cd /Users/jamesyong/Projects/project100/p008/spaghetti

RFC012_SSD_ROOT=/Volumes/SamsungRed/spaghetti-rfc012
mkdir -p "$RFC012_SSD_ROOT/worktrees" \
  "$RFC012_SSD_ROOT/build/cargo-target" \
  "$RFC012_SSD_ROOT/build/pnpm-store"

git worktree add -b work/c3-compatibility-report \
  "$RFC012_SSD_ROOT/worktrees/c3" <RUNBOOK_BASE>
git worktree add -b work/a1-c1-napi-parity \
  "$RFC012_SSD_ROOT/worktrees/a1-c1" <RUNBOOK_BASE>
git worktree add -b work/a2-access-boundary \
  "$RFC012_SSD_ROOT/worktrees/a2" <RUNBOOK_BASE>
```

Launch each agent with its worktree as its working directory. Do not copy the
untracked root `draft/` directory to the SSD worktrees.

When Agent C3 is merged and its slot is free, the primary integrator will
announce a new base commit. Then create the B3 worktree from that base:

```bash
cd /Users/jamesyong/Projects/project100/p008/spaghetti
RFC012_SSD_ROOT=/Volumes/SamsungRed/spaghetti-rfc012

git worktree add -b work/b3-readiness-next \
  "$RFC012_SSD_ROOT/worktrees/b3" <INTEGRATOR_PROVIDED_BASE>
```

Wave 2 worktrees must likewise be created from the exact base announced after
Wave 1 integration, not from `67a1ae9`.

### 3.2 Build-artifact rules

Each agent should set the shared Cargo target location for every Rust command:

```bash
export CARGO_TARGET_DIR=/Volumes/SamsungRed/spaghetti-rfc012/build/cargo-target
```

Cargo may serialize conflicting build operations against this shared target;
that is acceptable and saves substantial disk space. Agents should perform
analysis and editing concurrently but avoid launching multiple full Rust
matrices at once. Focused tests belong to lane agents; the primary integrator
runs the combined full matrix.

If a worktree needs dependencies installed, keep its `node_modules` on the SSD
and use the SSD-backed pnpm store:

```bash
pnpm install --frozen-lockfile \
  --store-dir /Volumes/SamsungRed/spaghetti-rfc012/build/pnpm-store
```

Operational rules:

- Confirm `/Volumes/SamsungRed` is mounted before starting or resuming an
  agent.
- Do not unplug or unmount the SSD while an agent, Cargo, pnpm, or Git command
  is active.
- No lane agent may run `cargo clean`; the Cargo target is shared.
- No lane agent may run `git worktree prune`, remove another worktree, switch
  the integration branch, stash integration-tree files, or use destructive Git
  recovery commands.
- Monitor both volumes periodically:

```bash
df -h /System/Volumes/Data /Volumes/SamsungRed
du -sh /Volumes/SamsungRed/spaghetti-rfc012/build/cargo-target
```

- If the SSD disappears, stop. Do not prune the missing worktrees. Remount it
  first and confirm `git worktree list` is coherent.

## 4. Review protocol

The primary integrator reviews continuously. Agents do not disappear until a
large final diff is ready.

### Checkpoint 1: scope freeze, before edits

The agent reports:

- base commit, branch, and worktree path;
- deviations found in RFC, plan, code, fixtures, or schemas;
- the smallest honest objective;
- exact intended paths;
- invariants, negative tests, and validation commands;
- dependencies and any policy decision the task would otherwise invent.

The primary integrator checks RFC fit, dependency direction, ownership, file
overlap, false authority, and whether the slice can be made smaller. The agent
must wait for `GO <lane> <frozen paths>` before editing.

### Checkpoint 2: coherent focused checkpoint

After the tree compiles and focused tests pass, the agent reports the exact
paths, semantic behavior, tests/pass counts, and current `git status --short`.
Everything remains unstaged.

The primary integrator reviews authority construction, canonical identities,
replay and reorder invariance, bounded preflight, privacy, restart behavior,
wire parity, and failure precedence. The integrator may require additional
negative tests before allowing the full matrix.

### Checkpoint 3: final unstaged diff

The agent reports:

- exact diff paths and statistics;
- every claim proved and every gate deliberately left open;
- all focused/static validation results;
- known unrelated failures;
- an exact proposed commit message and plan-status paragraph.

The primary integrator performs a line-by-line semantic diff review and reruns
selected focused tests independently. The agent must not stage while this
review is active.

### Checkpoint 4: rebase/integration review

Before a later lane lands, it is rebased or replayed onto the exact base
provided by the integrator. The integrator rechecks fixture and package
digests, generated output, schema mirrors, and assumptions changed by earlier
lanes. Agents do not independently choose a new base.

### Checkpoint 5: staging and commit

Only after explicit `STAGE <lane>` approval does the agent stage its exact
owned paths. It reports `git diff --cached --name-status` and
`git diff --cached --check`. The integrator confirms the cache before the agent
commits with the approved message.

### Checkpoint 6: post-commit integration

The agent reports its commit hash and clean status. The primary integrator
merges/cherry-picks it, reruns the affected focused tests, and later runs the
combined matrix. Only then may the implementation-plan status be updated.

If agents cannot message `/root` directly, the repository owner should paste
each checkpoint into the primary thread. Ask for review immediately when a
checkpoint is ready; do not wait for the other lanes.

### 4.1 Active Wave 1 policy decisions

The following decisions were frozen by the primary integrator on 2026-08-19
after the A1/C1 and A2 scope audits. They are review constraints, not permission
to broaden either lane.

For Agent A1/C1:

- expose JSON-string parse helpers on the native addon; do not introduce
  `#[napi(object)]` mirrors for these semantic values;
- reject unknown fields in C1 `parseQualifiedValue` so its strictness matches
  the A1 coverage boundary and RFC 012D parsers;
- keep the already-committed value fixtures; retraction/replacement rows remain
  a later semantic slice; and
- import `@vibecook/spaghetti-sdk-native` directly in focused tests; do not add
  `native.ts` or package-index barrel exports in this lane.

For Agent A2:

- bind one opaque, host-supplied, nonzero 32-byte `access_policy_digest` by
  exact equality. RFC 012A assigns it no LOCAL/WITHHELD, catalog, or artifact
  interpretation in this slice;
- replay protection means a portable value never becomes authority and one
  retrieval request is bound to one exact report digest. No durable nonce
  table is introduced;
- add no `adapter/mod.rs` or other public export unless compilation makes it
  strictly necessary and the path expansion is reported before editing; and
- do not edit `scoped_observation.rs`. The root-owned D slice `71afae4` already
  replaced `ScopedKnownObjectGrant`'s path-leaking derived `Debug` with a
  redacted implementation.

## 5. Timeline and spawn schedule

Durations below are planning ranges, not release promises. Evidence access,
review findings, or newly discovered policy choices can extend them.

### T+0: setup and spawn now

Estimated owner setup: 20-40 minutes.

Create the three SSD worktrees and spawn exactly:

1. Agent C3 with the prompt in section 7.1.
2. Agent A1/C1 with the prompt in section 7.2.
3. Agent A2 with the prompt in section 7.3.

The primary integrator remains active in the integration tree.

Expected first checkpoint: the first working session, usually 30-90 minutes
per agent. When any scope-freeze message arrives, immediately ask the primary
integrator to review that lane. Approved agents continue without waiting for
the other scope reviews.

### Primary integrator work during Wave 1

The primary integrator will not start a conflicting implementation lane.
Instead it will:

1. review and approve/reject every scope freeze;
2. maintain a live path-ownership and merge queue;
3. perform a read-only Claude Wave 2 promotion-gate audit across A2, A3, B2,
   C2, and the current Candidate package;
4. identify the exact promotion-minimum family and evidence matrix;
5. independently test focused checkpoints;
6. merge reviewed commits in dependency order; and
7. own all plan-status updates and final combined validation.

This review/integration work is the fourth active slot.

### Wave 1a: first three lanes

Approximate lane budgets:

- C3 report: one-half to one agent-day if representative data is accessible;
  otherwise it should return a precise blocker quickly.
- A1/C1 parity: one to two agent-days.
- A2 authority boundary: one to two agent-days.

When Agent C3 reaches its final unstaged checkpoint, ask the primary integrator
to review it immediately. After review, staging approval, commit, merge, and a
focused integration check, the integrator will announce:

```text
C3 MERGED — B3 MAY START — BASE <sha>
```

Only then create the B3 worktree and spawn Agent B3 with section 7.4. Agent B3
will overlap the still-running A1/C1 and A2 lanes.

If C3 is blocked only because representative external data is unavailable, do
not leave its slot idle. The integrator may authorize B3 to start from a stated
base while C3 remains a named promotion blocker.

### Wave 1b: B3 and integration

Approximate B3 budget: one to two agent-days, but a read-only blocker report is
the correct output if the next transition requires policy that does not exist.

As A1/C1, A2, and B3 finish, ask the primary integrator to review each final
unstaged diff immediately. Do not wait for all three before beginning reviews.
The merge queue is normally:

1. C3 evidence and any candidate digest repin;
2. A1/C1 parity;
3. A2 lifecycle;
4. B3 private durability.

After the last Wave 1 merge, everyone waits while the primary integrator runs
the combined matrix and updates the plan. Do not spawn Wave 2 until the primary
integrator emits:

```text
WAVE 1 INTEGRATION GREEN — BASE <sha>
```

Estimated integration window: one-half to one agent-day, depending on schema,
SDK, and support-package changes.

### Wave 2: Claude promotion vertical

Create three fresh SSD worktrees from the exact Wave 1 base and spawn:

1. Agent C2: promotion-minimum runtime/scoped parity, section 8.1.
2. Agent A3: Claude artifact and evidence completion, section 8.2.
3. Agent B2: real Claude composition/access/coverage preparation, section 8.3.

The primary integrator again occupies the fourth slot, reviews checkpoints,
and maintains the exact promotion gate. Expected Wave 2 work is two to four
agent-days plus integration, depending on artifact access and evidence gaps.

Merge order is:

1. C2 parity;
2. A3 artifact/evidence;
3. B2 runtime composition and coverage;
4. full combined validation and evidence review;
5. a separate, primary-integrator-owned promotion decision; and
6. a separate runtime-selection change, if promotion succeeds.

No Wave 2 implementation agent may promote a release. Do not spawn Wave 3
until the primary integrator emits either:

```text
CLAUDE PROMOTION GATE PASSED — BASE <sha>
```

or a blocker report that explicitly replans the next wave.

### Wave 3 and later

After the Claude gate passes, the next parallel lanes are:

- Codex A3/B2 evidence and composition;
- Grok A3/B2 evidence and composition;
- Claude D1-D3 real promoted scoped-observer integration; and
- primary-integrator-owned B1/B3 caller-authorized public policy/exposure
  integration.

After those merge and validate:

1. B1/B3 public N-API/SDK catalog exposure;
2. D4 public observer migration;
3. B4 host/UX and C4 downstream migration;
4. B5/D5 performance calibration;
5. A4 fourth-adapter proof; and
6. X integration gates.

## 6. Universal agent rules

Every agent must:

1. Read this file and the relevant RFC/implementation-plan sections fully.
2. Start read-only and wait at Checkpoint 1.
3. Preserve Candidate/unsupported status unless the primary integrator owns an
   explicit later promotion operation.
4. Treat access, completeness, identity, compatibility, and safety as typed
   evidence, never caller booleans or repeated digest strings.
5. Preflight collection and encoded-byte bounds before retaining or sorting
   attacker-sized input.
6. Keep native paths, IDs, prompts, content, and secrets out of fixtures,
   Debug, logs, reports, and portable DTOs.
7. Use `rg` for searches and `apply_patch` for hand edits.
8. Never use `git add -A`, `git add .`, broad cleanup, destructive Git
   commands, or another lane's worktree.
9. Keep changes unstaged until the explicit staging checkpoint.
10. Run `git diff --check` before every handoff.
11. Report exact pass counts rather than saying only “tests pass.”
12. Leave `docs/rfcs/012-implementation-plan.md`, architecture ledger status,
    promotion status, and `draft/` to the primary integrator.

## 7. Immediate agent prompts

These prompts are intended to be copied verbatim when the worktrees are ready.

### 7.1 Agent C3 — compatibility closeout

```text
You own the RFC 012C C3 compatibility-window closeout.

Worktree: /Volumes/SamsungRed/spaghetti-rfc012/worktrees/c3
Branch: work/c3-compatibility-report
Base: <RUNBOOK_BASE announced by /root>

Read docs/rfcs/012-parallel-work-handoff.md completely before acting. Set
CARGO_TARGET_DIR=/Volumes/SamsungRed/spaghetti-rfc012/build/cargo-target for
Rust commands. Do not run cargo clean and do not touch draft/.

Begin read-only. Before editing, send /root:
1. the exact remaining C3 gate;
2. the representative input/window you can actually access;
3. exact intended paths;
4. privacy model and report fields;
5. validation commands; and
6. every candidate-package digest that would change.
Wait for GO before editing.

Task:
- Run the existing bounded compatibility sampler over a genuinely
  representative external window.
- Produce only deterministic aggregate/privacy-reduced evidence.
- Classify equal, legacy-higher, usage-v2-higher, and incomparable buckets
  without treating expected semantic differences as failures.
- Bind the report to exact artifact/declaration/release/contract and sampler
  evidence.
- Update candidate evidence bindings only when justified.
- Determine whether this closes C3 or exposes another gate.

Do not fabricate a report when data is unavailable. Do not commit paths,
native IDs, content, secrets, or raw timestamps. Do not promote a release or
edit docs/rfcs/012-implementation-plan.md. Separate report generation from
support-package digest repinning where practical. Remain unstaged until /root
grants STAGE.

Report scope freeze, focused report/privacy checks, final unstaged diff, exact
cached paths, report digest, pass counts, remaining gates, and git status at
the checkpoints defined by the runbook.
```

### 7.2 Agent A1/C1 — portable and N-API parity

```text
You own one bounded RFC 012A A1 / RFC 012C C1 contract-parity closure.

Worktree: /Volumes/SamsungRed/spaghetti-rfc012/worktrees/a1-c1
Branch: work/a1-c1-napi-parity
Base: <RUNBOOK_BASE announced by /root>

Read docs/rfcs/012-parallel-work-handoff.md and the A1/C1 plan sections fully.
Set CARGO_TARGET_DIR=/Volumes/SamsungRed/spaghetti-rfc012/build/cargo-target.
Do not run cargo clean or touch draft/.

Start read-only. Confirm whether the smallest promotion-critical gap is N-API
parity for the already-landed canonical coverage, actor-run,
actor-affiliation, and usage-v2 values. If a narrower prerequisite exists,
report it. Send /root the frozen objective, exact paths, fixture matrix,
negative tests, dependency conflicts, and validation before editing. Wait for
GO.

Intended task:
- Freeze Rust-produced semantic fixtures for existing types only.
- Consume equivalent values through N-API and portable TypeScript.
- Prove durable/scoped forms preserve the same SemanticRevisionRef and
  canonical identities.
- Reject unknown nested fields, invalid nulls, unsafe integers, zero
  generations, incompatible majors, noncanonical references, oversized
  values, and malformed qualified evidence.
- Prove no native locator/path or database handle crosses the boundary.
- Add the narrow architecture coverage required for the boundary.

Do not add a semantic family, change storage/query selection, touch support
classification, catalog files, RFC 012D delivery, or vendor packages. Shared
barrel/export files belong to the integrator unless a minimal compilation edit
is approved first. Do not claim A1/C1 complete beyond the executable slice.
Remain unstaged until STAGE approval.

Report exact identities/revisions compared, wire negatives, privacy and size
bounds, architecture results, focused Rust/SDK pass counts, remaining gates,
and git status at every runbook checkpoint.
```

### 7.3 Agent A2 — strict access lifecycle

```text
You own the next bounded RFC 012A A2 authority/lifecycle slice.

Worktree: /Volumes/SamsungRed/spaghetti-rfc012/worktrees/a2
Branch: work/a2-access-boundary
Base: <RUNBOOK_BASE announced by /root>

Read docs/rfcs/012-parallel-work-handoff.md and the A2 plan section fully. Set
CARGO_TARGET_DIR=/Volumes/SamsungRed/spaghetti-rfc012/build/cargo-target. Do
not run cargo clean or touch draft/.

First perform a read-only audit and freeze the smallest contract-only closure
for access-report retrieval and the trusted native-probe/grant request. Send
/root the exact paths, authority flow, invariants, negatives, portable surface,
and validation matrix. Wait for GO before editing.

Requirements:
- Issue request/report authority only from the existing verified private
  authorization.
- Bind exact adapter, release ID/digest, declaration digest, scope program,
  capability topology, operation, selection, and access-policy inputs.
- Deny Candidate or unsupported access before negotiation or native access.
- Keep authority-bearing Rust types private, non-Serde, nonconstructible, and
  redacted in Debug.
- Keep portable request/report values strict, bounded, path-free, and unable
  to grant authority by themselves.
- Reject wrong operation/program, capability restriction, stale or foreign
  digest, selection drift, and replay.
- Perform no native read and issue no public host permission.

Own common support/authorization code and support contract/tooling only. Do
not modify concrete vendor packages, catalog durability, or RFC 012C semantic
families. Shared exports require prior approval. Do not promote. Remain
unstaged until STAGE approval.

At each checkpoint report who can construct every authority type, what is
checked before native access, bounds/privacy, public gates left open, exact
paths, tests/pass counts, and git status.
```

### 7.4 Agent B3 — next private readiness transition

Use only after the integrator announces the new B3 base.

```text
You own the next policy-neutral RFC 012B B3 durable-readiness slice after the
integrator-provided base.

Worktree: /Volumes/SamsungRed/spaghetti-rfc012/worktrees/b3
Branch: work/b3-readiness-next
Base: <INTEGRATOR_PROVIDED_BASE>

Read docs/rfcs/012-parallel-work-handoff.md and current B1/B3 plan status
fully. Set CARGO_TARGET_DIR=/Volumes/SamsungRed/spaghetti-rfc012/build/cargo-target.
Do not run cargo clean or touch draft/.

Start read-only. Compare independently-safe discarded refresh, retry lineage,
Degraded/Partial, no-snapshot Error, and public policy transport. Recommend
only the smallest state whose authority already exists. A discarded active
refresh that preserves an authenticated predecessor is a hypothesis, not an
authorization: prove or reject it. Send /root the frozen design, exact paths,
state transition, evidence authority, corruption/crash matrix, and exclusions.
Wait for GO.

Any implementation must bind exact restart-authenticated plan, selection,
snapshot, publication/content digests, epoch, attempt, refresh-start commit,
and state-commit CAS. It must not accept caller-selected safety. Persist one
atomic source-neutral zero-fact transition with append-only evidence and a
privacy-safe outbox; reconstruct independently of prunable notification rows;
preserve pages, resolution, retirement, and SnapshotExpired behavior; and
prove crash rollback, lost-ack replay, stale/foreign rejection, and
Rust/TypeScript schema parity.

If the transition requires a new policy or producer, stop with a frozen
blocker rather than inventing authority. No public catalog method, LOCAL view,
physical compaction, richer query, source read, promotion, or plan-status edit
is allowed. Remain unstaged until STAGE approval.
```

## 8. Wave 2 prompts

Replace `<WAVE_1_BASE>` only with the commit announced by the primary
integrator. Create fresh worktrees on the SSD.

### 8.1 Agent C2 — promotion-minimum runtime parity

```text
Base your work on <WAVE_1_BASE> in a fresh SSD worktree. Read the parallel
runbook and C1-C3 plan status. Identify and close the smallest remaining RFC
012C C2 runtime-family or observer-envelope mapping required for the Claude
promotion surface. Prove durable/scoped entity identity, semantic revision,
correction, complete/partial replacement, retraction, actor, affiliation, and
coverage parity. Do not change query selection or promote support. Begin
read-only, send /root exact paths/invariants/tests, and wait for GO. Remain
unstaged through final semantic review and follow every runbook checkpoint.
```

### 8.2 Agent A3 — Claude artifact and evidence

```text
Base your work on <WAVE_1_BASE> in a fresh SSD worktree. Read the parallel
runbook and A3 plan status. Finish the candidate-only Claude evidence package:
exact artifact pin, deterministic sanitized transitions, complete claimed RFC
012D relation coverage, required RFC 012C semantic fixtures,
identity/compositionality/cross-topology checks, bounded performance evidence,
and human sanitizer-review inputs. Keep the release Candidate and every
unsupported capability unsupported. Do not implement runtime composition or
promote. Begin read-only and report every document and compiled-binding digest
that would change before editing. Follow all review/staging checkpoints.
```

### 8.3 Agent B2 — real Claude composition preparation

```text
Base your work on <WAVE_1_BASE> in a fresh SSD worktree and consume only the
reviewed Claude declarations supplied by the integrator. Implement the actual
common/runtime and Claude catalog composition plus source-access/coverage
producer behind Candidate denial. It may execute in conformance with synthetic
authorization but must remain impossible to authorize for the built-in
Candidate. Bind complete membership authority, exact access
policy/declaration/selection, component completion, source coverage, and final
identity parity. Add no persistence, public API, policy expansion, or
promotion. Start read-only, freeze exact paths with /root, remain unstaged, and
follow every runbook checkpoint.
```

## 9. Merge and validation ownership

Lane agents run only their focused matrix plus inexpensive static checks. The
primary integrator runs the relevant combined checks after each merge and the
full matrix at wave boundaries.

The full integration matrix is selected according to touched surfaces and
normally includes:

- focused Rust tests for every changed module;
- `cargo test -p spaghetti-napi --lib`;
- `cargo clippy -p spaghetti-napi --lib --tests -- -D warnings`;
- `cargo fmt --all -- --check`;
- portable SDK tests, workspace typecheck, SDK build/package, and Prettier;
- support-package validation and support contract tests;
- RFC 011 architecture and RFC 012 delta ratchets;
- fresh-schema, restart, crash, corruption, and Rust/TypeScript DDL parity for
  schema changes;
- small and medium ingest-diff for schema or durable-query changes;
- fixture privacy and deterministic digest checks; and
- `git diff --check` plus exact staged-path review.

Live native census drift is never silently ignored. If it is unrelated to the
lane, the agent and integrator record the exact deviation separately; if the
lane claims representative native evidence, that drift must be resolved or the
claim remains blocked.

## 10. Stop conditions

An agent stops and reports rather than improvising when it encounters:

- missing representative evidence or inaccessible native input;
- a required policy choice about identity, admission, disclosure, retry,
  degradation, retention, or promotion;
- a need to edit another lane's owned path;
- an authorization that would be constructed from caller booleans or repeated
  digests rather than verified evidence;
- a need to expose a public API before its caller/transport authority exists;
- a schema change outside an approved schema lane;
- privacy-sensitive output that cannot be reduced safely; or
- a failing shared baseline unrelated to the lane that prevents honest final
  validation.

The primary integrator resolves the dependency, revises the frozen scope, or
records the task as blocked. Agents do not broaden scope on their own.

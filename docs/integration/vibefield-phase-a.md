# VibeField Phase A — the Spaghetti surface

What [`docs/petition/vibefield-needs.md`](../petition/vibefield-needs.md) §7
asks Spaghetti for in Phase A, and where each item is in `@vibecook/spaghetti-sdk`
0.8.0.

- **Landing surface:** [RFC 012 landing plan](../rfcs/012-landing-plan.md) §3.2
- **Contracts:** [012A](../rfcs/012a-agent-adaptation-and-engine-boundaries.md)
  (identity), [012B](../rfcs/012b-catalog-readiness-and-progressive-startup.md)
  (catalog, pagination), [012D](../rfcs/012d-session-scoped-observation.md)
  (observer epochs)
- **Implementation:** `packages/sdk/src/vibefield.ts`, types generated into
  `packages/sdk/src/generated/`
- **Tests:** `packages/sdk/src/__tests__/vibefield.test.ts`

Everything below is generated from Rust. There is no hand-written TypeScript
mirror to drift.

## 1. Stable session and project references

```ts
import {
  isSameEntity,
  type ExternalEntityRef,
  type ProjectRef,
  type SessionRef,
} from '@vibecook/spaghetti-sdk';
```

`SessionRef` and `ProjectRef` are both aliases of RFC 012A's
`ExternalEntityRef`. The aliases name the role; a session reference is not a
different *kind* of reference.

```ts
type ExternalEntityRef = {
  external_entity_reference_version: number; // contract major, currently 1
  entity_key: string;                        // "v1:<base64url 32-byte digest>"
};
```

The digest is derived from the adapter id, the canonical source-instance key,
the entity kind, and the adapter-declared native key. It is machine-independent,
survives restart, does not depend on pagination order or an in-memory handle,
and carries no path, prompt, or database identifier.

**Compare by value, never by `===`.** Two decodings of the same reference are
different objects, which is why the helpers exist:

```ts
isSameEntity(left, right); // version AND entity_key must match
```

Comparing the version too is deliberate: references minted under different
contract majors are not comparable, and treating them as equal is exactly the
identity conflict RFC 012A requires to stay explicit.

> **Encoding trap.** Catalog and history *rows* carry `externalRef` as a string
> with a `"1:"` prefix; the `entity_key` inside an `ExternalEntityRef` object
> uses `"v1:"`. Same 32-byte digest, two textual spellings. Pass `row.externalRef`
> straight back to `resolveCatalogEntity()` — that is what it accepts — but do
> **not** string-compare it against `entity_key`, and do not build an
> `ExternalEntityRef` by hand from it.

## 2. Native session id when provable, conflicts kept explicit

```ts
const page = await api.listCatalogSessions({ projectId, limit: 500 });
for (const session of page.sessions) {
  session.externalRef;         // persist this
  session.nativeSessionId;     // present only when the adapter can prove one
  session.identityConflicts;   // competing claims that lost, with their evidence
}
```

`identityConflicts[]` entries carry `competingNativeProjectKey`, `basis`, and
`provenance`. A session claimed by two projects appears under the winner and
keeps the loser's evidence; nothing is merged away silently.

Native identities are namespaced — the same `native_id` from two agent products
is two identities:

```ts
import { isSameNativeIdentity, type NativeIdentity } from '@vibecook/spaghetti-sdk';
isSameNativeIdentity(
  { native_namespace: 'claude-code', native_id: 'abc' },
  { native_namespace: 'codex', native_id: 'abc' },
); // false
```

## 3. Project-association evidence with provenance

Every catalog session row says *why* it is filed under its project:

| Field | Meaning |
| --- | --- |
| `associationBasis` | which native evidence established it (index, transcript cwd, session directory, rollout header, declared ancestor) |
| `associationQuality` | `exact`, `native_claimed`, `derived`, or `estimated` |
| `associationProvenance` | the specific source evidence behind the claim |
| `catalogState` | `discovered` → `transcript_backed` → `hydrated` → `searchable` |
| `degraded` / `degradedReason` | the source could not be read completely, and why |

A row with no accepted association reports that rather than inventing an
unscoped project.

## 4. Durable query watermark and snapshot-consistent pagination

Every durable query result carries `atCommitSeq`:

```ts
import { queryWatermark, isSameSnapshot } from '@vibecook/spaghetti-sdk';

const projects = await engine.listHistoryProjects();
const sources = await engine.listSources();

queryWatermark(projects);              // number
isSameSnapshot(projects, sources);     // safe to join without re-reading?
```

Two results with equal watermarks describe the same durable snapshot and may be
joined. `DurableQueryWatermark` is derived structurally from the napi-generated
result types, so a rename in Rust breaks compilation here rather than at runtime
in VibeField.

Catalog pages are cursor-paginated and bound to the snapshot the first page was
answered at — a background commit between pages cannot duplicate, drop, or
reorder a row:

```ts
let cursor: string | undefined;
do {
  const page = await api.listCatalogSessions({ projectId, cursor, limit: 500 });
  ingest(page.sessions);
  cursor = page.cursor;
} while (cursor);
```

`atCommitSeq` orders durable commits **only**. Never compare it to an observer
`sequence` or to a native cursor.

## 5. `SemanticRevisionRef` — the durable/live join identity

```ts
type SemanticRevisionRef = {
  semantic_reference_contract_version: number;
  fact_revision_id: string; // "v1:<base64url 32-byte digest>"
};

import { isSameRevision } from '@vibecook/spaghetti-sdk';
```

The same revision returns the same reference from a durable query and from the
scoped observer (`SemanticEvent.semantic_revision_ref`). That equality is what
makes cross-topology reconciliation possible without comparing observer event
ids — which are epoch-scoped delivery keys and are not a join identity.

Deduplicate durable and live items on `SemanticRevisionRef`. Retire unmatched
overlay state only when coverage actually proves the durable side subsumes it;
partial or incomparable coverage proves nothing about absence.

**Not shipped:** no code in this repository performs that deduplication. The
references and the watermark are the contract; the joiner is yours.

## 6. Readiness — knowing what you are reading

```ts
const readiness = await api.getReadiness();
readiness.catalog.state;  // 'pending' | 'indexing' | 'ready' | 'degraded' | 'unavailable'
readiness.history.state;
readiness.atCommitSeq;
```

Six independent fields — `catalog`, `history`, `usage`, `capabilities`,
`artifacts`, `search` — each with the commit its evidence was read at and a
human `detail`. `catalog` is routinely `ready` while `history` is `indexing`.

An aggregator should read this before concluding anything from an empty result:
an entity missing while `history` is `indexing` has not been indexed yet, which
is a different fact from it not existing.

## 7. Observer epochs and full-replacement resync

```ts
import { observeSession, isSemanticEvent } from '@vibecook/spaghetti-sdk';
```

Every observer event carries a `scope_epoch`. Deduplicate on
`(scope_epoch, event_id)`. On continuity loss the observer emits `overflow`,
stops ordinary delivery, and rebuilds a complete snapshot in a new epoch ending
at `resync_complete`, whose `family_manifest` gives per-family entity counts and
a topology-neutral digest. Stage the new epoch, swap atomically at the barrier,
and remove entities absent from the replacement.

Full detail and the porting table:
[chopsticks-observe-session.md](./chopsticks-observe-session.md).

## Phase A checklist

| Petition §7 Phase A requirement | Shipped? | Surface |
| --- | --- | --- |
| Stable project/session refs | yes | `ExternalEntityRef` / `SessionRef` / `ProjectRef`; `externalRef` on every catalog and history row |
| Native session ID when available | yes | `CatalogSessionRow.nativeSessionId`, `NativeIdentity`, `isSameNativeIdentity` |
| Project association evidence | yes | `associationBasis` / `associationQuality` / `associationProvenance`, `identityConflicts[]` |
| Durable query watermark | yes | `atCommitSeq`, `queryWatermark`, `isSameSnapshot` |
| Snapshot-consistent pagination | yes | catalog cursors bound to the page-one watermark |
| Stable semantic event/revision IDs | yes | `SemanticRevisionRef`, `isSameRevision`; equal across durable and observer |
| Scoped observer epoch + full-replacement resync | yes | `scope_epoch`, `overflow`, `resync_complete`, `family_manifest` |

## Known limits

- **Two families rest on narrower evidence than their name.** All eleven are
  emitted with typed values, but `plan` revisions come from
  `ExitPlanMode`/`EnterPlanMode` tool evidence rather than from
  `plans/<slug>.md` sidecars (which stay snapshot facts with no actor binding),
  and an orphaned `tool_result` keeps content-block evidence without a guessed
  tool name. Both are recorded in
  [RFC 012C](../rfcs/012c-runtime-semantics-and-usage-v2.md) §7.
- **The per-family value types are not nameable from the package entry point.**
  `SemanticEvent.value` is a typed `RuntimeSemanticValue` and narrowing on
  `family` narrows it, but the union and its `*Fact` members are not exported
  and there is no `./generated` subpath. Derive what you need:
  `type V = NonNullable<SemanticEvent['value']>`.
- **No identity relations.** Alias, `SameEntity`, `Supersedes`, and `ReplacedBy`
  facts do not exist. A project moved without provable native identity becomes a
  new project. Conflicts are reported, never resolved.
- **Reference resolution is three-valued.** `resolveCatalogEntity` returns a
  live project, a live session, `retracted`, or `unknown` — there is no
  `superseded` arm to follow.
- **No policy gating.** Native ids and locators are returned to any local
  caller; the petition's authorized-view distinction is not implemented.
- **First run after 0.8.0 rebuilds the index.** The catalog is back in about a
  second; history and search take hours on a large corpus. Read
  `getReadiness()` rather than assuming an empty query means empty data.

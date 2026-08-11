# RFC 010: Adopt `node:sqlite` and Remove the Last Install Script

**Status:** Draft v1
**Created:** 2026-08-11
**Author:** James Yong + Claude
**Motivated by:** the npm 12 install breakage found while proving [RFC 008](./008-rust-ingest-production-readiness.md) §9 rollback
**Independent of:** [RFC 009 — Retire the TypeScript Bulk Ingest Engine](./009-retire-typescript-bulk-ingest.md), though it shrinks 009's surface

---

## Summary

Replace `better-sqlite3` with Node's built-in `node:sqlite`, deleting the only
dependency in the tree that needs an install script to work.

The whole production migration is **two files** — `io/sqlite-service.ts` and
`io/sqlite-health.ts` — because the facade already exists and nothing outside it
touches the raw handle. The one genuine hazard is that
`data/ingest-service.ts` depends on `better-sqlite3` nesting transactions as
savepoints; a naive `BEGIN`/`COMMIT` port throws on the live-ingest path. That is
solved and prototyped below.

The cost is the Node floor: `>=18` today, and `node:sqlite` needs 22.5+.

---

## Motivation

npm 12 blocks package install scripts by default. `better-sqlite3` fetches its
prebuilt native binding from a `postinstall`, so under a stock npm 12:

```
npm install -g @vibecook/spaghetti     # succeeds
spag projects                          # Could not locate the bindings file
```

This shipped in `0.6.0`, `0.6.1`, and `0.6.2`, and is the sole reason RFC 008 §9
is unsigned. Two attempts to paper over it have already failed in instructive
ways:

- `0.6.1` reported the failure as *"Install a supported agent and re-run"* —
  the wrong diagnosis entirely, sending users to look for something that was
  already there.
- `0.6.2` diagnosed it correctly but printed **two remedies that do not work**
  (#117). Documentation is not a fix when the documentation is also a moving
  target: the npm surface here has three different answers depending on whether
  the install is global, project-scoped, or already on disk.

The recurring lesson of RFC 008 applies to packaging too. A failure mode that
depends on the *user's* package-manager version cannot be caught by this
repo's CI, because `pnpm-workspace.yaml` allowlists `better-sqlite3` — the
workspace is structurally incapable of seeing the breakage. **The only durable
fix is to have nothing to allowlist.**

---

## Why `node:sqlite` and not the standard packaging fix

There are three established ways to ship native code on npm. All three are
present in this repo's own dependency tree:

| Pattern                                       | In this tree                                                 | Survives npm 12 |
| --------------------------------------------- | ------------------------------------------------------------ | --------------- |
| Per-platform `optionalDependencies`           | `rollup` (21 pkgs), `@napi-rs/*` (11–15), `@vibecook/mille` (8) | ✅ nothing runs |
| All binaries bundled in one tarball           | `@vibecook/spaghetti-sdk-native` (8 targets, `files: ["*.node"]`) | ✅ nothing runs |
| `postinstall` + `prebuild-install`            | **`better-sqlite3`**, `@parcel/watcher`, `electron`          | ❌ the bug      |

Per-platform `optionalDependencies` is the industry standard — esbuild, rollup,
swc, Turbo and essentially every napi-rs project use it, and npm selects the
right package from `os`/`cpu` without executing anything. (esbuild also keeps a
`postinstall`, but only as a validator: when npm 12 blocks it, esbuild still
works because the binary arrived as a dependency.)

**Neither packaging pattern is the right answer here, because the dependency
itself is removable.** Node ships SQLite. Adopting `node:sqlite` is strictly
better than making `better-sqlite3` deliverable:

- No platform matrix to maintain, publish, or keep version-locked.
- No tarball-size decision.
- Nothing to re-verify the next time a package manager changes its script policy
  — and npm 12 will not be the last such change.

The native Rust addon already proves the bundled variant works; it has never
been affected by any of this. Pairing `node:sqlite` with that addon leaves the
project with **zero native install machinery of any kind**.

---

## Eligibility — verified, not assumed

Every claim below was executed on Node 26.5.0 against the real API.

**The feature that would have blocked this works.** Search is the reason
`text_content` exists at all, and it rides on FTS5:

```
create virtual table f using fts5(body)  →  ✅ matches returned
```

**The used API surface is covered.** Call counts across `packages/sdk/src`:

| better-sqlite3 API      | Uses | `node:sqlite`                                       |
| ----------------------- | ---: | ---------------------------------------------------- |
| `.run()`                |  159 | ✅ `StatementSync.run` — same `{changes, lastInsertRowid}` |
| `.get()`                |   80 | ✅                                                    |
| `.exec()`               |   73 | ✅ `DatabaseSync.exec`                                |
| `.close()`              |   38 | ✅                                                    |
| `.prepare()`            |   28 | ✅                                                    |
| `.all()`                |   12 | ✅                                                    |
| `.transaction(fn)`      |    7 | ⚠️ no equivalent — see the savepoint section          |
| `.pragma(str)`          |    3 | ⚠️ no method — `exec('PRAGMA …')` / `prepare('PRAGMA …').get()` |
| `.iterate()`            |    2 | ✅                                                    |

Also confirmed present: `db.function()` for user-defined functions, `backup`,
and the `readOnly` / `timeout` open options — note the spelling, which is a trap
in its own right (below). No `ExperimentalWarning` is emitted on 26.5.

**`db.inTransaction` does not exist** on `node:sqlite`. It is not used in this
codebase — the `beginTransaction()` hits in `lifecycle-owner.ts` are an
unrelated `IngestService` method — but the facade needs its own depth counter
(below), so this matters to the implementation rather than to callers.

### The open-options trap: `readonly` is not `readOnly`

`DatabaseSync` does not validate its options bag — it accepts
`{ totallyBogusOption: 1 }` without complaint. Combined with a one-character
spelling difference from `better-sqlite3`, that produces the most dangerous
finding in this RFC.

**`better-sqlite3` spells it `readonly`. `node:sqlite` spells it `readOnly`.**
The lowercase form is therefore an unknown option, silently ignored, and the
database opens **read-write**. Measured, not inferred:

| Option passed          | Missing file        | Write to an existing DB              |
| ---------------------- | ------------------- | ------------------------------------ |
| `{ readonly: true }`   | opens, no error     | **write SUCCEEDS — not read-only**   |
| `{ readOnly: true }`   | **throws**          | blocked ✅ `attempt to write a readonly database` |

`fileMustExist` does not exist in `node:sqlite` at all (also silently ignored),
but note the second column: **`readOnly: true` already throws on a missing
file**, so it covers what `fileMustExist` was there for.

This lands squarely on `sqlite-health.ts:103`:

```ts
db = new Database(dbPath, { readonly: true, fileMustExist: true });
```

That is the health checker, whose entire purpose is to notice a missing or
corrupt database. Copy that line across unchanged and it keeps compiling, keeps
running, and silently opens the user's database **writable** — in the one
component whose job is to inspect a suspect file without touching it.

**Phase 1 must:**

- rename `readonly` → `readOnly` at both call sites, and grep for any other
  option name assumed to carry over;
- add an explicit `existsSync` check where `fileMustExist` was load-bearing on a
  read-write open (`sqlite-service.ts`), since `readOnly` only covers the
  read path;
- pin both with tests — "missing file is an error, not an empty database" and
  "a read-only handle rejects writes."

This is the same shape as every other defect in this program: the option is
still there, still looks right, and no longer does anything.

### Two smaller differences

- **`foreign_keys` is already on.** `enableForeignKeyConstraints` defaults to
  `true`, and a fresh `DatabaseSync` reports `PRAGMA foreign_keys = 1`. The
  explicit `pragma('foreign_keys = ON')` in `sqlite-service.ts:101` becomes
  redundant — keep it as documentation or drop it, but do not assume it is what
  turns the constraint on.
- **`verbose` has no equivalent** and is silently ignored. It is plumbed through
  `SqliteConfig` but never set by any caller; drop it from the config type
  rather than leaving a dead option that looks live.

---

## Scope

**Two production files.**

| File                                | Lines | Work                                                                   |
| ----------------------------------- | ----: | ----------------------------------------------------------------------- |
| `packages/sdk/src/io/sqlite-service.ts` |   212 | swap the driver; implement `transaction()` and `pragma`; guard `fileMustExist` |
| `packages/sdk/src/io/sqlite-health.ts`  |   138 | swap the driver; `pragma('quick_check')` → `prepare`; guard missing-file |

Plus: 2 test files, 3 dev scripts (`ingest-diff`, `bench-ingest`,
`recover-legacy-data`), 7 `Database.Database` type annotations across 3 files,
and dependency/bundler removals.

**The facade holds.** `getDb()` — which returns the raw `better-sqlite3` handle
— has **zero callers** outside `sqlite-service.ts` itself. All six
`.transaction()` sites in `token-activity.ts`, `segment-store.ts`, and
`ingest-service.ts` call the `SqliteService` method, not the driver. This is why
the migration is two files rather than forty, and it is worth stating plainly:
the seam that makes this cheap was already built.

### Non-goals

- Changing the schema, query text, or any SQL.
- Touching the Rust engine. `rusqlite` is compiled in and unaffected.
- Removing `@parcel/watcher` or `electron`, which also carry install scripts.
  Neither breaks the CLI: the watcher degrades to polling, and Electron is not
  part of the published CLI path.

---

## The savepoint hazard

This is the part that breaks if ported naively, and the reason this RFC exists
rather than a one-line dependency swap.

`ingest-service.ts:1030–1042` opens a transaction by hand, then calls
`rebuildDirtyTokenActivity()`, which itself calls `SqliteService.transaction()`.
Its comment states the assumption directly:

> `better-sqlite3` nests transaction helpers as a savepoint, so the canonical
> rows and their affected aggregate buckets become visible atomically. An
> aggregation failure rolls the whole live batch back.

Under `better-sqlite3`, a nested `.transaction()` issues `SAVEPOINT` instead of
`BEGIN`. Implementing `transaction()` as a plain `BEGIN`/`COMMIT` pair therefore
throws **`cannot start a transaction within a transaction`** on the live-ingest
path — a path the fixtures exercise, so it would fail loudly rather than
silently, but only after the driver swap looked complete.

**The facade must track depth and issue savepoints.** Prototyped end to end on
`node:sqlite`:

```js
let depth = 0;                       // node:sqlite has no db.inTransaction
function transaction(fn) {
  const top = depth === 0;
  const name = 'sp' + depth;
  db.exec(top ? 'BEGIN' : `SAVEPOINT ${name}`);
  depth++;
  try   { const r = fn(); depth--; db.exec(top ? 'COMMIT'   : `RELEASE ${name}`);       return r; }
  catch (e) { depth--;            db.exec(top ? 'ROLLBACK' : `ROLLBACK TO ${name}`);   throw e; }
}
```

Verified against the three cases that matter:

| Case                                   | Expected | Result |
| -------------------------------------- | -------- | ------ |
| nested commit                          | 2 rows   | ✅     |
| inner throws — only inner rolls back   | 3 rows   | ✅     |
| outer throws — all of outer rolls back | 3 rows   | ✅     |

One subtlety to preserve: `ingest-service` manages its outermost transaction
with raw `exec('BEGIN')` / `exec('COMMIT')` and its own `inTransaction` flag,
*outside* the facade's counter. The facade's depth must therefore be seeded
from, or made aware of, an externally-opened transaction — otherwise the first
nested call issues `BEGIN` while one is already open. **Phase 1 must decide
whether `ingest-service` adopts the facade for its outer transaction, or the
facade exposes an explicit depth hook.** Adopting the facade is cleaner and is
the recommended path.

---

## The Node floor

`package.json`, `packages/sdk`, and `packages/cli` all declare `"node": ">=18"`.
Per the Node documentation, `node:sqlite`'s history is:

| Node version         | Status                                             |
| -------------------- | -------------------------------------------------- |
| v22.5.0              | module added, behind `--experimental-sqlite`       |
| v22.13.0 / v23.4.0   | flag removed; still marked experimental            |
| v25.7.0              | **Stability 1.2 — Release Candidate** (current)    |

**It is a release candidate, not Stable (2).** That is the honest status and it
should inform the decision rather than be smoothed over — though in practice
the API this project needs is the oldest and most exercised part of it, and
`0.6.x` already requires a native module that fails outright on the current npm.

Recommended floor: **`>=22.13.0`**, the first release where no flag is needed.
Node 18 and 20 are both out of support, so this excludes only unsupported
runtimes. Setting `>=24` instead would buy a longer-baked module at the cost of
a narrower audience.

The alternative — keeping `better-sqlite3` as a fallback behind the facade —
defeats the purpose, because the install script comes back with it.

This is a product decision, not a technical one. **Phase 0 is exactly this
choice and nothing else.**

---

## Phases

### Phase 0 — Decide and pin

Choose the floor — `>=22.13.0` recommended, `>=24` if the release-candidate
status argues for a longer-baked module — and set it in all three
`package.json` files. **Exit:** the floor is agreed and written down. No code
moves before this.

### Phase 1 — Port the facade

Swap the driver in `sqlite-service.ts` and `sqlite-health.ts`. Implement
`transaction()` with the savepoint scheme above, and resolve the
`ingest-service` outer-transaction question. Convert the three `pragma()` calls.
Replace `Database.Database` annotations with the `node:sqlite` types.

Replace the `fileMustExist` / `readonly` open guards with an explicit
`existsSync` check, since `node:sqlite` ignores both.

**Exit:** `pnpm test:packages` green; the four fixture diffs at zero; new unit
tests covering nested-commit, inner-rollback, outer-rollback, and
"missing file is an error, not an empty database."

### Phase 2 — Drop the dependency

Remove `better-sqlite3` from `packages/sdk`, `apps/playground`, and root
`devDependencies`; drop it from `pnpm-workspace.yaml`'s `onlyBuiltDependencies`,
from `packages/cli/tsup.config.ts` `external`, and from
`apps/playground/electron.vite.config.ts`. Port the three dev scripts.

**Exit:** `grep -r better-sqlite3` returns only historical prose. `pnpm install`
reports **no** blocked install scripts for it.

### Phase 3 — Prove it where it broke

Publish, then verify on a machine with npm 12:

```bash
npm install -g @vibecook/spaghetti@<version>   # no flags, no allowlist
spag projects                                  # must work
```

**Exit:** a stock install runs. This is RFC 008 §9's outstanding gate, and it
closes here. Re-run the real-corpus audit and rollback check against the
published artifact, then sign §9.

---

## Risks

| Risk | Mitigation |
| --- | --- |
| Nested-transaction semantics differ | Prototyped above; Phase 1 exit requires the three-case test. The live-ingest path is fixture-covered. |
| **Silently-ignored open options** | `readonly` → `readOnly` is a one-character rename that, missed, opens the DB writable in the health checker. `fileMustExist` does not exist. Both pinned by tests in Phase 1. Assume any option is ignored until proven otherwise — the bag is not validated. |
| `node:sqlite` API drifts while young | It is a Node builtin under semver; the facade is the single point of contact if it does. |
| Node floor excludes a consumer | Phase 0 decides this before any code moves. |
| A behavioural difference the fixtures miss | The real-corpus audit is the backstop — it is what found the data loss RFC 008 shipped with. Run it in Phase 1, not only Phase 3. |
| Performance regression | `better-sqlite3` and `node:sqlite` both bind SQLite synchronously; no async boundary is introduced. Confirm with `pnpm bench:ingest --mode warm --scenario unchanged`, whose 60 ms baseline is recorded in RFC 008 §3. |

---

## What this buys

- A stock `npm install` works, on any package manager, without documentation.
- The `crates/spaghetti-napi/npm/*` platform packages stay vestigial rather than
  becoming a maintenance surface.
- One fewer dependency in the plane RFC 009 is about to simplify.
- The class of bug closes rather than being documented: after this there is no
  install script left to block.

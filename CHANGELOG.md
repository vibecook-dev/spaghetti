# Changelog

<!-- Standing notice — maintained by hand, not by release-please. -->

## Upgrading to 0.8.0 — RFC 012 landing

Release-please generates the commit list below. This block is the part a
release note cannot derive from commit subjects: what breaks, what the first
run feels like, and what to do about it.

### ⚠ BREAKING — token totals are corrected downward, about 2×

Usage is now counted **per agent response**. One response contributes once,
however many times the transcript revised its counters while the reply was
streaming. The previous accounting added every one of those rows as if it were
new consumption.

On a large Claude corpus the same data now reports **36.88B tokens where 0.7.x
reported 78.52B — 2.129× lower** — from 362,043 native usage rows that resolve
to 158,118 actual responses. Nothing was lost and nothing is being hidden: the
old totals were wrong, and any dashboard, budget, or report built on them will
shift by roughly this factor.

Each of the four buckets (input, output, cache creation, cache read) is now
qualified independently. A bucket the source never asserted is *unknown* and is
never summed as zero, so a total is labelled `exact`, `estimated`, or `mixed`
instead of implying a precision it does not have.

Codex totals also change: its legacy path double-counted cache-read inside
input and reasoning inside output.

Grok totals change too, and become **exact**. Usage is now read per response
from `turn_completed` records in `updates.jsonl` instead of being estimated
from a session-scoped aggregate; the estimate path is deleted. A turn that does
not report `cacheCreationTokens` leaves that bucket unknown rather than
inferring it, and context occupancy is explicitly not treated as usage.

### ⚠ BREAKING — the first run rebuilds the whole index

`SCHEMA_VERSION` is 64. On first start after upgrading, the entire corpus is
re-read.

- The **catalog** — every project and session — is committed first and is
  listable in roughly 100 ms. `spag projects`, `spag sessions`, and the library
  screen work immediately.
- **History, usage, artifacts, and search** converge in the background, in
  **minutes rather than hours**: a 3.2 GB Claude corpus reaches complete history
  in about 12 minutes (725 s, 2.3 M runtime facts across all eleven families)
  and queryable search shortly after. Durable ingest went from 70 to 11,653
  records/s on a frozen 301 MB corpus — **166×** — this release; the
  measurements, both root causes, and the trade-offs accepted are in
  [the performance report](docs/rfcs/012-landing-perf-report.md).
- **Search is unavailable, not partial, during the rebuild.** Queries return a
  typed `projection_pending` error and the playground shows
  `Building the search index…`, rather than answering from an incomplete index.
- Nothing is lost. The database is a pure function of your agent files, and
  `spag doctor` shows exactly which fields are still `indexing`.
- The rebuild runs with `synchronous=OFF` for speed (about 30% faster; normal
  durability returns the moment the build completes). The accepted trade: a
  power loss or kernel panic **during the rebuild** can corrupt the index file
  itself, not just lose recent progress. Most such damage is caught by the
  integrity check that gates completion, and a failed check now wipes and
  rebuilds automatically on the next start. If a restart after an unclean
  power-off keeps failing instead, delete the index database and let it
  rebuild — your agent files are the source of truth and are never written to.

### ⚠ BREAKING — fact revision identities changed for eight runtime families

`message`, `content_block`, `tool`, `user_input_request`, `plan`, `task`,
`native_marker`, and `effective_state` now derive their revision identity from
the **record that proved the value**, not from the value alone.

This is a correctness fix. These families key facts by an entity that outlives
any single record — an actor's model, a tool call answered several records
later — so two records can legitimately prove the same value for one entity, and
only the record distinguishes those revisions. Binding the value alone collapsed
them into one revision.

What this means for you:

- **Entity identities are unchanged.** `CanonicalFactId`, `ExternalEntityRef`,
  session and project references all keep their values.
- **`FactRevisionId` changed** for those eight families, and therefore so did
  the `SemanticRevisionRef.fact_revision_id` they appear in. Usage,
  actor-affiliation, and artifact-snapshot revisions are unchanged — a repeated
  value there is deliberately one revision.
- Locally the schema v64 rebuild regenerates everything. **A downstream
  consumer that persisted or derived state from those revision ids must rebuild
  it**; the old and new ids do not compare equal, and nothing translates
  between them.

The rule now lives in one place — `Fact::revision_binding` in
`crates/spaghetti-napi/src/adapter/semantic.rs` — so the durable coordinator,
the scoped observer, and the RFC 012C reducers cannot disagree about it.

### ⚠ BREAKING — the SDK entry point is an allowlist

`@vibecook/spaghetti-sdk` used to `export *` twenty-four hand-written contract
modules, putting hundreds of symbols on the public API. Those are gone; the
barrel is now an explicit list of exports that each have a named consumer.

Removed from the package entry point:

- `./contracts/rfc012a.js`
- `./contracts/rfc012b.js`, `rfc012b-client.js`, `rfc012b-hydration.js`,
  `rfc012b-pages.js`
- `./contracts/rfc012c.js`, `rfc012c-unknown-evidence.js`
- `./contracts/rfc012d.js` and its sixteen companions: `rfc012d-actor-envelope`,
  `rfc012d-artifact`, `rfc012d-artifact-availability`,
  `rfc012d-artifact-availability-envelope`, `rfc012d-capability-snapshot`,
  `rfc012d-close`, `rfc012d-completion-envelope`, `rfc012d-continuity-envelope`,
  `rfc012d-event-envelope`, `rfc012d-known-envelope`,
  `rfc012d-replacement-manifest`, `rfc012d-scope-coverage`,
  `rfc012d-source-envelope`, `rfc012d-unknown-wire`, `rfc012d-usage-envelope`,
  `rfc012d-watermark`
- `mergeDurableAndScopedUsage`, `DurableLiveUsageMerge`,
  `DurableUsageContribution`, `ScopedUsageObserverEvent`
- `SCOPED_OBSERVATION_REQUEST_CONTRACT_VERSION`,
  `ScopedObservationRequestError`, `ScopedObservationTransportError`,
  `SessionObservationApply`, `SessionObservationRequest`,
  `SessionObservationRootIdentity`

`observeSession` and `SessionObserver` keep their names but **not their
shape** — the old callback/apply observer is replaced by the async iterator
described below. The identity and event types that replace those contract
modules are generated from Rust and exported from the same entry point.

### New

- **`observeSession(request, options)`** — a store-free observer over one
  session tree, as an async iterator. It opens no database, enumerates no
  unrelated sessions, and follows the root transcript plus its subagent
  transcripts and declared sidecars. Events carry a deterministic `event_id`, a
  `scope_epoch`, and — on semantic events — the same `semantic_revision_ref` a
  durable query returns for that revision. Losing continuity is explicit: the
  observer says so and replaces the epoch with a full snapshot rather than
  dropping events.

  All eleven RFC 012C families are on the stream — message, content block,
  tool, user-input request, plan, task, native marker, effective state, actor
  run, actor affiliation, usage — and `SemanticEvent.value` is a typed
  `RuntimeSemanticValue`, so narrowing on `family` narrows the value with it
  and nothing needs casting. Two evidence limits are documented rather than
  papered over: `plan` revisions come from `ExitPlanMode`/`EnterPlanMode` tool
  evidence rather than `plans/<slug>.md` sidecars, and a `tool_result` whose
  call fell outside the bounded correlation window keeps its content-block
  evidence without a guessed tool name. See
  [SDK README](packages/sdk/README.md#watching-one-session-observesession) and
  [the migration note](docs/integration/chopsticks-observe-session.md).
- **`getReadiness()`** on the observation service (`readiness()` on the native
  engine and the host) — six independent fields, `catalog`, `history`, `usage`,
  `capabilities`, `artifacts`, `search`, each `pending` / `indexing` / `ready` /
  `degraded` / `unavailable` with the commit its evidence was read at.
  `spag doctor` renders it.
- **Catalog-first `listProjects` / `listSessions`** — rows now carry
  `externalRef` (a persistable, restart-stable reference), `catalogState`
  (`discovered` → `transcript_backed` → `hydrated` → `searchable`), `degraded`
  with `degradedReason`, `nativeMessageCount` beside `decodedMessageCount`, and,
  on sessions, `associationBasis` / `associationQuality` /
  `associationProvenance` plus `identityConflicts[]`. Existing fields are
  unchanged.
- **VibeField Phase A helpers** — `queryWatermark`, `isSameSnapshot`,
  `isSameEntity`, `isSameRevision`, `isSameNativeIdentity`, with generated
  `SessionRef` / `ProjectRef` / `SemanticRevisionRef` / `NativeIdentity`. See
  [docs/integration/vibefield-phase-a.md](docs/integration/vibefield-phase-a.md).
- **The RFC 012C value types are importable by name** — `RuntimeSemanticValue`
  and every per-family `*Fact` type, 46 names in all, so a handler signature can
  say `ToolRevisionFact` instead of deriving it from `SemanticEvent['value']`.

### Fixed

- **One spelling for an opaque reference.** Catalog and history rows used to
  spell an entity reference `"1:<digest>"` while `ExternalEntityRef.entity_key`
  spelled the same digest `"v1:<digest>"`, so comparing the two silently failed.
  Everything now uses the RFC 012A `"v1:"` form, and the SDK tests pin it.
- **A startup failure says what failed.** A configured observation that dies
  during startup now surfaces the underlying `EngineError::WorkerFailed` cause
  instead of the generic "worker is unavailable".
- **A truncated plan step no longer fails a decode.** It stays a canonical
  runtime key rather than aborting the record.

### Deprecated

- **`watchSessionTranscript`** — superseded by `observeSession`. It still ships
  and still works, and it is removed **one release after** downstream consumers
  migrate. The porting table is in the SDK README: raw lines from one file
  become reduced revisions from the whole session tree, callbacks become
  `for await`, a silent re-read becomes an explicit `reset`, and continuity
  becomes a claim the observer actually makes.

Design: [RFC 012](docs/rfcs/012-evidence-backed-adapters-and-progressive-readiness.md)
and its children [012B](docs/rfcs/012b-catalog-readiness-and-progressive-startup.md),
[012C](docs/rfcs/012c-runtime-semantics-and-usage-v2.md),
[012D](docs/rfcs/012d-session-scoped-observation.md). Measurements:
[the performance report](docs/rfcs/012-landing-perf-report.md). Known
follow-ups, none of them blockers:
[landing plan §8a](docs/rfcs/012-landing-plan.md#8a-follow-ups-filed-during-the-landing-not-blockers).

<!-- End standing notice. -->

<!-- Standing notice — maintained by hand, not by release-please. -->

## Removed in 0.6.0 — Plane 3 (hooks, chat, plugins)

`spag hooks`, `spag chat`, `spag plugin`, the Hooks Monitor and Chat TUI views,
the `spaghetti-hooks` / `spaghetti-channel` Claude Code plugins, and the SDK
runtime surface (`api.runtime`, `createRuntimeBridge`, `RuntimeEvent`, the
hook/channel watchers and wire types, and the `hookEventsFile` /
`channelSessionsDir` / `channelMessagesDir` source paths) are gone. `ws` and
`@types/ws` left both packages with them.

Transcript ingest, search, query, and live updates are unaffected. No data was
migrated or deleted.

**If Claude Code still has the plugins installed**, removing the npm package
does not remove them — it never did. `spag doctor` reports them and prints:

```bash
claude plugin disable   --scope user spaghetti-hooks@spaghetti
claude plugin uninstall --scope user --keep-data spaghetti-hooks@spaghetti
claude plugin disable   --scope user spaghetti-channel@spaghetti
claude plugin uninstall --scope user --keep-data spaghetti-channel@spaghetti
claude plugin marketplace remove --scope user spaghetti
```

`--keep-data` preserves the plugins' data directories; `~/.spaghetti/hooks` and
`~/.spaghetti/channel` are left alone entirely. Needs Claude Code 2.1.223+.

`api.runtime.listActiveSessions()` is replaced by `listActiveSessionsFromDir(paths.sessionsDir)`.
`ActiveSessionFile` and `paths.sessionsDir` are retained.

Design: [RFC 007](docs/rfcs/007-retire-runtime-bridge.md). The plugins remain in
git history at `211f4b1` if they are ever wanted back.

<!-- End standing notice. -->

## [0.8.0](https://github.com/vibecook-dev/spaghetti/compare/spaghetti-v0.7.0...spaghetti-v0.8.0) (2026-08-24)


### ⚠ BREAKING CHANGES

* **sdk:** delete the consumer-less RFC 012D validators and observer shims

### Features

* **adapters:** emit response-level usage from Codex and Grok ([00afa1f](https://github.com/vibecook-dev/spaghetti/commit/00afa1f5e62d1f8ac97402aa18549ab299c64e44))
* **adapter:** type the observer payload per RFC 012C family ([46ef643](https://github.com/vibecook-dev/spaghetti/commit/46ef64358f2975206c8272efd82e52abbb158848))
* add bounded Codex and Grok support probes ([1e1ab1c](https://github.com/vibecook-dev/spaghetti/commit/1e1ab1cd80ec80b5b081bb153ad17e3fac408c53))
* add confined directory snapshots ([91aa1d1](https://github.com/vibecook-dev/spaghetti/commit/91aa1d1a66393ba70130fc970bb4f229c642fb8f))
* add ordered artifact availability envelope ([dabce41](https://github.com/vibecook-dev/spaghetti/commit/dabce410f81752ee6e1887ce9edaf1d0e3baa17b))
* add RFC 012A/012C N-API JSON contract parity ([c89ab5a](https://github.com/vibecook-dev/spaghetti/commit/c89ab5a7661470a846f1eba013992ebf3ac16b69))
* add RFC 012C native marker contract parity ([39d26d9](https://github.com/vibecook-dev/spaghetti/commit/39d26d9b6fa030fb9f9f38c939bf91a308afb719))
* add RFC 012C native marker facts ([b9cfd1e](https://github.com/vibecook-dev/spaghetti/commit/b9cfd1edc82da04ea4fba364e63fdbec9bf701ad))
* add RFC 012C native marker fixture ([a0f8932](https://github.com/vibecook-dev/spaghetti/commit/a0f8932bdeb13b6a1c15cb360a0197dde0c9a47d))
* add RFC 012C typed content blocks ([d0d98c5](https://github.com/vibecook-dev/spaghetti/commit/d0d98c5b8ca0a3fc443bee17977b89929bf35e24))
* add RFC 012C usage-v2 compatibility-window collector ([7c842d1](https://github.com/vibecook-dev/spaghetti/commit/7c842d1452f0c829889537e4955b750e75e94a9a))
* add RFC 012D explicit resync future ([cf0bfa4](https://github.com/vibecook-dev/spaghetti/commit/cf0bfa47af0b1991d7bc0223d1f1fc4627c572b4))
* add typed RFC 012B catalog client ([31f9692](https://github.com/vibecook-dev/spaghetti/commit/31f9692efede6837d2d900c554557e546ec01587))
* add typed RFC 012D session observer ([f048656](https://github.com/vibecook-dev/spaghetti/commit/f048656df52985a027925aba730e1011b48efa97))
* add weighted source-pass admission ([9ec1075](https://github.com/vibecook-dev/spaghetti/commit/9ec107503aaf2ab5c6aef2b52cd8b8ff5198ea81))
* admit RFC 012D related source lifecycles ([38bb60f](https://github.com/vibecook-dev/spaghetti/commit/38bb60f0e95be0698cbc86836c5389fd48a81057))
* **agent-support:** restore the evidence-backed Claude scope relations ([1f9c17a](https://github.com/vibecook-dev/spaghetti/commit/1f9c17a1c3996822c2bd4db5a44ea9bb261d833d))
* assemble complete catalog projection batches ([eb633df](https://github.com/vibecook-dev/spaghetti/commit/eb633dfa015b9a1ccd9461468f24cdbc703e308e))
* audit scoped directory enumeration ([51c766b](https://github.com/vibecook-dev/spaghetti/commit/51c766b9f4c0ab567f29d495897247c37b6d5f57))
* bind artifact availability to scoped barriers ([dae66ba](https://github.com/vibecook-dev/spaghetti/commit/dae66ba64978924d527c0133c0b68fd455c0bb89))
* bind authorized scoped directory listings ([8d71360](https://github.com/vibecook-dev/spaghetti/commit/8d71360409b776b9e7724fb1fd7b8860161af251))
* bind authorized scoped runtime streams ([b8f308e](https://github.com/vibecook-dev/spaghetti/commit/b8f308eaadaccb235a3cbac2097acd8204e904e0))
* bind catalog negotiation to durable selection ([e84a371](https://github.com/vibecook-dev/spaghetti/commit/e84a371f8e60f099b4e9fe5e46c94dd4d9d4ac27))
* bind catalog sessions to confined locators ([9359037](https://github.com/vibecook-dev/spaghetti/commit/9359037b8b703d44a2bcb4ebe08f110e917091f6))
* bind Claude catalog runtime to typed authority ([856418f](https://github.com/vibecook-dev/spaghetti/commit/856418fde54e88cca2c540c657631b8fcc89aba1))
* bind Claude RFC 012D observation sources ([eabef8d](https://github.com/vibecook-dev/spaghetti/commit/eabef8db2c4ea2776290f3dd6f9aee87cabee340))
* bind Codex catalog runtime to typed authority ([03ecb26](https://github.com/vibecook-dev/spaghetti/commit/03ecb263da1eeeb1f0c95c27889ee8f687a9d1e2))
* bind dependency-free scoped member bootstrap ([4503e3b](https://github.com/vibecook-dev/spaghetti/commit/4503e3b04c3ead9c5010111267a6b889701bf7d3))
* bind Grok catalog runtime to typed authority ([a8b0437](https://github.com/vibecook-dev/spaghetti/commit/a8b0437500379c7adcf35de77df8d3e6bbb19b77))
* bind RFC 012A access requests to typed authority ([639b0b3](https://github.com/vibecook-dev/spaghetti/commit/639b0b3941a07117d68aa7482e12a9b663654b93))
* bind RFC 012A adapter scope joins ([9f17b86](https://github.com/vibecook-dev/spaghetti/commit/9f17b868ac55d9c949b6dfe556537d7cd135aa9d))
* bind RFC 012B readiness to client offers ([8df4062](https://github.com/vibecook-dev/spaghetti/commit/8df40629489a8e6f6c0ca819280d24fc3b0d8226))
* bind RFC 012D barrier capabilities ([29ac300](https://github.com/vibecook-dev/spaghetti/commit/29ac3001a542a9c708c226eb968ec2df08cc0aa6))
* bind RFC 012D child directory locators ([1ccb0dd](https://github.com/vibecook-dev/spaghetti/commit/1ccb0dd6b36f231d10157ee2a04918922c55dc1d))
* bind RFC 012D close to async runtime ([5f494a6](https://github.com/vibecook-dev/spaghetti/commit/5f494a63bf0af4ec59bbc5ad9060267cfd749d1b))
* bind RFC 012D continuity contexts ([b88c902](https://github.com/vibecook-dev/spaghetti/commit/b88c902ab313e509db57e30232f71710d9ed6230))
* bind RFC 012D dynamic relation membership ([8253e51](https://github.com/vibecook-dev/spaghetti/commit/8253e517aa08f9c35a76c7402f946867fdaa696f))
* bind RFC 012D known roots to scoped streams ([3821f2c](https://github.com/vibecook-dev/spaghetti/commit/3821f2caf5da5e0478ae55ccd807bf95f4317c91))
* bind RFC 012D observation sources ([dde701b](https://github.com/vibecook-dev/spaghetti/commit/dde701bf7b55f612485ee451e5430553b12cc053))
* bind RFC 012D poll completions ([5854c5f](https://github.com/vibecook-dev/spaghetti/commit/5854c5fa5ce9e1566e0620fa3533938e6ca52476))
* bind RFC 012D related membership ([72d5ec7](https://github.com/vibecook-dev/spaghetti/commit/72d5ec79318c283d35af8f6c4be9c210b2ef9a68))
* bind RFC 012D related object locators ([f343497](https://github.com/vibecook-dev/spaghetti/commit/f3434970862af0ec8858a1c20ceabbf32b55ae2a))
* bind RFC 012D related source state ([0808fdd](https://github.com/vibecook-dev/spaghetti/commit/0808fdd0faa64636d06f8a35b8c46dfa8f30c611))
* bind RFC 012D unknown wire attachments ([89a0d3b](https://github.com/vibecook-dev/spaghetti/commit/89a0d3b38b4d97efd308c07ec24f18ccf78912d3))
* bind RFC 012D unknown-wire delivery ([71afae4](https://github.com/vibecook-dev/spaghetti/commit/71afae4275117207f19ca5dbe9ae2a2c6bdf505a))
* bind scoped artifact generations ([38ce442](https://github.com/vibecook-dev/spaghetti/commit/38ce4429daefce665018490546a56c9435fb436b))
* bind scoped artifacts to source declarations ([18971a8](https://github.com/vibecook-dev/spaghetti/commit/18971a89a6f1c50d66390c7010ac150620630729))
* bind scoped attachments to source instances ([eddecc2](https://github.com/vibecook-dev/spaghetti/commit/eddecc29f27cd86dcd4b42ec64641cbe506a6323))
* bind scoped directory entry accounting ([6592ff1](https://github.com/vibecook-dev/spaghetti/commit/6592ff199aeca56d61c6f28439ca5eb055bda1e0))
* bind scoped directory member reads ([8c53831](https://github.com/vibecook-dev/spaghetti/commit/8c538314bb357fef6193bb3c341aa5f89f1407b0))
* bind scoped directory membership contracts ([2870468](https://github.com/vibecook-dev/spaghetti/commit/287046844afb9c901dc52549f234e21db60e3d85))
* bind scoped discovered member identities ([39cf02e](https://github.com/vibecook-dev/spaghetti/commit/39cf02ee45bdb41e0b580dff7bbf7545fbda4f11))
* bind scoped member decoder descriptors ([d9d2b12](https://github.com/vibecook-dev/spaghetti/commit/d9d2b127c9afbd159f888f5c923ba96813b3ad18))
* bind scoped observation source reservations ([71c3f1e](https://github.com/vibecook-dev/spaghetti/commit/71c3f1ed96aaac6cb5a19799ba11dee959672617))
* bind scoped runtime sources to attachment roots ([db947dc](https://github.com/vibecook-dev/spaghetti/commit/db947dc4aea03d949b7b752bafaf1c139499f162))
* bind unknown evidence to RFC 012D completion ([8ad97ea](https://github.com/vibecook-dev/spaghetti/commit/8ad97ea3a3602bdb84746457b3b9b96bb5369f6f))
* bind unknown evidence to RFC 012D watermarks ([ff42a6f](https://github.com/vibecook-dev/spaghetti/commit/ff42a6f063012a1e1e0bd1357127ef551b6b9195))
* bootstrap configured RFC 012D directories ([15b5675](https://github.com/vibecook-dev/spaghetti/commit/15b56752f4a144a75884cea7623f40baf5187411))
* capture confined scoped artifacts ([386e025](https://github.com/vibecook-dev/spaghetti/commit/386e0251780392a373c65631617391c028426905))
* carry RFC 012A mappings into durable batches ([e646b56](https://github.com/vibecook-dev/spaghetti/commit/e646b563a55270a1dbea1746747468588943f5b0))
* **catalog:** replace the 012B publication stack with catalog-first startup ([c22ada9](https://github.com/vibecook-dev/spaghetti/commit/c22ada94a7ff043a61a2e8cc242d8419773ff98b))
* **catalog:** wire catalog-first startup to the SDK, CLI, and playground ([f9c5a69](https://github.com/vibecook-dev/spaghetti/commit/f9c5a69ab558413264eca25508789c99fcb0bf0a))
* classify RFC 012A record mappings ([596535b](https://github.com/vibecook-dev/spaghetti/commit/596535b6c313b8a660bff9fbdd5ea7c2b76488d4))
* classify RFC 012B refresh failures ([b1cdf88](https://github.com/vibecook-dev/spaghetti/commit/b1cdf882b95c9f41eceacbbbee6b143b69103029))
* **claude:** emit the eight missing RFC 012C fact families ([13efc08](https://github.com/vibecook-dev/spaghetti/commit/13efc087ed203a03b546afe14e459d0b015eda1e))
* compose configured RFC 012D root authority ([551d559](https://github.com/vibecook-dev/spaghetti/commit/551d5590b97238c70d1ce535ad4cba42c95645c1))
* compose real Claude scoped sidecar replay ([cad998f](https://github.com/vibecook-dev/spaghetti/commit/cad998f7cd9c625cd8be04d2af754e7274274d3e))
* compose scope-join directories to fixed point ([c6f57aa](https://github.com/vibecook-dev/spaghetti/commit/c6f57aa42175a47700474f8da0e49cd3c75a98a8))
* continue history after catalog readiness ([1766d33](https://github.com/vibecook-dev/spaghetti/commit/1766d3358c6d64369eade584789adf992f92e76d))
* coordinate authorized RFC 012B catalog builds ([0595b8a](https://github.com/vibecook-dev/spaghetti/commit/0595b8ae112c6b7667f8656d6263d86ae2d5a8c0))
* coordinate RFC 012B workloads fairly ([994028c](https://github.com/vibecook-dev/spaghetti/commit/994028c97f8f0cbc382aef23f70d73e582350f84))
* cover RFC 012C effective-state dimensions ([4eef047](https://github.com/vibecook-dev/spaghetti/commit/4eef047bcd8c63360cef360aac92c87873d1a8ec))
* define RFC 012B coverage-plan successors ([55a970b](https://github.com/vibecook-dev/spaghetti/commit/55a970be6d9e4751381584e2486758c96fe77f28))
* derive Claude RFC 012D scope joins ([506bcd4](https://github.com/vibecook-dev/spaghetti/commit/506bcd4b46303a51994cfbf1cb31086f0bfb614b))
* derive RFC 012A tier digests from decoded facts ([d7b8d8c](https://github.com/vibecook-dev/spaghetti/commit/d7b8d8ce323148859793d8a67ad5d0a5f2bd1f1d))
* dispatch known RFC 012D wire events ([1bda3ee](https://github.com/vibecook-dev/spaghetti/commit/1bda3ee0549b0c1ae4f86146f6354407220cbff8))
* **engine:** make response-level usage the only usage query ([619ba21](https://github.com/vibecook-dev/spaghetti/commit/619ba21e7043a6bec9df127dfaa1757179092d3b))
* execute catalog reads on query workers ([eadb752](https://github.com/vibecook-dev/spaghetti/commit/eadb752e483c5ced6c535ad3973f99897dd8185e))
* execute confined catalog hydration ([57618ad](https://github.com/vibecook-dev/spaghetti/commit/57618ada3711f298a7132aeefb07edea29accd55))
* execute RFC 012D related source reads ([b503a5f](https://github.com/vibecook-dev/spaghetti/commit/b503a5f99288c8501190cf2ecb88d853b980bad6))
* expose RFC 012B selected hydration ([997ce39](https://github.com/vibecook-dev/spaghetti/commit/997ce39e378a9126ab124b5843435d553df5bbb7))
* expose RFC 012D native observer owner ([630071e](https://github.com/vibecook-dev/spaghetti/commit/630071ed09b2e84776e02ef2c8f2a503d04bb015))
* expose withheld RFC 012B catalog queries ([08e8d87](https://github.com/vibecook-dev/spaghetti/commit/08e8d87919404b9e239507592e15612d61163125))
* frame authorized append directory members ([3333bc2](https://github.com/vibecook-dev/spaghetti/commit/3333bc2b94c915b5217f9e9731741ecf13f8b9ba))
* frame scoped members with replace driver ([aa9b15d](https://github.com/vibecook-dev/spaghetti/commit/aa9b15d6a0abfd1e1524b2bfc9d146db7031d14a))
* freeze bounded RFC 012D observer requests ([380c76d](https://github.com/vibecook-dev/spaghetti/commit/380c76d2b82b49432f29227a4980aaa813135585))
* freeze bounded unknown evidence snapshot ([2925be5](https://github.com/vibecook-dev/spaghetti/commit/2925be57d6e1e545865a0f27a1a570b775882f48))
* freeze catalog plan sources before reads ([1cfe06d](https://github.com/vibecook-dev/spaghetti/commit/1cfe06d026379a6e8faeea087205e2dce7180b9a))
* freeze RFC 012D completion envelopes ([dc0fc41](https://github.com/vibecook-dev/spaghetti/commit/dc0fc417d15b8ed0c400887ff07b28c205e58dfa))
* freeze RFC 012D poll watermarks ([c28bdef](https://github.com/vibecook-dev/spaghetti/commit/c28bdef7782af05266ba001952f0dd42b9290042))
* freeze scoped artifact availability ([a8175c2](https://github.com/vibecook-dev/spaghetti/commit/a8175c225f43b651b5a2985cc8638301e2629ce6))
* freeze scoped artifact availability wire ([2d1ead4](https://github.com/vibecook-dev/spaghetti/commit/2d1ead4232a864dd8ba49dc1608dc859b50cc02b))
* gate RFC 012D scoped support preparation ([e5a0a9a](https://github.com/vibecook-dev/spaghetti/commit/e5a0a9a174abd43aebdfe90868a169a15e83f4d3))
* **grok:** read exact per-turn usage instead of a session-scoped estimate ([99cb41c](https://github.com/vibecook-dev/spaghetti/commit/99cb41ccbfe8198f8e16f8337f4531a014e1e51f))
* mediate RFC 012D decoder dependencies ([a7cfcb6](https://github.com/vibecook-dev/spaghetti/commit/a7cfcb6776a4cd8bf2859e22e06c8c977fa9cb9b))
* multiplex known RFC 012D events ([612e680](https://github.com/vibecook-dev/spaghetti/commit/612e680f9ae8ae9fedd2f75e66d19e487c721cce))
* **observer:** evaluate the declared scope program instead of restating it ([960921d](https://github.com/vibecook-dev/spaghetti/commit/960921dc18966f2d544bc3b5f2f2f9c636734cae))
* **observer:** rebuild the store-free session observer small ([c292e00](https://github.com/vibecook-dev/spaghetti/commit/c292e005e0eab76648d190f4748387021fd0615e))
* **observer:** surface records the decoder cannot interpret ([55d6bc3](https://github.com/vibecook-dev/spaghetti/commit/55d6bc39962ff5c1d115605a8535ba3875ffaea6))
* order scoped artifact availability ([89e2ead](https://github.com/vibecook-dev/spaghetti/commit/89e2eada6251b0b78002b1380f1b2e789f7d84ba))
* persist bounded RFC 012C unknown evidence ([2710e4d](https://github.com/vibecook-dev/spaghetti/commit/2710e4d8fce33ec8e47d64342e8ff79798bab7e3))
* persist cold RFC 012B source failures ([61036f9](https://github.com/vibecook-dev/spaghetti/commit/61036f94d164e612bbc1cd771dc48477dd1db9be))
* persist RFC 012B coverage plan replacements ([806b8d2](https://github.com/vibecook-dev/spaghetti/commit/806b8d2543f763a523a40694ca3100649dcf2a39))
* persist RFC 012B degraded refresh recovery ([d5b92e0](https://github.com/vibecook-dev/spaghetti/commit/d5b92e0958a144eee190c165d4e0571f507d5502))
* persist RFC 012B partial build progress ([b303b49](https://github.com/vibecook-dev/spaghetti/commit/b303b491be2b49df3f121f4669f40fc660e46bf6))
* persist RFC 012B source generation epochs ([e98beb6](https://github.com/vibecook-dev/spaghetti/commit/e98beb622a74f0377d08c7d47c1a64991013d86d))
* plan RFC 012D related source joins ([54bbbff](https://github.com/vibecook-dev/spaghetti/commit/54bbbff40e269fbb23208b96c5f6e27f17626e13))
* **playground:** complete catalog-first session startup ([a49b7ac](https://github.com/vibecook-dev/spaghetti/commit/a49b7ac707b3fa0bd94571ea7a946f37226b1418))
* **playground:** show the library from the catalog while history converges ([c301fff](https://github.com/vibecook-dev/spaghetti/commit/c301fffc91c14939a146799b02629d92b08961f2))
* prepare configured RFC 012D append sources ([abe2961](https://github.com/vibecook-dev/spaghetti/commit/abe296170b75ed6a1c2cf54ff1076181b498be88))
* prepare observation watchers before history scans ([5e45547](https://github.com/vibecook-dev/spaghetti/commit/5e455473629d5bce1ca121b2c67c0440ec221d7a))
* prepare RFC 012D directory relations ([d832dbc](https://github.com/vibecook-dev/spaghetti/commit/d832dbc50aa6220e0318425207771bbf36577ed2))
* prepare RFC 012D related source authority ([306e0bf](https://github.com/vibecook-dev/spaghetti/commit/306e0bfa50f92083daad9adb7c0a7f915a58a1c6))
* preserve unknown RFC 012D wire events ([765dda4](https://github.com/vibecook-dev/spaghetti/commit/765dda46adb7eed15eda9a444d8310a46408d0b1))
* project catalog membership into typed publication ([62ffa13](https://github.com/vibecook-dev/spaghetti/commit/62ffa133699621f6b34584a79660fcef343b2c1b))
* project Claude catalog members for publication ([e5c27e3](https://github.com/vibecook-dev/spaghetti/commit/e5c27e3e8920ebcae8ae4b45b77bd00ca9d98bb4))
* project Codex catalog members for publication ([3d187ba](https://github.com/vibecook-dev/spaghetti/commit/3d187ba7d1032c64c12a9fcc802323e5a5f997b5))
* project Grok catalog members for publication ([66a7fe2](https://github.com/vibecook-dev/spaghetti/commit/66a7fe29165e125edd44fa5d1611f809103cffd8))
* publish catalog refreshes from complete coverage ([5932f79](https://github.com/vibecook-dev/spaghetti/commit/5932f7946cede4cffadd1772abea9e5e4bac1172))
* publish initial catalogs from frozen plans ([42704ad](https://github.com/vibecook-dev/spaghetti/commit/42704adcef3fa7b7d686f85b1c59c9e0080a0330))
* qualify RFC 012C effective state ([45ce567](https://github.com/vibecook-dev/spaghetti/commit/45ce567f0b50b127642340c1d7ee29be31cb2476))
* reconcile adapter catalog refresh scans ([2205549](https://github.com/vibecook-dev/spaghetti/commit/2205549978d01070fed0936db2ad0543987ab8a2))
* reconcile all RFC 012C semantic families ([fd69883](https://github.com/vibecook-dev/spaghetti/commit/fd698839b209eac2000007784fa95e6e76935e01))
* reconcile catalog refresh source generations ([9e66a45](https://github.com/vibecook-dev/spaghetti/commit/9e66a454eafe388221d6e80f3fada8858e97083b))
* reconcile RFC 012D related sources ([6748df4](https://github.com/vibecook-dev/spaghetti/commit/6748df44c2c14e680f48e7846c948deeb22ad266))
* recover RFC 012B initial integrity failures ([aea79a7](https://github.com/vibecook-dev/spaghetti/commit/aea79a7a0b2805ff45cfbd6bc8cd02ffd0741859))
* recover RFC 012B integrity errors ([8910505](https://github.com/vibecook-dev/spaghetti/commit/89105058f2b724b842b7f038581ebf016261042f))
* reduce bounded RFC 012C unknown evidence ([4b20b5f](https://github.com/vibecook-dev/spaghetti/commit/4b20b5fa7132abb1c2d3f5f251635656d7ae11a8))
* reduce RFC 012C content-block state ([4e6d328](https://github.com/vibecook-dev/spaghetti/commit/4e6d328f900cbbc02fbfa13343b85d316b6e0d3b))
* reduce RFC 012C native marker state ([7ce4ae0](https://github.com/vibecook-dev/spaghetti/commit/7ce4ae0150e95c7cc1e6ec463ce96d9327966878))
* refresh configured catalogs after source changes ([32e9867](https://github.com/vibecook-dev/spaghetti/commit/32e98670d7122fe2a707b562694c036cbd607c50))
* release configured history scans concurrently ([00cd0f5](https://github.com/vibecook-dev/spaghetti/commit/00cd0f5801be392438f4b534b89d3f889c182d29))
* replay append-delimited scoped directory members ([fb44b96](https://github.com/vibecook-dev/spaghetti/commit/fb44b9672d6e994de16fde83a7481044d5d1ca8b))
* replay RFC 012D directory snapshots ([e51b1a0](https://github.com/vibecook-dev/spaghetti/commit/e51b1a087d0094985a88229b1a53749f21546d51))
* resync RFC 012D directory polls ([374df12](https://github.com/vibecook-dev/spaghetti/commit/374df122ad32ee76c006f9e4f5543384b6e38fb8))
* retain authorized observation source contracts ([9238ac2](https://github.com/vibecook-dev/spaghetti/commit/9238ac2dbc951feddf903123c3c95cbfa8a6bd2b))
* retain bounded unknown evidence in scoped state ([cebdee0](https://github.com/vibecook-dev/spaghetti/commit/cebdee099942781994a425fa63b5454c8be6ecb8))
* retain RFC 012D scoped join state ([a4ff2f9](https://github.com/vibecook-dev/spaghetti/commit/a4ff2f96162b21f523d6abea8cdacfa162f85ca1))
* retain RFC 012D sidecar in event drains ([b9d8824](https://github.com/vibecook-dev/spaghetti/commit/b9d8824ef5a6d89c3e275459e7d37ad3b507353b))
* retain scoped member bootstrap authority ([a951d2d](https://github.com/vibecook-dev/spaghetti/commit/a951d2d77213650b3cf051b3f00f689613a82a98))
* **rfc-012a:** add Factory candidate adapter without common-runtime change ([b968bab](https://github.com/vibecook-dev/spaghetti/commit/b968bab8f912f8e8f837306599def9c658591fa0))
* **rfc-012:** add frozen X1/X2/B5/D5 experiment helpers ([31b8362](https://github.com/vibecook-dev/spaghetti/commit/31b8362e4e0be05bc4c4851da88a3d82485cb015))
* **rfc-012a:** promote Claude 2.1.223 durable path with rollback ([7b90858](https://github.com/vibecook-dev/spaghetti/commit/7b90858f51e55ea6371b8eafa693ed07c5f6d870))
* **rfc-012b:** bind Claude catalog producer to source-instance roots ([ef4536f](https://github.com/vibecook-dev/spaghetti/commit/ef4536fffdaca94b24a3554f7bdf028a451be05d))
* **rfc-012:** C1 state/interaction fixtures and remaining Exit evidence ([b591bcc](https://github.com/vibecook-dev/spaghetti/commit/b591bcc33755ed837a187c8048c3cf5bcef70392))
* **rfc-012:** close remaining honest Wave III gates except X4 ([994f145](https://github.com/vibecook-dev/spaghetti/commit/994f1453efbdf7feac5cb12193b342337019a12f))
* **rfc-012:** close remaining program gates with promoted fixture-agent ([c51c072](https://github.com/vibecook-dev/spaghetti/commit/c51c072d8f3419a6c8fe2a09e81e2e17fa1ed94d))
* **rfc-012c:** merge durable and scoped usage by semantic revision ([1cc37da](https://github.com/vibecook-dev/spaghetti/commit/1cc37da94b06b772afb90a2e5753e02b322d5263))
* **rfc-012d:** decode scoped directory children through replace driver ([28c2ddb](https://github.com/vibecook-dev/spaghetti/commit/28c2ddb8afd6e0528e37e84a8f8ffad5ff97aabf))
* **rfc-012d:** feature-flag watchSessionObservationShadow beside transcript tail ([591b79f](https://github.com/vibecook-dev/spaghetti/commit/591b79f576a12946ae54896d37c3436aafd310d0))
* **rfc-012:** freeze scoped effective-state replacement from C1 ([c735eea](https://github.com/vibecook-dev/spaghetti/commit/c735eea9ee7e4b43de19cd9b2ad2f680f1878704))
* **rfc-012:** freeze scoped message and task replacement from C1 ([e6950e1](https://github.com/vibecook-dev/spaghetti/commit/e6950e12b53111e354911a0ab168d693817c39d7))
* **rfc-012:** freeze scoped plan replacement from C1 ([ce6fb4d](https://github.com/vibecook-dev/spaghetti/commit/ce6fb4d82c88098723067001de7921ba7662a838))
* **rfc-012:** freeze scoped tool replacement from C1 ([60252db](https://github.com/vibecook-dev/spaghetti/commit/60252db7455890b1a4ced459844e81c7cee564c8))
* **rfc-012:** freeze scoped user-input-request replacement from C1 ([d2a2228](https://github.com/vibecook-dev/spaghetti/commit/d2a222856b6f9f32b2fed9d68d9f5a1753279a44))
* **rfc-012:** start honest Wave III remaining gates ([ca8fc3f](https://github.com/vibecook-dev/spaghetti/commit/ca8fc3fbba43adaaeb8e4042f48dfebe21d5c7c8))
* schedule bounded RFC 012B catalog hydration ([c083bfe](https://github.com/vibecook-dev/spaghetti/commit/c083bfea09fd745f59851895999888a41c7e8398))
* **sdk,cli:** serve corrected usage to the CLI and playground ([74b3add](https://github.com/vibecook-dev/spaghetti/commit/74b3add057253faecb91c51cc5d82c58dad19e91))
* **sdk:** expose session decode state and identity ([c0232c9](https://github.com/vibecook-dev/spaghetti/commit/c0232c91adc4f22bb6eaf1e87694aec3a787e7fb))
* **sdk:** expose the session observer to Node and retire the old wire ([42360e4](https://github.com/vibecook-dev/spaghetti/commit/42360e4c2cafed7c0e59d65706c0491603d2c40e))
* **sdk:** generate TypeScript bindings from Rust with ts-rs ([a8bfe0e](https://github.com/vibecook-dev/spaghetti/commit/a8bfe0e8c87ac1733bd5de4736543fe07d8ebea3))
* **sdk:** let a consumer name the value type a handler takes ([1d4fba0](https://github.com/vibecook-dev/spaghetti/commit/1d4fba0b1154100407a233c3446cc36bf2635f26))
* **sdk:** let an AbortSignal end an observeSession iteration ([c981633](https://github.com/vibecook-dev/spaghetti/commit/c981633a56cab310f47e0294a0ce3ec20c3bdbb0))
* **sdk:** observeSession — the store-free session observer as an async iterator ([3a2c0c4](https://github.com/vibecook-dev/spaghetti/commit/3a2c0c4237b8195fc3275df1b26b89d16e9ac4b5))
* select remaining RFC 012C scoped families ([d478679](https://github.com/vibecook-dev/spaghetti/commit/d4786799988a3d3584228cbd1cdf417d67af097c))
* shadow Claude tail with RFC 012D observer ([7c916bc](https://github.com/vibecook-dev/spaghetti/commit/7c916bc054004a639e59e64fbb54885ea9e5cc7c))
* share RFC 012D bounded pass permits ([9402172](https://github.com/vibecook-dev/spaghetti/commit/94021723c68d2f6c2b1ffaa708dc1aad539af868))
* start configured sources behind a global catalog barrier ([dba4f45](https://github.com/vibecook-dev/spaghetti/commit/dba4f454cc70a2ffcc67d13b2d3158430d409c4f))
* start observation hosts through global planning ([ca7e1fc](https://github.com/vibecook-dev/spaghetti/commit/ca7e1fcecbc0ee5b7bc710a23c3fe604964aedbf))
* supervise configured RFC 012D append observers ([3e8bf44](https://github.com/vibecook-dev/spaghetti/commit/3e8bf44d7fca665a56498772b1e239991343eec6))
* supervise RFC 012D related source replay ([ddadd5b](https://github.com/vibecook-dev/spaghetti/commit/ddadd5ba8efe5ebc53f2507692f96606263b51fa))
* track RFC 012D applied readiness ([eb68a6a](https://github.com/vibecook-dev/spaghetti/commit/eb68a6aed4c72382a6d091958a617c09b31f2bcd))
* track RFC 012D applied resync ([62e6a0b](https://github.com/vibecook-dev/spaghetti/commit/62e6a0b6af5d411b010c0a9ca9bb6edeaa74bba5))
* unify RFC 012C effective-state reduction ([e8ea18e](https://github.com/vibecook-dev/spaghetti/commit/e8ea18e105b954f364dc9b0ab2bfa208d3ce06c1))
* wire RFC 012B coverage plan replacements ([7dd5143](https://github.com/vibecook-dev/spaghetti/commit/7dd514360feda9e110d68957acb6d2871189bfcf))


### Bug Fixes

* authorize bounded directory membership sources ([f3bca3e](https://github.com/vibecook-dev/spaghetti/commit/f3bca3e68f51f526305ea3add35143c1826b2f83))
* bind catalog components to verified source streams ([5f1d5ee](https://github.com/vibecook-dev/spaghetti/commit/5f1d5ee1edba8e73eaac096deb862c4fb62ad979))
* bind catalog queries to held snapshots ([8c327cd](https://github.com/vibecook-dev/spaghetti/commit/8c327cda13efad3efbd6daa69c03e02c912c755e))
* bind RFC 012C identities to semantic values ([7fa5ede](https://github.com/vibecook-dev/spaghetti/commit/7fa5ede9bfaaef321c21230d1f174b1cf1de17a5))
* bind RFC 012C live merge identities ([7a4198e](https://github.com/vibecook-dev/spaghetti/commit/7a4198eb5767a014c0be96a8334de94e412c2930))
* bind RFC 012D directory reads to passes ([5173dbc](https://github.com/vibecook-dev/spaghetti/commit/5173dbcd17e945498f0d545575eacca3ad9272d8))
* bind runtime fact revisions to the record that proved them ([debff36](https://github.com/vibecook-dev/spaghetti/commit/debff3677999f2a90280d2a299c2d7dbe4c365f7))
* bind scoped directory identity authority ([149442a](https://github.com/vibecook-dev/spaghetti/commit/149442a7cd99b41685cf898c25faf8158d566489))
* bound directory snapshot fan-out ([a2bb82d](https://github.com/vibecook-dev/spaghetti/commit/a2bb82df39a5cc435b5614e254041cf6b4991935))
* bound retained unknown record evidence ([ca13e17](https://github.com/vibecook-dev/spaghetti/commit/ca13e17161f3383e679a900b62ed99c3f28ef8b5))
* bound RFC 012D watcher scheduling ([76ca2ff](https://github.com/vibecook-dev/spaghetti/commit/76ca2ff47a059bb233db3bba19062d06b49850c7))
* **catalog:** admit session directories, and keep the library off the ingest path ([20afef6](https://github.com/vibecook-dev/spaghetti/commit/20afef6c11f9f7c68069d9b20688bf27c4ce2d49))
* **catalog:** keep a failure reason inside the row that records it ([d0fdd5d](https://github.com/vibecook-dev/spaghetti/commit/d0fdd5d4124052fa3eacc938190565a717b967dd))
* **catalog:** keep listProjects/listSessions on their existing contract ([497b95c](https://github.com/vibecook-dev/spaghetti/commit/497b95c7ee722a17033ff8004603a390e649a785))
* **catalog:** name claude projects from decoded session cwd ([d17bb14](https://github.com/vibecook-dev/spaghetti/commit/d17bb1417784e52d83c541ef646ad1ca81c465ad))
* **catalog:** recover entity keys by prefix, not by splitting the id ([7bbb725](https://github.com/vibecook-dev/spaghetti/commit/7bbb725481baa4f8ad7133087b78027fabc11d01))
* **ci:** close RFC 012 landing gaps ([c8ebeca](https://github.com/vibecook-dev/spaghetti/commit/c8ebecaa57f28406f51b327385b1f5f0abc5fbf4))
* **ci:** compare index sets, not index creation order, in the ingest oracle ([bd2b29b](https://github.com/vibecook-dev/spaghetti/commit/bd2b29b495a7fec7c20b271adc9459601988540c))
* **ci:** restore cross-platform landing gates ([68e53d8](https://github.com/vibecook-dev/spaghetti/commit/68e53d8039cb3a18361c383291a864de8c9bdbc6))
* **ci:** satisfy Rust 1.98 digest parsing lint ([50386d6](https://github.com/vibecook-dev/spaghetti/commit/50386d6ef1e5d5abc9b6290c1aa5c65c91ae295f))
* **cli:** resolve a project by the id the catalog handed the caller ([856ac91](https://github.com/vibecook-dev/spaghetti/commit/856ac91fb93933ebd099c7a6b6886db914ab1f4c))
* compose RFC 012D member coverage ([d01663c](https://github.com/vibecook-dev/spaghetti/commit/d01663c0b4edeb73def170be57a19a592c86eb9e))
* consume RFC 012D contract replay once ([0dd9351](https://github.com/vibecook-dev/spaghetti/commit/0dd9351e9dd2ad673cdc58c3e6780abe5a5dd8c8))
* **core:** make source paths portable on Windows ([68c1e0a](https://github.com/vibecook-dev/spaghetti/commit/68c1e0a281c1197847dcddf831653664f91ffa74))
* decode scoped members from authorized reads ([a4dbf80](https://github.com/vibecook-dev/spaghetti/commit/a4dbf8027bb24c14cdf37fa19a91e06cc7839a89))
* **engine:** gate remaining deferred-index probes in detail, timeline, and orchestration queries ([eac0e32](https://github.com/vibecook-dev/spaghetti/commit/eac0e32a84754838ba5bc0cb1e5247407d3cf3c8))
* **engine:** keep catalog and history queries bounded during deferred bootstrap ([ad0cc3d](https://github.com/vibecook-dev/spaghetti/commit/ad0cc3de95e389a87757ac3119e8ea1987c7c7d9))
* **engine:** keep catalog-first startup nonblocking ([c0e5cbf](https://github.com/vibecook-dev/spaghetti/commit/c0e5cbff1fb9e8d81643bc8bb941e5964420fa38))
* **engine:** one spelling for every opaque RFC 012A reference ([25ae8e2](https://github.com/vibecook-dev/spaghetti/commit/25ae8e2ea0968196eb8c5bc0679b5f06f849f734))
* **engine:** record rejected semantic revisions instead of failing the commit ([e847ca6](https://github.com/vibecook-dev/spaghetti/commit/e847ca66ea56e36281afc249875390880018ec6d))
* **engine:** satisfy the projection ratchet; admit the atis-latch record type ([c2852b8](https://github.com/vibecook-dev/spaghetti/commit/c2852b80b45cfe81a6b4ef94f1cafe7fd703b41d))
* **engine:** stop the usage-v2 intern from rescanning every contribution ([669c34d](https://github.com/vibecook-dev/spaghetti/commit/669c34d9d13883ca1f574352d4b9be117eba2cd9))
* expose blocked catalog retry coverage ([d0b0974](https://github.com/vibecook-dev/spaghetti/commit/d0b097498876e7a0255ef1e310bff422dc879d8c))
* fail closed on unfrozen RFC 012D interaction wire ([10e88d3](https://github.com/vibecook-dev/spaghetti/commit/10e88d37a452a4dbbcb518bcf1dac57c232f5e51))
* isolate RFC 012D bootstrap object failures ([eae57fd](https://github.com/vibecook-dev/spaghetti/commit/eae57fdac3104b89d673cdf7c05edd798dff55d1))
* keep a truncated plan step a canonical runtime key ([4568796](https://github.com/vibecook-dev/spaghetti/commit/4568796db3c822351b4bd5a0a0aa5b87e6ef3fd6))
* keep catalog membership stable across policy views ([307c64b](https://github.com/vibecook-dev/spaghetti/commit/307c64b81e5dfb405b1952a6759599399c03d99f))
* make RFC 012A scope joins replaceable ([48ce7b3](https://github.com/vibecook-dev/spaghetti/commit/48ce7b34484a6496deb62636871bfbac02f5e58e))
* make RFC 012D bootstrap completion atomic ([a798f02](https://github.com/vibecook-dev/spaghetti/commit/a798f0297f2b6e3750a2fb3dc448f8aab3ead441))
* **napi:** clear non-dead-code clippy lints and gate the fixture-parser module ([b2cbf86](https://github.com/vibecook-dev/spaghetti/commit/b2cbf864837f7a3aa1150cc35dd0dffa3497d570))
* negotiate RFC 012D unknown wire before probing ([e13724e](https://github.com/vibecook-dev/spaghetti/commit/e13724e8b6c86dfcae474a900b95646cfe417fd7))
* **observer:** retraction names the generation that owned the fact ([beb08e3](https://github.com/vibecook-dev/spaghetti/commit/beb08e32804e68d97d02de8289d69b791cf57726))
* **playground:** wait for supervisors before asserting on them ([e09ee94](https://github.com/vibecook-dev/spaghetti/commit/e09ee9498bf2a2e794a078fdfe0700e75a905e8a))
* preserve artifact replay foreign keys ([c986373](https://github.com/vibecook-dev/spaghetti/commit/c98637322c5874cd2fcad0172172972e5925f698))
* preserve catalog project reassociation evidence ([27a3e7a](https://github.com/vibecook-dev/spaghetti/commit/27a3e7a59a3151bdf4bf1da8ebea0208f46df9b7))
* preserve directory members across corrections ([6af4b29](https://github.com/vibecook-dev/spaghetti/commit/6af4b29f87f9b0ae179f783477bcd3069b0b432a))
* preserve RFC 012C selected identity axes ([a3cc8b5](https://github.com/vibecook-dev/spaghetti/commit/a3cc8b59bc920cc02f576ce23b8b4c92af19f4bb))
* preserve RFC 012D related source ownership ([68cf2d9](https://github.com/vibecook-dev/spaghetti/commit/68cf2d960399ff5deadb65474458a560b07c9e45))
* preserve RFC 012D watcher overflow ([d58ea42](https://github.com/vibecook-dev/spaghetti/commit/d58ea42c27a45dac02eabbda959052401aa2100b))
* publish coherent query completion telemetry ([7dfedbb](https://github.com/vibecook-dev/spaghetti/commit/7dfedbb4e778a235ee5b214d4edf1bae6a1e9a09))
* publish cold RFC 012B replacement epochs ([ba261f0](https://github.com/vibecook-dev/spaghetti/commit/ba261f029ae14798ad0a0f562b53f10167941937))
* recover cold RFC 012B integrity failures ([72218db](https://github.com/vibecook-dev/spaghetti/commit/72218db6a9aca083c7bf01770ddeebd00a8f86f2))
* redact RFC 012C collector source path ([3e0fac1](https://github.com/vibecook-dev/spaghetti/commit/3e0fac16ab2e5917c28b44bb2dd3be4b4e9c6b68))
* reject incomplete catalog authority before source access ([643ddee](https://github.com/vibecook-dev/spaghetti/commit/643ddeea1b31fa804631dccd459d57dd1fcac065))
* reject uncomposed RFC 012D relations ([9f9749b](https://github.com/vibecook-dev/spaghetti/commit/9f9749bde2efce4d2ee2ea301f66f2d7c527a796))
* replay canonical artifact revisions ([9dd7428](https://github.com/vibecook-dev/spaghetti/commit/9dd74287dee6823c5f198cdc140c92c80211e3d6))
* report selected hydration honestly ([91c04e0](https://github.com/vibecook-dev/spaghetti/commit/91c04e08847f767fccc64cbcb6e0ed74fb02549f))
* reserve related source bytes per object ([302c96c](https://github.com/vibecook-dev/spaghetti/commit/302c96c5a8fe70d8478e75a548d319a6cb4247d7))
* reserve RFC 012D related membership capacity ([b382c21](https://github.com/vibecook-dev/spaghetti/commit/b382c211acd6236209ec85fb6a8d961fa3ed6f01))
* restore RFC 012 authority and evidence boundaries ([465dd46](https://github.com/vibecook-dev/spaghetti/commit/465dd4663b4c8d4566c9599ce97257335160891c))
* retain prior-plan catalog continuations ([220f604](https://github.com/vibecook-dev/spaghetti/commit/220f604c0ee1f6f2358f11db67731926f7efb52e))
* retain RFC 012D directory owner bindings ([ba978e8](https://github.com/vibecook-dev/spaghetti/commit/ba978e872417397d329f2800e5d2c93240690c90))
* retain RFC 012D dynamic source state ([0d19336](https://github.com/vibecook-dev/spaghetti/commit/0d193362de8d8fcebdc5f81d08410049e39272d1))
* revalidate RFC 012D directory membership ([912bc72](https://github.com/vibecook-dev/spaghetti/commit/912bc72b1ec9072ffa72e061cf999ba0a40c4319))
* **rfc-012:** admit catalog queries before FTS and honest remaining gates ([9d6be3a](https://github.com/vibecook-dev/spaghetti/commit/9d6be3ae31d4eeb26722ced1b5e9926521aeae08))
* **rfc-012a:** leave promoted Claude artifact digest unpinned ([58e0005](https://github.com/vibecook-dev/spaghetti/commit/58e0005d47ff3a6c71dc5bf55392134c3f6dec3c))
* **rfc-012:** close honesty leftovers, Promoted directory composition, and query-pass-pool ([c5d57b8](https://github.com/vibecook-dev/spaghetti/commit/c5d57b8de611265fabec6c0731c83db169389fce))
* route Claude catalog decoding through common runtime ([9189065](https://github.com/vibecook-dev/spaghetti/commit/91890657119b4cde3cc3185bfd788990a7a1e2ca))
* route Claude catalog oracles through common runtime ([ff5a5f4](https://github.com/vibecook-dev/spaghetti/commit/ff5a5f44f0a34f799b77bb13fe72dc17ce440c50))
* route Codex catalog oracle through common runtime ([53f43a3](https://github.com/vibecook-dev/spaghetti/commit/53f43a3a01cc6c149374954b2794c03be9cb13c1))
* route cold RFC 012B source failures ([c12fd74](https://github.com/vibecook-dev/spaghetti/commit/c12fd74b163222ba4f840468b4989bfcc8d11da8))
* route Grok catalog oracle through common runtime ([9a279e8](https://github.com/vibecook-dev/spaghetti/commit/9a279e8f17b2beef6b38b3c523853e95cf234cf9))
* **sdk:** a search that outruns its index is pending, not an internal failure ([d74bd41](https://github.com/vibecook-dev/spaghetti/commit/d74bd4126189d42ca3fc4d24fdcd13f32141b7ca))
* **sdk:** default the observation host back to cold-build query bootstrap ([ed40a3b](https://github.com/vibecook-dev/spaghetti/commit/ed40a3b0b4d9ce015ace0895861066d2c9b52c34))
* **sdk:** do not fail a suite because a temp directory outlived its handle ([4228949](https://github.com/vibecook-dev/spaghetti/commit/42289498b282f299f197d87e449341383ad2941f))
* **sdk:** make an empty extending interface an alias so `pnpm lint` passes ([a82f598](https://github.com/vibecook-dev/spaghetti/commit/a82f598a2a06cb6a5032d1cb5b5a4714b4a4f4a6))
* **sdk:** re-apply the empty-interface lint fix lost in the L1/L2 merge ([f9a1b02](https://github.com/vibecook-dev/spaghetti/commit/f9a1b02c75162fb8b7ff11b20016a39b809e7976))
* share RFC 012C message log reduction ([410d99a](https://github.com/vibecook-dev/spaghetti/commit/410d99ac903677b976a384bf59aefd6026a10f44))
* share RFC 012C plan reduction ([c1a7fa4](https://github.com/vibecook-dev/spaghetti/commit/c1a7fa4ee89a7235994e3465cd00c832abe1d3b1))
* share RFC 012C task reduction ([64de86e](https://github.com/vibecook-dev/spaghetti/commit/64de86ecc61f8a947f497f315dded7e9dfa3a42c))
* share RFC 012C tool reduction ([68a3660](https://github.com/vibecook-dev/spaghetti/commit/68a3660612818846a2e2aa67a3b92b605ba451e4))
* share RFC 012C user-input lifecycle reduction ([32cc6da](https://github.com/vibecook-dev/spaghetti/commit/32cc6da1134aadb78e81fc516f7d21e4b60c19e8))
* surface what failed a configured observation startup ([50c75ac](https://github.com/vibecook-dev/spaghetti/commit/50c75ace5dd79f63b44639e4cabdda60df87ea6d))
* treat unknown RFC 012C evidence as incomplete ([ec87514](https://github.com/vibecook-dev/spaghetti/commit/ec875146222b097c829e109e6f4cadb8e7d79c73))
* validate sanitized unknown evidence ([2beac04](https://github.com/vibecook-dev/spaghetti/commit/2beac04936d735508336232db7807b486d122202))
* version RFC 012D unknown-evidence watermarks ([a7059d2](https://github.com/vibecook-dev/spaghetti/commit/a7059d231b9dc884a92971afc9564a2c777b4dbd))
* **windows:** normalize catalog paths ([9eb6c36](https://github.com/vibecook-dev/spaghetti/commit/9eb6c36bde7fed9129eac4b55d3034eebcfa9a81))


### Performance Improvements

* **catalog:** decide hydration by seek, not by counting every message ([d422939](https://github.com/vibecook-dev/spaghetti/commit/d422939c0307e3a9f56478336067e01ade4dd130))
* **claude:** stop parsing every transcript record four times ([00d2854](https://github.com/vibecook-dev/spaghetti/commit/00d285487e9aa3029d215e54648e2cf4dd8f2984))
* **engine:** evaluate the usage scope bridge once per query ([3239024](https://github.com/vibecook-dev/spaghetti/commit/323902429c31b36bd3958e51cc482f028c796c37))
* **engine:** reader-safe bootstrap checkpoints, synchronous=OFF cold builds, self-healing validation failures ([c4d4090](https://github.com/vibecook-dev/spaghetti/commit/c4d4090436615c863e273bdd2c54e1a7b6e7408a))
* **observer:** reconcile what changed, and stop cutting bootstrap short ([7195551](https://github.com/vibecook-dev/spaghetti/commit/71955513939c0fef25766440c754164e6bb0312b))
* **observer:** record confirmed absence, so a missing sidecar costs one stat ([b4cfb18](https://github.com/vibecook-dev/spaghetti/commit/b4cfb18687908a2c8bfa513269e983405beee1a1))
* **observer:** settle the transport on a measured comparison ([637ef80](https://github.com/vibecook-dev/spaghetti/commit/637ef80ae547f520b31fd0f073d337984e9c6df0))


### Code Refactoring

* **sdk:** delete the consumer-less RFC 012D validators and observer shims ([86ad9e5](https://github.com/vibecook-dev/spaghetti/commit/86ad9e5b6a7d1f438d9e78f70b3f77a8e4c45d11))

## [0.7.0](https://github.com/vibecook-dev/spaghetti/compare/spaghetti-v0.6.2...spaghetti-v0.7.0) (2026-08-11)


### ⚠ BREAKING CHANGES

* **sdk:** requires Node >=22.13.0, the first release where node:sqlite needs no flag. Node 18 and 20 are both out of support.

### Features

* **sdk:** run the index on node:sqlite and delete the last install script ([#120](https://github.com/vibecook-dev/spaghetti/issues/120)) ([9dfd57d](https://github.com/vibecook-dev/spaghetti/commit/9dfd57de4cda3cd73597c4765e6ec400e41796aa))


### Bug Fixes

* **cli:** correct the npm 12 install guidance, which 0.6.2 got wrong ([#117](https://github.com/vibecook-dev/spaghetti/issues/117)) ([1d42f62](https://github.com/vibecook-dev/spaghetti/commit/1d42f629d551f54d15431e2f37beea8c122022e4))

## [0.6.2](https://github.com/vibecook-dev/spaghetti/compare/spaghetti-v0.6.1...spaghetti-v0.6.2) (2026-08-11)


### Bug Fixes

* **napi:** close every non-accepted RFC 008 divergence found on macOS ([#115](https://github.com/vibecook-dev/spaghetti/issues/115)) ([456bf28](https://github.com/vibecook-dev/spaghetti/commit/456bf2816e9b2a96ffda2773659858a63c42b46a))
* **napi:** report a typed-parse failure that costs indexed text ([#116](https://github.com/vibecook-dev/spaghetti/issues/116)) ([4df448c](https://github.com/vibecook-dev/spaghetti/commit/4df448c80b45836ee57140a4024b4bc3a3c38283))
* **napi:** strip the carriage return from CRLF plan titles ([#111](https://github.com/vibecook-dev/spaghetti/issues/111)) ([cd5efda](https://github.com/vibecook-dev/spaghetti/commit/cd5efda08ed533175fb11f31534f618324b6984c))

## [0.6.1](https://github.com/vibecook-dev/spaghetti/compare/spaghetti-v0.6.0...spaghetti-v0.6.1) (2026-08-10)


### Bug Fixes

* **napi:** publish the musl platform packages ([#109](https://github.com/vibecook-dev/spaghetti/issues/109)) ([5ead091](https://github.com/vibecook-dev/spaghetti/commit/5ead091980c21a133441facf62960f21b5d0788d))

## [0.6.0](https://github.com/vibecook-dev/spaghetti/compare/spaghetti-v0.5.23...spaghetti-v0.6.0) (2026-08-10)


### ⚠ BREAKING CHANGES

* api.runtime, createRuntimeBridge, RuntimeEvent, the hook/channel watchers and wire types, and the hookEventsFile / channelSessionsDir / channelMessagesDir source paths are removed with no replacement. api.runtime.listActiveSessions() is replaced by listActiveSessionsFromDir(paths.sessionsDir). spag hooks, spag chat, and spag plugin are removed. Transcript ingest, search, query, and live updates are unaffected.

### Features

* **fixtures:** add Codex corpus and record bench hardware ([#97](https://github.com/vibecook-dev/spaghetti/issues/97)) ([20ec07c](https://github.com/vibecook-dev/spaghetti/commit/20ec07cd9ab63290d3eeeae62618ea4b15780212))
* **ingest:** complete the source clear and force one upgrade repair ([#101](https://github.com/vibecook-dev/spaghetti/issues/101)) ([09aca56](https://github.com/vibecook-dev/spaghetti/commit/09aca564146e6ca1eb1cea157014a93b92a46e93))
* **ingest:** fingerprint every consumed input, treat absent roots as deletion ([#102](https://github.com/vibecook-dev/spaghetti/issues/102)) ([29851ab](https://github.com/vibecook-dev/spaghetti/commit/29851abbf5be534d0c0b9e93f8d8c9aa2ed7d3ca))
* **napi:** Codex token estimation decision and port (RFC 008 Phase 3) ([#105](https://github.com/vibecook-dev/spaghetti/issues/105)) ([812dedc](https://github.com/vibecook-dev/spaghetti/commit/812dedca8a8bda5ce19f868e97b7e0bec7af7429))
* **napi:** transaction and error protocol (RFC 008 Phase 2) ([#104](https://github.com/vibecook-dev/spaghetti/issues/104)) ([c8971c2](https://github.com/vibecook-dev/spaghetti/commit/c8971c2987700a46f93a82dc68d8091089ba9761))
* retire the runtime bridge, hooks, chat, and plugins ([#93](https://github.com/vibecook-dev/spaghetti/issues/93)) ([b0cfa8d](https://github.com/vibecook-dev/spaghetti/commit/b0cfa8d2703e9e7e1471abf304b615136f835503))
* **rfc-008:** freeze the Phase 0 contracts and capture the baseline ([#99](https://github.com/vibecook-dev/spaghetti/issues/99)) ([996d79d](https://github.com/vibecook-dev/spaghetti/commit/996d79d694c3aa4bb6546ab0bcfb57405814a05b))
* show a project's git worktrees, and which agent is in each ([#90](https://github.com/vibecook-dev/spaghetti/issues/90)) ([b0d74d3](https://github.com/vibecook-dev/spaghetti/commit/b0d74d3592688e322ccbaf94fd28ef5e205eae8d))
* warm strategy decision, musl artifacts, loud fallback (RFC 008 Phase 4) ([#106](https://github.com/vibecook-dev/spaghetti/issues/106)) ([81e848e](https://github.com/vibecook-dev/spaghetti/commit/81e848e596628f352f35fd07d95b41bf192872d1))


### Bug Fixes

* **ingest:** key fingerprints by (source_id, path) ([#100](https://github.com/vibecook-dev/spaghetti/issues/100)) ([51c400a](https://github.com/vibecook-dev/spaghetti/commit/51c400a91661d9ac98ad89c74d5eb79c1eb2430c))
* **napi:** compute mtimeMs the way Node does ([#108](https://github.com/vibecook-dev/spaghetti/issues/108)) ([776bf73](https://github.com/vibecook-dev/spaghetti/commit/776bf73514d4b9ce27cffc6e8e5a14771d4cb5ee))
* **napi:** converge the warm ingest on the cold result (RFC 008 Phase 1 gate) ([#103](https://github.com/vibecook-dev/spaghetti/issues/103)) ([ec19799](https://github.com/vibecook-dev/spaghetti/commit/ec197990d59517377017442391c81dde3052097b))
* **napi:** subagent spawn linkage + RFC 008 readiness report (Phase 5) ([#107](https://github.com/vibecook-dev/spaghetti/issues/107)) ([be78471](https://github.com/vibecook-dev/spaghetti/commit/be78471c1fefc9c001ee4a35af2ecd9136fd2d76))
* **types:** cover Claude Code fields the validators flagged ([#95](https://github.com/vibecook-dev/spaghetti/issues/95)) ([1b29935](https://github.com/vibecook-dev/spaghetti/commit/1b29935ac6ea6da4969e8cdd216f02c7b390c3bd))

## [0.5.23](https://github.com/vibecook-dev/spaghetti/compare/spaghetti-v0.5.22...spaghetti-v0.5.23) (2026-07-30)


### Bug Fixes

* **ci:** ship the napi loader as a build artifact ([#86](https://github.com/vibecook-dev/spaghetti/issues/86)) ([8c5e175](https://github.com/vibecook-dev/spaghetti/commit/8c5e175c3a42652c04c466c978429a9837027baa))

## [0.5.22](https://github.com/vibecook-dev/spaghetti/compare/spaghetti-v0.5.21...spaghetti-v0.5.22) (2026-07-28)


### Features

* **napi:** build a native binary for Windows on ARM ([#82](https://github.com/vibecook-dev/spaghetti/issues/82)) ([0e9a741](https://github.com/vibecook-dev/spaghetti/commit/0e9a7411b2059b1090a6f0a12c1d8740498f721f))


### Bug Fixes

* **ci:** stop generated files from failing every release PR ([#85](https://github.com/vibecook-dev/spaghetti/issues/85)) ([02452b0](https://github.com/vibecook-dev/spaghetti/commit/02452b0e4871216dbda8aa0c96346daedfe204a9))
* make Windows a first-class platform ([#79](https://github.com/vibecook-dev/spaghetti/issues/79)) ([a6c88cf](https://github.com/vibecook-dev/spaghetti/commit/a6c88cf95c9a78c0eb857f906623137644850deb))
* **sdk:** ship the parse worker so parallel cold start actually runs ([#81](https://github.com/vibecook-dev/spaghetti/issues/81)) ([5e60e75](https://github.com/vibecook-dev/spaghetti/commit/5e60e75febd8856376d72ee85e8adaad9989904f))

## [0.5.21](https://github.com/jamesyong-42/spaghetti/compare/spaghetti-v0.5.20...spaghetti-v0.5.21) (2026-07-25)


### Features

* add database-backed message filtering ([3cde1c5](https://github.com/jamesyong-42/spaghetti/commit/3cde1c5d142511412db30db3ba0e8633841be3c5))
* add project token activity heatmap ([d6a935d](https://github.com/jamesyong-42/spaghetti/commit/d6a935d2ab2076dbddf1fd10659a28bd9256b0f6))
* aggregate projects and index subagent transcripts ([bdf1083](https://github.com/jamesyong-42/spaghetti/commit/bdf10836030147db5d3306a53c3eb63e749102b8))
* ingest rich Codex transcript records ([6436e88](https://github.com/jamesyong-42/spaghetti/commit/6436e88140db0a77e74f0c523ea05799b9463a7b))
* ingest rich Grok transcripts ([80257a5](https://github.com/jamesyong-42/spaghetti/commit/80257a5db16ccfd255ebb3aa824c51543d849bb6))
* move live session indexing to utility process ([1327e7f](https://github.com/jamesyong-42/spaghetti/commit/1327e7f541c19959c90223699dc4a89894f7b5eb))
* **playground,sdk:** multi-source UI, timeline chat, and SQLite recovery ([97d5773](https://github.com/jamesyong-42/spaghetti/commit/97d57738d0b12b6a7772fd95fc5e1d9e41c28d33))
* **playground:** adapt shell to archive paper design ([53fa7f0](https://github.com/jamesyong-42/spaghetti/commit/53fa7f027598c6ff9f93d77e20677c73f63fb3a2))
* **playground:** add archive settings panel ([01fcd19](https://github.com/jamesyong-42/spaghetti/commit/01fcd19364c2fdb1684c03b8850a433ae8b8bd92))
* **playground:** add debug galleries and file viewer ([4c47651](https://github.com/jamesyong-42/spaghetti/commit/4c47651ad390626ca27ef9460601698d4ad0000e))
* **playground:** archive chrome, mille Structure panel, and design typography ([f9da1d2](https://github.com/jamesyong-42/spaghetti/commit/f9da1d2a4b8729deb14873261fa7aa3b5c2c7740))
* **playground:** embed mille file explorer as right Files panel ([adb0a7e](https://github.com/jamesyong-42/spaghetti/commit/adb0a7ede083447cce30aba4b50cfd878009aae6))
* **playground:** search, live chat, artifacts, filters, and shell polish ([e02c397](https://github.com/jamesyong-42/spaghetti/commit/e02c397a996e7be32b6f3bc32e3fe06967064ec5))
* refine live session indicators ([054a289](https://github.com/jamesyong-42/spaghetti/commit/054a289a207946a3d9cd37f084bd7c85c41466c9))
* **sdk,napi:** materialize Claude session titles ([9e9a1d8](https://github.com/jamesyong-42/spaghetti/commit/9e9a1d81c26a4f00eaaa7706077e1f015d5dc43d))


### Bug Fixes

* **ci:** pin pnpm via packageManager instead of a stale major ([de2fa92](https://github.com/jamesyong-42/spaghetti/commit/de2fa92bcee27dfa9dc49ead5caf922bad7bb1a1))
* **codex:** force re-ingest when first_prompt extract rules change ([23e05c7](https://github.com/jamesyong-42/spaghetti/commit/23e05c7aa845c4ba6abe4ef9b4405ac49a911461))
* **codex:** use real human prompt for session list titles ([d60ecd4](https://github.com/jamesyong-42/spaghetti/commit/d60ecd440693c10da8601c2ae8356f5824bc6445))
* **napi:** apportion Grok session tokens like the TS writer ([bec4a83](https://github.com/jamesyong-42/spaghetti/commit/bec4a833f4490bc94d01d38bfdc0c7fe85048f41))
* normalize Codex sessions and transcript identity ([aded0e8](https://github.com/jamesyong-42/spaghetti/commit/aded0e8332633331ff58b076de475308a5844da2))
* **playground:** align archive colors with spaghetti-ui-design ([958f98d](https://github.com/jamesyong-42/spaghetti/commit/958f98d9e85189ce5a1693d01ac684d5b40cfa76))
* **playground:** align archive design details ([1b38b6b](https://github.com/jamesyong-42/spaghetti/commit/1b38b6b912a2bf4061453b086fece275d8eed886))
* **playground:** always show boot screen ([6c6e70a](https://github.com/jamesyong-42/spaghetti/commit/6c6e70a4203e71f2a2e26df86e5f19be59ed07a8))
* **playground:** depend on published mille instead of a sibling checkout ([a231ce0](https://github.com/jamesyong-42/spaghetti/commit/a231ce04da03aeabdb45cacdb322722cbfffda55))
* **playground:** show all message tool filters ([14120b3](https://github.com/jamesyong-42/spaghetti/commit/14120b31dfb23f45180d4e821da94dcc60fad217))
* **sdk:** name the path when the native watcher cannot bind ([130c6c0](https://github.com/jamesyong-42/spaghetti/commit/130c6c01d618aca26e9403b3031e3b78a412ef4a))
* **sdk:** render Codex/Grok messages in playground session view ([cb08ae9](https://github.com/jamesyong-42/spaghetti/commit/cb08ae9ded58656fc74871ae297ae698fa875fe0))
* **sdk:** spell watcher events under the caller-supplied root ([2a082ce](https://github.com/jamesyong-42/spaghetti/commit/2a082ce0707e3531ac65b681a854b535acd5b969))
* **sdk:** stop stamping wall-clock into sessions.modified_at ([17491fe](https://github.com/jamesyong-42/spaghetti/commit/17491fee35fe4ef3151667ad91ae9603f22c981c))

## [0.5.20](https://github.com/jamesyong-42/spaghetti/compare/spaghetti-v0.5.19...spaghetti-v0.5.20) (2026-07-16)


### Bug Fixes

* **ci:** darwin-x64 smoke test via Rosetta; deflake watcher tests ([16ddb32](https://github.com/jamesyong-42/spaghetti/commit/16ddb322cf47a64df2770caefabc8fdb6bd7ce09))

## [0.5.19](https://github.com/jamesyong-42/spaghetti/compare/spaghetti-v0.5.18...spaghetti-v0.5.19) (2026-07-16)


### Bug Fixes

* **ci:** gate release on native publish; run SDK tests; full smoke matrix ([ded5927](https://github.com/jamesyong-42/spaghetti/commit/ded5927789c3c2dc0f2c47abc24d592a7a7837e6))
* **ci:** scope SDK test gate to ubuntu/macos pending Windows compat ([0e4de1b](https://github.com/jamesyong-42/spaghetti/commit/0e4de1bf67124005cbdd2a7f29f37d55e47124fa))
* **cli:** error boundary, arg guards, TUI robustness ([728f19f](https://github.com/jamesyong-42/spaghetti/commit/728f19fb8d35918cc5be52c692faac16626bed1b))
* **napi:** serde tolerance, fingerprint robustness, TS parity ([af4aa94](https://github.com/jamesyong-42/spaghetti/commit/af4aa949d1e3cf3fbc9c61604be9d54f2a6879af))
* **sdk,napi:** multi-source ingest correctness after Grok review ([322fd3a](https://github.com/jamesyong-42/spaghetti/commit/322fd3aefe7dbd8125ea8ecf7ef2330e715975b2))
* **sdk:** consume only terminated JSONL lines in live tailers ([728482b](https://github.com/jamesyong-42/spaghetti/commit/728482bfdc22e8a3ea0987e83e2205312e809559))
* **sdk:** lifecycle ordering, runtime scoping, react cache identity ([6bbd0bb](https://github.com/jamesyong-42/spaghetti/commit/6bbd0bb6f8cf909ce26fe3167568304254bd4fab))
* **sdk:** move live checkpoint state out of the watched rootDir ([aaf4306](https://github.com/jamesyong-42/spaghetti/commit/aaf4306c2b3c4ffb9933a3424cc7f274e2dd0164))
* **sdk:** rewrite worker-pool crash recovery ([2684902](https://github.com/jamesyong-42/spaghetti/commit/268490222385a6da5ed132ba4e9d3da481acfb65))
* **sdk:** second round of Windows CI fixes ([b333cd6](https://github.com/jamesyong-42/spaghetti/commit/b333cd6fdfb2a1878d031a39fa84ec7ff6f09169))
* **sdk:** Windows compatibility for tests and path handling ([#73](https://github.com/jamesyong-42/spaghetti/issues/73)) ([18ea883](https://github.com/jamesyong-42/spaghetti/commit/18ea883a7105b60098401f17b680cb63f3a6c211))

## [0.5.18](https://github.com/jamesyong-42/spaghetti/compare/spaghetti-v0.5.17...spaghetti-v0.5.18) (2026-07-15)


### Features

* **cli,coverage:** multi-agent TUI for Grok + coverage claim + CI ([d1f6469](https://github.com/jamesyong-42/spaghetti/commit/d1f64694bdeaf795ccbfb2285b61af4f3d9dbc77))
* **cli,sdk:** multi-agent CLI + TUI — Claude & Codex side by side ([#71](https://github.com/jamesyong-42/spaghetti/issues/71)) ([90c75e3](https://github.com/jamesyong-42/spaghetti/commit/90c75e315f67305fc9893cfc6a6f9d1112ab4666))
* **cli:** render Grok sessions — display adapter + agent labels (M6 A6b) ([0e8e70a](https://github.com/jamesyong-42/spaghetti/commit/0e8e70a39d742c34475463eb43c2721ec438ea57))
* **coverage:** interactive HTML report for claims + ground truth ([31e4abb](https://github.com/jamesyong-42/spaghetti/commit/31e4abbdef9c5e4df2ca0e14db0397a2969143f2))
* **coverage:** multi-agent ground-truth scan + claim validation harness ([7fe0a8a](https://github.com/jamesyong-42/spaghetti/commit/7fe0a8aa6993a8cffc4526e6fac4ae0ad7f3bb5f))
* **napi,sdk:** Grok native cold/warm ingest + ingest-diff fixture ([c683f96](https://github.com/jamesyong-42/spaghetti/commit/c683f96c9f807d5231492855bf340e3798fde233))
* **napi,sdk:** native Codex cold/warm ingest (source_id=codex) ([f69e8e3](https://github.com/jamesyong-42/spaghetti/commit/f69e8e32e25ceb0396a363bb321594a669641cab))
* **sdk,cli:** Grok (xAI) AgentSource — third RFC 006 source (M6 A5) ([0ad419a](https://github.com/jamesyong-42/spaghetti/commit/0ad419a36e8d920202581d35bad6a4ed4e52937e))
* **sdk,napi:** Grok native live batch + events/signals sidecars ([0ab287c](https://github.com/jamesyong-42/spaghetti/commit/0ab287c6d7a957b66b136a2601065d04d70b6608))
* **sdk:** Grok live-watch — Plane 2 incremental tail (M6 A6a) ([3710a43](https://github.com/jamesyong-42/spaghetti/commit/3710a43f2a5a7b3e6d8bb7881dd6d774beabedc2))
* **sdk:** multi-source data plane + Codex live-watch (Codex source, composite PK, per-source LifecycleOwners, Plane 2) ([#70](https://github.com/jamesyong-42/spaghetti/issues/70)) ([aa2f024](https://github.com/jamesyong-42/spaghetti/commit/aa2f02462ed63a501b2713c101f097b75695365c))
* **sdk:** relocate Claude message extraction into a source-owned MessageExtractor (RFC 006 1-3) ([#65](https://github.com/jamesyong-42/spaghetti/issues/65)) ([7a7eefb](https://github.com/jamesyong-42/spaghetti/commit/7a7eefba54fd714fb953a42304f03d92e0b5094f))


### Bug Fixes

* **ci:** clippy + eslint for Grok native and classify re-export ([f74d097](https://github.com/jamesyong-42/spaghetti/commit/f74d0971363185f5505a882a9ea92e225eef80ad))
* **ci:** rustfmt + normalise mtime float noise in grok ingest-diff ([f54a2c4](https://github.com/jamesyong-42/spaghetti/commit/f54a2c432bbb72ff6a59bc5c5178ce11d10bdfef))
* **coverage:** unified hero stats across agents ([e8ea2e3](https://github.com/jamesyong-42/spaghetti/commit/e8ea2e3d96e3c5b87bc76c3bd1d10494f5edae6e))
* **sdk,cli:** prevent multi-source native from corrupting shared SQLite ([d95bb44](https://github.com/jamesyong-42/spaghetti/commit/d95bb4485d51b75d6c1a3e8897d61f4d7db88384))
* **sdk,napi:** turn-scoped Grok timestamp join ([107a674](https://github.com/jamesyong-42/spaghetti/commit/107a674c7a688b496c02f332319adaa1fbc14ec2))
* **sdk:** IdleMaintenance FTS merge targets search_fts ([3dccffa](https://github.com/jamesyong-42/spaghetti/commit/3dccffadc25d67b89a997b44d129dff857cb4db9))

## [0.5.17](https://github.com/jamesyong-42/spaghetti/compare/spaghetti-v0.5.16...spaghetti-v0.5.17) (2026-07-13)


### Features

* **sdk:** multi-agent groundwork — source dimension, source-owned classifier, normalized-model RFC ([#62](https://github.com/jamesyong-42/spaghetti/issues/62)) ([3a99ca5](https://github.com/jamesyong-42/spaghetti/commit/3a99ca5199fadae88e3f0339858ea882008d17dd))

## [0.5.16](https://github.com/jamesyong-42/spaghetti/compare/spaghetti-v0.5.15...spaghetti-v0.5.16) (2026-07-13)


### Features

* **cli:** show active RS/TS ingest engine indicator ([#58](https://github.com/jamesyong-42/spaghetti/issues/58)) ([f21b161](https://github.com/jamesyong-42/spaghetti/commit/f21b161dcf813266a52f6eec588b293ef8e2f2c5))
* **sdk,cli:** three-plane architecture, live TUI, and api.runtime ([31b7f3d](https://github.com/jamesyong-42/spaghetti/commit/31b7f3dc02739095039f3c48ea9ec8f37558f020))
* **sdk:** watchSessionTranscript — scoped single-session transcript tail ([#61](https://github.com/jamesyong-42/spaghetti/issues/61)) ([03ffe98](https://github.com/jamesyong-42/spaghetti/commit/03ffe9851d7bece6e496a1bec11dfd562a790a9f))


### Bug Fixes

* **sdk:** remove unused mkdirSync import in active-sessions test ([32462d6](https://github.com/jamesyong-42/spaghetti/commit/32462d695c3db06ef23e9c2d855b181795eef3da))

## [0.5.15](https://github.com/jamesyong-42/spaghetti/compare/spaghetti-v0.5.14...spaghetti-v0.5.15) (2026-07-03)


### Features

* **sdk,napi:** read agent-{id}.meta.json sidecar; use its agentType (both engines) ([#56](https://github.com/jamesyong-42/spaghetti/issues/56)) ([78d6d56](https://github.com/jamesyong-42/spaghetti/commit/78d6d560dce687314f700c535ce61a0dd0515c0a))
* **sdk:** live-watch nested workflow subagent transcripts (grouped under run) ([#57](https://github.com/jamesyong-42/spaghetti/issues/57)) ([08e1762](https://github.com/jamesyong-42/spaghetti/commit/08e1762ac6a5c56def3ef7c0a274267f5c4adf79))
* **sdk:** telemetry type refresh + session-env script listing (2026-07 audit LOW) ([#55](https://github.com/jamesyong-42/spaghetti/issues/55)) ([0f4d3c2](https://github.com/jamesyong-42/spaghetti/commit/0f4d3c252874d017e799b13a4ea33bf5d328fc10))
* **sdk:** wire mcp-needs-auth-cache + plugins/blocklist readers (2026-07 audit LOW) ([#53](https://github.com/jamesyong-42/spaghetti/issues/53)) ([cf12913](https://github.com/jamesyong-42/spaghetti/commit/cf129130bbdab68d44e3b247e0551a74a93e346a))

## [0.5.14](https://github.com/jamesyong-42/spaghetti/compare/spaghetti-v0.5.13...spaghetti-v0.5.14) (2026-07-03)


### Features

* **cli:** add Workflow tab to the session view ([#49](https://github.com/jamesyong-42/spaghetti/issues/49)) ([1242562](https://github.com/jamesyong-42/spaghetti/commit/12425621c7ef2c07051e23a577b24e4ac7c246b1))
* **sdk,napi:** ingest workflow runs + nested subagent transcripts (schema v4) ([#48](https://github.com/jamesyong-42/spaghetti/issues/48)) ([c2422e3](https://github.com/jamesyong-42/spaghetti/commit/c2422e311e7cad60e175b815b58e6685c11bfa20))
* **sdk,napi:** model new session message types + harden Rust against unknown types ([#46](https://github.com/jamesyong-42/spaghetti/issues/46)) ([b02795e](https://github.com/jamesyong-42/spaghetti/commit/b02795e22d5d48d276c2694413df842a1c6b672c))
* **sdk:** promote settings.local.json into config + refresh settings/plugin types ([#50](https://github.com/jamesyong-42/spaghetti/issues/50)) ([1610430](https://github.com/jamesyong-42/spaghetti/commit/16104309583c5a5dcd45205aab72b30af4dd9309))
* **sdk:** refresh ActiveSessionFile + HistoryPastedContent types (2026-07 audit) ([#51](https://github.com/jamesyong-42/spaghetti/issues/51)) ([f038051](https://github.com/jamesyong-42/spaghetti/commit/f03805122856f6e532394e440222ac76d25fcc1d))

## [0.5.13](https://github.com/jamesyong-42/spaghetti/compare/spaghetti-v0.5.12...spaghetti-v0.5.13) (2026-07-02)


### Features

* **napi:** emit plans in cold ingest + accept arbitrary toolUseResult shapes ([#44](https://github.com/jamesyong-42/spaghetti/issues/44)) ([3a50336](https://github.com/jamesyong-42/spaghetti/commit/3a503364c34af584deb87ed71587c8eefac32add))
* parse ~/.claude/teams/ + Team tab in the session TUI ([#41](https://github.com/jamesyong-42/spaghetti/issues/41)) ([dc87c34](https://github.com/jamesyong-42/spaghetti/commit/dc87c34135f172edc7506e6578ba66c1052e5f44))


### Bug Fixes

* **cli,sdk:** warm-start boot screen + orphaned-project recovery ([#39](https://github.com/jamesyong-42/spaghetti/issues/39)) ([fe531ad](https://github.com/jamesyong-42/spaghetti/commit/fe531ad72b73a95f5348e49a42c2e57763cc5070))
* **scripts:** repair the ingest-diff parity harness + record engine-flow audit ([#42](https://github.com/jamesyong-42/spaghetti/issues/42)) ([78ab81b](https://github.com/jamesyong-42/spaghetti/commit/78ab81b8f257b6df9f1d4c5c6985a63055624171))
* **sdk,cli:** warm-start msg_index corruption + fingerprint TOCTOU + stray-file projects + accent-bar crash ([#43](https://github.com/jamesyong-42/spaghetti/issues/43)) ([df08ef7](https://github.com/jamesyong-42/spaghetti/commit/df08ef766a69b8cf6b2294a46f11da9058ddb8c3))

## [0.5.12](https://github.com/jamesyong-42/spaghetti/compare/spaghetti-v0.5.11...spaghetti-v0.5.12) (2026-04-18)


### Features

* **playground:** read engine from app-scoped settings.json ([#32](https://github.com/jamesyong-42/spaghetti/issues/32)) ([4a81a14](https://github.com/jamesyong-42/spaghetti/commit/4a81a14208135c6de9cfd12d77ce41a8ebc9f0b3))

## [0.5.11](https://github.com/jamesyong-42/spaghetti/compare/spaghetti-v0.5.10...spaghetti-v0.5.11) (2026-04-18)


### Features

* **sdk:** add per-instance engine option to createSpaghettiService ([#31](https://github.com/jamesyong-42/spaghetti/issues/31)) ([9ffc941](https://github.com/jamesyong-42/spaghetti/commit/9ffc94138bb5f9dd6c4fbdd15d7ce00a47e52dbd))

## [0.5.10](https://github.com/jamesyong-42/spaghetti/compare/spaghetti-v0.5.9...spaghetti-v0.5.10) (2026-04-18)


### Features

* **playground:** sync with current SDK surface + use userData for DB ([#29](https://github.com/jamesyong-42/spaghetti/issues/29)) ([97228e7](https://github.com/jamesyong-42/spaghetti/commit/97228e721e89617f6612c3f3d1cee79c2ac079ec))

## [0.5.9](https://github.com/jamesyong-42/spaghetti/compare/spaghetti-v0.5.8...spaghetti-v0.5.9) (2026-04-18)


### Performance Improvements

* **napi:** drop FTS triggers during bulk ingest + tune writer PRAGMAs ([#26](https://github.com/jamesyong-42/spaghetti/issues/26)) ([6ba17b2](https://github.com/jamesyong-42/spaghetti/commit/6ba17b2b41d1b376d4d109a9b0212c5c06cd081a))

## [0.5.8](https://github.com/jamesyong-42/spaghetti/compare/spaghetti-v0.5.7...spaghetti-v0.5.8) (2026-04-17)


### Bug Fixes

* **napi:** populate created/modified/first_prompt on discovered sessions ([#22](https://github.com/jamesyong-42/spaghetti/issues/22)) ([c51892a](https://github.com/jamesyong-42/spaghetti/commit/c51892af3a316c9dbf72844b8ccfbd8757d00076))

## [0.5.7](https://github.com/jamesyong-42/spaghetti/compare/spaghetti-v0.5.6...spaghetti-v0.5.7) (2026-04-17)


### Features

* **sdk:** wire native ingest as default; add rebuildIndex() ([#20](https://github.com/jamesyong-42/spaghetti/issues/20)) ([c1df75a](https://github.com/jamesyong-42/spaghetti/commit/c1df75aa3a51d1331bfec1b33009b8569ce30b89))

## [0.5.6](https://github.com/jamesyong-42/spaghetti/compare/spaghetti-v0.5.5...spaghetti-v0.5.6) (2026-04-17)


### Features

* **napi:** RFC 003 Phase 3 — warm-start fast path ([#18](https://github.com/jamesyong-42/spaghetti/issues/18)) ([450d8c5](https://github.com/jamesyong-42/spaghetti/commit/450d8c58544d72f5d8b11e2af8bf841aa937bcdd))

## [0.5.5](https://github.com/jamesyong-42/spaghetti/compare/spaghetti-v0.5.4...spaghetti-v0.5.5) (2026-04-17)


### Performance Improvements

* **napi:** RFC 003 Phase 2 — rayon parallelism + bench harness ([#16](https://github.com/jamesyong-42/spaghetti/issues/16)) ([41e459d](https://github.com/jamesyong-42/spaghetti/commit/41e459db5037820b376d70c253dee121e0e3c322))

## [0.5.4](https://github.com/jamesyong-42/spaghetti/compare/spaghetti-v0.5.3...spaghetti-v0.5.4) (2026-04-17)


### Features

* **napi:** RFC 003 Phase 1 — Rust ingest core (cold-start parity) ([#14](https://github.com/jamesyong-42/spaghetti/issues/14)) ([925d364](https://github.com/jamesyong-42/spaghetti/commit/925d3644160f05ce47bb52b4b0b32263c8b2ab75))

## [0.5.3](https://github.com/jamesyong-42/spaghetti/compare/spaghetti-v0.5.2...spaghetti-v0.5.3) (2026-04-17)


### Bug Fixes

* **ci:** commit napi index stubs, handle prerelease publish tag ([#12](https://github.com/jamesyong-42/spaghetti/issues/12)) ([562b9f1](https://github.com/jamesyong-42/spaghetti/commit/562b9f13d685eca8206376d3fa8b5c998cc538e6))

## [0.5.2](https://github.com/jamesyong-42/spaghetti/compare/spaghetti-v0.5.1...spaghetti-v0.5.2) (2026-04-15)


### Performance Improvements

* optimize cold start and warm start data parsing ([#9](https://github.com/jamesyong-42/spaghetti/issues/9)) ([eeb2a69](https://github.com/jamesyong-42/spaghetti/commit/eeb2a69c821af8bd64b694d5c347544fec8fe970))

## [0.5.1](https://github.com/jamesyong-42/spaghetti/compare/spaghetti-v0.5.0...spaghetti-v0.5.1) (2026-04-13)


### Bug Fixes

* **ci:** use pnpm publish to rewrite workspace:* and ignore lock file ([be3b500](https://github.com/jamesyong-42/spaghetti/commit/be3b500fba185c7b00dc1ac3da6e89f7b6a62016))
* **packages:** add README to sdk and cli npm bundles ([e322f2d](https://github.com/jamesyong-42/spaghetti/commit/e322f2d3a96792245ee30dea27a879fb396b7f5d))

## [0.5.0](https://github.com/jamesyong-42/spaghetti/compare/spaghetti-v0.4.0...spaghetti-v0.5.0) (2026-04-13)


### ⚠ BREAKING CHANGES

* @vibecook/spaghetti-core and @vibecook/spaghetti-ui are replaced by a single package @vibecook/spaghetti-sdk with subpath exports:

### Features

* **apps:** add electron playground using @vibecook/spaghetti-sdk ([5dcfe26](https://github.com/jamesyong-42/spaghetti/commit/5dcfe26e1cd5adcdf99e902d6516d55a7baa1e33))
* **cli:** redesign message blocks in TUI messages view ([2e65c6e](https://github.com/jamesyong-42/spaghetti/commit/2e65c6e7a7a6aa7bced893c5abb62944cb6d73aa))
* **cli:** render assistant markdown in TUI detail view ([c7eaa49](https://github.com/jamesyong-42/spaghetti/commit/c7eaa4969be5c81face4ae060c3baa5db49f2b4a))
* merge spaghetti-core and spaghetti-ui into @vibecook/spaghetti-sdk ([fabf345](https://github.com/jamesyong-42/spaghetti/commit/fabf345acc7cd7d138aa1256092147fd1f50dad3))


### Bug Fixes

* **cli:** use cross-env for FORCE_COLOR=1 in tests ([17a1682](https://github.com/jamesyong-42/spaghetti/commit/17a16823b1dfe12869b6157a9f96de98a9e63383))


### Performance Improvements

* **cli:** switch TUI messages view to line-based scrolling ([be04066](https://github.com/jamesyong-42/spaghetti/commit/be04066ac15c669193ed4b57882c70afedb16c92))

## [0.4.0](https://github.com/jamesyong-42/spaghetti/compare/spaghetti-v0.3.3...spaghetti-v0.4.0) (2026-04-10)


### Features

* add channel plugin for interactive chat with Claude Code sessions ([0e9f877](https://github.com/jamesyong-42/spaghetti/commit/0e9f877))
* **cli:** add doctor command and TUI health-check view ([e1717c0](https://github.com/jamesyong-42/spaghetti/commit/e1717c0))
* **cli:** install and manage both hooks and channel plugins ([f07b386](https://github.com/jamesyong-42/spaghetti/commit/f07b386))


### Bug Fixes

* **channel:** address audit findings - race conditions and reliability ([3d87b64](https://github.com/jamesyong-42/spaghetti/commit/3d87b64))

## [0.3.3](https://github.com/jamesyong-42/spaghetti/compare/spaghetti-v0.3.2...spaghetti-v0.3.3) (2026-04-10)


### Bug Fixes

* **ci:** bump release workflow to Node 24 for npm OIDC auth ([263215d](https://github.com/jamesyong-42/spaghetti/commit/263215d589d47dbda5b92696c035cf3ad35673c6))

## [0.3.2](https://github.com/jamesyong-42/spaghetti/compare/spaghetti-v0.3.1...spaghetti-v0.3.2) (2026-04-10)


### Bug Fixes

* **ci:** remove broken npm self-upgrade step from release workflow ([0da596f](https://github.com/jamesyong-42/spaghetti/commit/0da596f91108df55717db686205d8a5b79b13aef))

## [0.3.1](https://github.com/jamesyong-42/spaghetti/compare/spaghetti-v0.3.0...spaghetti-v0.3.1) (2026-04-10)


### Features

* add @spaghetti/cli with 10 commands and full test suite ([20e8884](https://github.com/jamesyong-42/spaghetti/commit/20e8884fcddd0284b9bb5e511145e2090c26ce50))
* add CI/CD, auto-update, install script, and npm publishing ([7474740](https://github.com/jamesyong-42/spaghetti/commit/74747401bb8488cd4f1f2b0785190f7cd39c4cc4))
* add hooks inspector plugin and CLI hooks monitor ([fee32b5](https://github.com/jamesyong-42/spaghetti/commit/fee32b55a32cdb43732c4e719feaf63863d47832))
* add integration tests, benchmarks, validation suite, and UI fixes ([ee5a52e](https://github.com/jamesyong-42/spaghetti/commit/ee5a52eab177da2915e9c5131658019590619027))
* adopt truffle's CI/CD pattern — separate workflows ([5971cfd](https://github.com/jamesyong-42/spaghetti/commit/5971cfd81b115cb1d242c6ca03d237f501c8d0ff))
* **ci:** replace Changesets with Release-Please for automated releases ([d3ae982](https://github.com/jamesyong-42/spaghetti/commit/d3ae982e42abf42f6a3daadb72e7be426d3171ce))
* **cli:** add generic interactive list with viewport scrolling ([da87cb8](https://github.com/jamesyong-42/spaghetti/commit/da87cb8599f2e475cd43f260f29bca120a582a2b))
* **cli:** add hierarchical browser with 4-level navigation ([f35b41a](https://github.com/jamesyong-42/spaghetti/commit/f35b41a167fc02c3471c32afd676559276483abf))
* **cli:** add scrollbar track to messages view ([6ec6eb3](https://github.com/jamesyong-42/spaghetti/commit/6ec6eb36c0fd83b64a2c187ab883d266a92e5b67))
* **cli:** add session/message/detail semantic colors to theme ([4eb851d](https://github.com/jamesyong-42/spaghetti/commit/4eb851d8074ea136c571b93acb303967851f25aa))
* **cli:** add thin TUI layer with keypress parsing and screen control ([9332dc5](https://github.com/jamesyong-42/spaghetti/commit/9332dc50a011dee192b0e4d2feaceca3bdbf1561))
* **cli:** extract thinking blocks as distinct display items ([90a5bae](https://github.com/jamesyong-42/spaghetti/commit/90a5baef25c05b5cf6796616e177bf1126ef1f8b))
* **cli:** merge task-notification messages into Agent tool-call items ([6656641](https://github.com/jamesyong-42/spaghetti/commit/66566411733aeb7e2a3b322da6ceb201ca981c38))
* **cli:** merge tool_use + tool_result pairs, add tool-specific rendering ([600d5e3](https://github.com/jamesyong-42/spaghetti/commit/600d5e37cbb06150cc729d3e14616ce9beff66b7))
* **cli:** pill-style tab badges with breadcrumb integration ([18c0fe7](https://github.com/jamesyong-42/spaghetti/commit/18c0fe7cd780a88cb5d64e1056ebf4311df836c6))
* **cli:** replace slash commands with menu home, tabs, and search bar ([8e4987c](https://github.com/jamesyong-42/spaghetti/commit/8e4987cd3f889cf33c9d4e48f89373e093e020df))
* **cli:** reverse message order — latest messages at the top ([c42123a](https://github.com/jamesyong-42/spaghetti/commit/c42123ab09370aa7c711510b02deea4aff7e3304))
* **cli:** show 3 body lines for user messages, 2 for claude ([172cdeb](https://github.com/jamesyong-42/spaghetti/commit/172cdebc47a392bcb4a9d11e8c7268df9f2c4f56))
* **cli:** show all message types + interactive filter toggles ([cfcf10c](https://github.com/jamesyong-42/spaghetti/commit/cfcf10c609e6bc660e365f55278c715fa3e103d5))
* **cli:** show truncated session ID right-aligned on session cards ([10e28a7](https://github.com/jamesyong-42/spaghetti/commit/10e28a7c4c8344e9c0b744da27e23323f1398ecf))
* **cli:** TUI redesign — Ink view stack with slash commands ([3c58556](https://github.com/jamesyong-42/spaghetti/commit/3c585569742ee9d1a7a4544826d90a1c19948ceb))
* **cli:** wire interactive browser into spag p with TTY detection ([edc1c4e](https://github.com/jamesyong-42/spaghetti/commit/edc1c4e94b69b3a66b3f74f64ea8f52032dc8951))
* close all CI/CD gaps — lint, format, cross-platform, cleanup ([b96a886](https://github.com/jamesyong-42/spaghetti/commit/b96a886b6b83c246e5e4dad48f1b625099c76c52))
* complete @spaghetti/core with Architecture C cache redesign ([330a6ea](https://github.com/jamesyong-42/spaghetti/commit/330a6ea01ea8a820f83bcc1a349b40c8902ed574))
* publish to npm under [@vibecook](https://github.com/vibecook) scope ([eebb059](https://github.com/jamesyong-42/spaghetti/commit/eebb059d054f5bce2774d4724026d6d627484ad2))
* recover 40,308 messages from legacy databases ([cd9f351](https://github.com/jamesyong-42/spaghetti/commit/cd9f351a293cc554512de42cf832ad19a156f3a7))
* switch to changesets + OIDC trusted publishing (no NPM_TOKEN needed) ([b9fa9dd](https://github.com/jamesyong-42/spaghetti/commit/b9fa9dd98b368b5689be5b3cfefa2942d38a139f))
* truffle-style update system with spaghetti update command ([8a016bf](https://github.com/jamesyong-42/spaghetti/commit/8a016bf921269d12963890d1c092985aa98c6e45))


### Bug Fixes

* add pnpm version to all action-setup steps ([686c9d8](https://github.com/jamesyong-42/spaghetti/commit/686c9d8fef95f87cd4756533fcf14597de73d482))
* add Windows cross-platform compatibility ([3fe9e23](https://github.com/jamesyong-42/spaghetti/commit/3fe9e23671c34845ee8a6ffb01f8b1c9cad80311))
* address 3 CLI UX issues found by QA ([ae12632](https://github.com/jamesyong-42/spaghetti/commit/ae1263205ffe3e62e9b5eb2b04a6616e9ae18de5))
* CI failures — build before typecheck, fix implicit any types ([723cb34](https://github.com/jamesyong-42/spaghetti/commit/723cb344509999bedc0691a396fbe5ed2bf674f3))
* **ci:** use OIDC trusted publishing instead of NPM_TOKEN ([17313f0](https://github.com/jamesyong-42/spaghetti/commit/17313f05ff5f18e489b4f8820c9de3bea57877d4))
* **cli:** add signal handlers, empty states, and scroll indicator format ([b07f28c](https://github.com/jamesyong-42/spaghetti/commit/b07f28ca06685a854cc7ee3c9c3f9009d0e9e35b))
* **cli:** bg color covers full line, user timestamp right-aligned ([75b1675](https://github.com/jamesyong-42/spaghetti/commit/75b16755e813ba01e98b56c80bed785a653507bc))
* **cli:** collapse newlines in project card prompt text ([d56344e](https://github.com/jamesyong-42/spaghetti/commit/d56344e523f1fc66e7bbf4a0f246ab5ec9bc48b2))
* **cli:** collapse newlines in session card prompt text ([ae5f4e0](https://github.com/jamesyong-42/spaghetti/commit/ae5f4e0e77920303849d8f7d23bcb8b7242fa1b4))
* **cli:** consume all tool-result user messages, not just i+1 ([d7c3afc](https://github.com/jamesyong-42/spaghetti/commit/d7c3afcebf5f5e0a97b55cb9f5e7836a17ae98d9))
* **cli:** enable all message filters by default ([c8ee78a](https://github.com/jamesyong-42/spaghetti/commit/c8ee78acbd38de93af740fe0121cf4aa680574f0))
* **cli:** escape from empty states, remove setEncoding, simplify entry ([55a55c5](https://github.com/jamesyong-42/spaghetti/commit/55a55c5a8db72ca23ff2c6e6bbc2b31e62f78f33))
* **cli:** hide progress and internal messages by default ([a5cdacc](https://github.com/jamesyong-42/spaghetti/commit/a5cdacc5ec86dda9fbe92262018fe4c6bc398a0d))
* **cli:** load latest messages first, paginate backward for older ([44ab271](https://github.com/jamesyong-42/spaghetti/commit/44ab2719425b964c97278a3a6e3edf0d39f46697))
* **cli:** move message filter chips above the header rule ([74c9f8a](https://github.com/jamesyong-42/spaghetti/commit/74c9f8af17a0bc92ce27407e732eced7c1ae41f7))
* **cli:** project card text truncation and scroll viewport ([cf62708](https://github.com/jamesyong-42/spaghetti/commit/cf627087ad43de9ecbf0e511e5e35fd9f23783c4))
* **cli:** remove unused getDefaultHookEventsPath import ([4ab9143](https://github.com/jamesyong-42/spaghetti/commit/4ab914387745989e1c48e23241a9e089d4f3a8a5))
* **cli:** resolve lint errors — unused var, control regex, dead code ([95cd8c5](https://github.com/jamesyong-42/spaghetti/commit/95cd8c548513f92670792bb05e2a61e17deaf0fa))
* **cli:** restore HRule and fix ←→ tab switching in messages view ([5663d0d](https://github.com/jamesyong-42/spaghetti/commit/5663d0d2ba79dede6a8283167ff66cc50419e293))
* **cli:** stabilize messages view height to prevent footer jumping ([c4b5071](https://github.com/jamesyong-42/spaghetti/commit/c4b50713c22e454cf580d73c664621efc114c465))
* **cli:** stable scrollbar thumb size based on total message count ([f757cef](https://github.com/jamesyong-42/spaghetti/commit/f757cef76c05f09dbdba764e4c1603224edf6d49))
* **cli:** stop breadcrumb duplication from useEffect re-render loop ([f5a3edb](https://github.com/jamesyong-42/spaghetti/commit/f5a3edbae0fc916a3e8f451af9c0a4e9941d1a3f))
* **cli:** top-right corner gap in welcome panel and boot screen borders ([2213e8e](https://github.com/jamesyong-42/spaghetti/commit/2213e8ee72aa2dc8c6da80185ffad4b00e9bac75))
* **cli:** use official claude plugin CLI for install/uninstall ([3f41a3e](https://github.com/jamesyong-42/spaghetti/commit/3f41a3e3561b762ec2a02e26300253f29b864499))
* improve CLI init progress display and worker fallback ([adad59a](https://github.com/jamesyong-42/spaghetti/commit/adad59a359ed263f64fd39882a7e16dc256175ce))
* mark UI package as private, rename to [@vibecook](https://github.com/vibecook) scope ([3f6fca3](https://github.com/jamesyong-42/spaghetti/commit/3f6fca3b81d1ab90e68ca677c86ff665e1f9d7d0))
* **plugin:** remove explicit hooks reference from manifest ([59e4c27](https://github.com/jamesyong-42/spaghetti/commit/59e4c275a2cf8b87b020d9f2902a35d3fedebdc5))
* **plugin:** remove Stop/SessionStart from additionalContext output ([ceae7a5](https://github.com/jamesyong-42/spaghetti/commit/ceae7a5f3c0a4e42fc0a3cf79981698c9a6b76c7))
* progress display stays on one line, truncate long slug names ([bfc1a99](https://github.com/jamesyong-42/spaghetti/commit/bfc1a99c7eb0c6e0fece935d2ea66e3be16cbd10))
* remove nav from deps since setSubtitle is a stable setState. ([f5a3edb](https://github.com/jamesyong-42/spaghetti/commit/f5a3edbae0fc916a3e8f451af9c0a4e9941d1a3f))
* rename @spaghetti/core to @vibecook/spaghetti-core in UI package ([39b2557](https://github.com/jamesyong-42/spaghetti/commit/39b25575f227d24b94728c43e3767daeb944ec85))
* resolve all lint errors across cli, core, and ui packages ([fab0bc5](https://github.com/jamesyong-42/spaghetti/commit/fab0bc57f7118b21279bc0b1951a1704fd443c7c))
* resolve SQLITE_BUSY, worker deadlock, and error handling bugs ([23598a3](https://github.com/jamesyong-42/spaghetti/commit/23598a3ddbc268e405fd29e7edc8241479396b8c))
* resolve stale sessions-index causing 0 messages for 14 projects ([0ef23c5](https://github.com/jamesyong-42/spaghetti/commit/0ef23c5000b8fdca5496e9152b6cf2850d2edc23))


### Performance Improvements

* **cli:** update header/footer in-place instead of recreating list ([8dd3a4a](https://github.com/jamesyong-42/spaghetti/commit/8dd3a4a2595c805899175f3aed1eff24a4874592))

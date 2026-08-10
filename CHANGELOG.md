# Changelog

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

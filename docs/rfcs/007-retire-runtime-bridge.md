# RFC 007: Retire the Runtime Bridge

**Status:** Proposed (v2)
**Implementation readiness:** Phase 0 may start; Phases 1–4 remain blocked by their stated gates.
**Created:** 2026-08-06
**Revised:** 2026-08-07
**Split from:** `007-retire-runtime-bridge-and-ts-ingest.md` Draft v6
**Author:** James Yong + Kimi
**Companion RFCs:** [RFC 008 — Rust Bulk Ingest Production Readiness](./008-rust-ingest-production-readiness.md) · [RFC 009 — Retire the TypeScript Bulk Ingest Engine](./009-retire-typescript-bulk-ingest.md)

---

## Summary

Retire Plane 3: the runtime bridge, hook-event and channel-session streaming surfaces, their CLI/TUI commands, and the two Claude Code plugin packages.

This RFC does one thing only. It does not change cold/warm ingest, engine selection, SQLite files, the query path, or the live transcript writer. Those concerns moved to RFCs 008 and 009 so this removal can ship independently and be reviewed as a bounded product-surface change.

The retirement spans two published releases:

1. A deprecation release that detects installed plugins and marketplace registrations and provides a working guided uninstall.
2. A removal release that deletes the bridge and plugins while retaining a read-only doctor check for leftovers.

---

## Why this is its own RFC

The runtime bridge is operationally independent from transcript ingest. Combining their removal made unrelated decisions block each other and obscured the rollback boundary. Plane 3 has no SQLite migration and no dependency on Rust ingest correctness, so it should have its own release train.

The outcome is intentionally narrow:

- no runtime event facade in the SDK;
- no hook-event or channel client/manager code;
- no `spag hooks`, `spag chat`, or `spag plugin` command;
- no hooks-monitor or chat TUI view;
- no bundled `spaghetti-hooks` or `spaghetti-channel` Claude Code plugin;
- doctor can still identify and explain how to remove leftovers.

---

## Non-goals

1. Do not change either bulk ingest engine.
2. Do not remove engine selection, native fallback, or per-engine database files.
3. Do not change the TypeScript live writer or filesystem watchers.
4. Do not change the SQLite schema or migrate user data.
5. Do not redesign the CLI/TUI outside the entries removed here.
6. Do not mutate Claude plugin or marketplace state unless the user invokes `spag plugin uninstall` and either confirms its interactive plan or passes `--yes`.
7. Do not recursively discover or mutate arbitrary project/local plugin declarations. Automated cleanup is limited to the user scope historically created by `spag plugin install`; detected non-user state is diagnostic and manual.
8. Do not purge plugin persistent data or the existing `~/.spaghetti/hooks`/`~/.spaghetti/channel` files. Retirement disables/uninstalls code and registrations only.
9. Do not decide the future of the Rust engine. RFC 008 owns readiness; RFC 009 owns cutover.

---

## Decisions

| ID  | Decision                         | Choice                                                                                                                                                    |
| --- | -------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------- |
| R1  | Plugin fate                      | Delete both bundled plugins after one published deprecation window                                                                                        |
| R2  | Uninstall authorization          | Interactive invocation previews and prompts; affirmative confirmation or `--yes` executes. Non-interactive invocation without `--yes` never mutates state |
| R3  | Leftover detection after removal | Keep a read-only doctor probe; do not keep the plugin command                                                                                             |
| R4  | Historical documents             | Keep dated records and add supersession notes instead of rewriting history                                                                                |
| R5  | Automated cleanup scope          | Mutate user-scope Spaghetti registrations only; report project/local, source-mismatched, and unknown state for manual resolution                          |
| R6  | Post-removal recovery            | Preserve an immutable deprecation Git tag and reinstall plugins from a local checkout of that tag; pinning the npm package alone is insufficient          |
| R7  | Notice lifetime                  | Doctor always renders current leftover state; the deprecation-release TUI banner appears once per process, with no persisted acknowledgement              |
| R8  | Removal cutover                  | Merging plugin deletion to the canonical marketplace branch is the external removal event; do it only when the removal release is ready to publish        |
| R9  | Plugin persistent data           | Preserve it during guided/manual uninstall with Claude's `--keep-data` option; purging plugin-generated data is outside this RFC                          |

Changing one of these decisions requires editing this table before implementation.

---

## Normative retirement contracts

The following contracts are release behavior, not implementation suggestions.

### Leftover identity and scope

The retired identities are:

- `spaghetti-hooks@spaghetti`;
- `spaghetti-channel@spaghetti`;
- the user-scope marketplace named `spaghetti` whose normalized source is this repository (`vibecook-dev/spaghetti`).

`spag plugin install` historically invokes Claude without a scope flag and therefore creates user-scope state. The automated cleanup path owns only that user-scope state.

- Project/local installations or marketplace declarations are never changed automatically. If the probe can see them, it reports their scope and the owning project/context needed for a manual command. This RFC does not scan arbitrary project trees to find them.
- A marketplace named `spaghetti` is automatically owned only when its source is the structured GitHub repo `vibecook-dev/spaghetti` or an HTTPS/SSH Git URL that normalizes to that exact repository (ignoring a trailing `.git`). Directory sources, other owners/repos, and unsupported source shapes are source mismatches. They are reported but never removed automatically.
- A dangling user `enabledPlugins` entry counts as a leftover even when no installed-plugin record exists.
- “Clean” means that both retired plugin IDs are absent and disabled in every state visible to the probe, the expected user marketplace is absent, and no relevant input is `unknown`.

### Standalone leftover probe

Implement the retained probe in `packages/cli/src/lib/plugin-leftovers.ts`. Each input is represented as a tri-state result:

- `present`, with identity, scope, source, install path, and enabled status where available;
- `absent`, only when a missing or well-formed source proves the identity is not present;
- `unknown`, with the affected path/command and reason.

The aggregate report contains separate results for each plugin's installed and enabled state, non-user installations visible to the probe, and the user marketplace registration. It never collapses `unknown` or a source mismatch into absence.

Primary read-only inputs are:

- `<claudeHome>/plugins/installed_plugins.json`;
- `<claudeHome>/settings.json`;
- `<claudeHome>/plugins/known_marketplaces.json`.

For unsupported or unreadable installed-plugin or marketplace formats, the only permitted fallback is the corresponding structured Claude command (`claude plugin list --json` or `claude plugin marketplace list --json`). A fallback parser ships only with captured fixtures and schema tests. If the executable is unavailable, exits non-zero, or returns unsupported output, the affected result remains `unknown`. Human-formatted CLI output is never parsed.

The probe accepts an explicit Claude home/path set and an injected read-only command runner. Neither the module nor its tests capture `homedir()` in import-time constants. Unit tests use fake homes and fake command output; integration tests pass an isolated Claude configuration environment explicitly and must prove they did not read or write the developer's real `~/.claude`.

Automatic execution has a capability gate: the detected Claude CLI must support explicit `--scope user` for plugin disable, plugin uninstall, and marketplace removal, plus `--keep-data` for plugin uninstall. Claude Code 2.1.223 is the known-good RFC-review fixture, not an asserted minimum. Phase 0 records the verified minimum version/capability fixture. An older or incompatible CLI is treated as an unavailable executor for mutation; never drop `--scope user` or `--keep-data` as a compatibility fallback.

### Guided uninstall state machine

`spag plugin uninstall [plugin] [--yes]` first probes state and builds a complete ordered plan. It does not edit JSON files directly.

For a selected user-scope plugin, the plan may contain:

1. `claude plugin disable --scope user <plugin-id>` when the user settings entry is enabled;
2. `claude plugin uninstall --scope user --keep-data <plugin-id>` when a user installation is present.

The no-target command selects both plugins in hooks-then-channel order. After both are proven absent and disabled, it may append:

3. `claude plugin marketplace remove --scope user spaghetti`, but only when the marketplace source matches this repository and no visible plugin scope or relevant input is present or unknown.

A targeted command changes only the selected user-scope plugin and never removes the shared marketplace. Visible non-user copies of that plugin are reported but do not block a verified user-scope targeted cleanup. Running the no-target command is the explicit full-cleanup operation; for that operation, any visible non-user installation, source mismatch, or relevant `unknown` result blocks all mutation before execution.

Before mutation, print the planned operations and the exact raw commands. Authorization and exit status are:

An invocation is interactive only when both `process.stdin.isTTY` and the prompt/output stream's `isTTY` are true. Every other no-`--yes` invocation follows the non-interactive row.

| Situation                                                                   | Mutation                                       | Exit                     |
| --------------------------------------------------------------------------- | ---------------------------------------------- | ------------------------ |
| requested postcondition already satisfied                                   | none; report clean no-op                       | 0                        |
| interactive, no `--yes`, user confirms                                      | execute plan                                   | 0 on verified completion |
| interactive, no `--yes`, user declines                                      | none; report cancelled, never success          | 2                        |
| non-interactive without `--yes`                                             | none; print plan and authorization requirement | 2                        |
| `--yes` and all required state is known and automatable                     | execute plan                                   | 0 on verified completion |
| a required state is unknown or the no-target plan has a manual-only blocker | none; print diagnostics and manual commands    | 2                        |
| Claude executable is unavailable or lacks required scope capabilities       | none; print compatible manual guidance         | 1                        |
| an operation or postcondition check fails                                   | stop; report partial state and remaining work  | 1                        |

Execution is fail-fast. Re-run the probe after every command and after the final command. A command that exits zero but does not establish its postcondition is a failure. Never attempt marketplace removal after an earlier failure, a remaining plugin, a non-user installation, a source mismatch, or an `unknown` result. Never label an incomplete default cleanup as successful.

Manual output includes destructive commands only for identities and scopes proven to belong to Spaghetti. For a source mismatch or unknown identity, print inspection evidence and explain that the user must resolve it; do not recommend a marketplace-removal command as though ownership were proven.

`--yes` belongs to `spag` and is not forwarded to Claude. All subprocess arguments are passed as an argument vector through an injected executor, not interpolated into a shell string.

Evaluation precedence is deterministic: probe and requested-postcondition checks first, then plan preview/authorization, then executor capability, then execution/postcondition verification. Thus clean state returns 0 without needing Claude; unknown/manual state or missing authorization returns 2 without executor checks; an authorized known non-empty plan with an unavailable/unsupported executor returns 1.

### Deprecation and notice behavior

- Every Phase 3 SDK deletion is marked `@deprecated` in the published deprecation-release declarations, with the exact removal version. Hook/channel streaming and control APIs say “no replacement”; active-session callers are directed to the retained `listActiveSessionsFromDir`/`isProcessAlive` exports.
- `spag hooks`, `spag chat`, and `spag plugin install|status` print a removal warning on stderr on every invocation. JSON stdout remains machine-readable.
- `spag plugin uninstall` identifies itself as the migration path and names the last version that contains it.
- During the deprecation release, TUI boot displays one retirement banner per process even when plugin state is clean. It has no acknowledgement file or other persistent state. When leftovers or unknown state exist, the banner names them.
- Doctor always renders the Plane 3 leftover section. It reports `clean`, named leftovers, source mismatches, or `unknown` paths; it never offers install/enable actions.
- After Phase 3 removes the hooks/chat views, only the doctor leftover section remains. Keep it for at least one additional minor release.

### Immutable recovery source

The deprecation release tag is the recovery artifact. Before Phase 3 can merge:

1. The exact deprecation version and Git tag are recorded in the root, SDK, and CLI changelogs. The canonical release evidence records the tag's resolved commit SHA after the tag is created; the tagged commit is not required to contain its own hash.
2. The tag exists on the canonical remote and contains both plugin directories plus the root marketplace manifest.
3. From a clean checkout of that tag, an isolated Claude environment can register the checkout as a local user marketplace and install both plugins.
4. The tested rollback runbook is published. It states explicitly that the pinned npm CLI's `spag plugin install` is not a post-removal recovery mechanism because it resolves the repository's current default branch.

Phase 3 must not delete the default-branch plugin sources before this immutable recovery rehearsal passes.

The canonical default branch remains a functioning marketplace source throughout the deprecation window. Phase 3 deletion work may be developed and verified on a release branch, but merging that deletion is the removal cutover because existing and pinned deprecation CLIs resolve the repository's current default branch. The cutover merge occurs only when the next-minor removal artifacts and site deployment are ready to publish. If publication fails after the merge, immediately revert the deletion commit to restore the marketplace source before retrying.

---

## Current coupling that must be preserved or relocated

Three pieces that look like Plane 3 dependencies or adjacent cleanup are still used elsewhere:

- `planes/active-sessions.ts`, `isProcessAlive`, and `ActiveSessionFile` support index-live/doctor reporting. Relocate them to `sources/claude-code/active-sessions.ts`; do not delete them or their current SDK exports.
- The plugin/marketplace probe must survive removal of `lib/plugins.ts`. Its retained home is `packages/cli/src/lib/plugin-leftovers.ts`; doctor and TUI import it directly.
- The retained `spag uninstall` command currently removes only the npm CLI, says `~/.claude` is unaffected, and offers a broad optional `rm -rf ~/.spaghetti` that would also remove hook/channel history. Its instructions must use the read-only leftover probe, include safe state-specific Plane 3 cleanup before the CLI is removed, and make cache-only cleanup distinct from an explicit data purge without performing either itself.

These relocations are a gate, not cleanup to discover after deletion.

---

## Phase 0 — Inventory and safety harness

### Work

1. Record the production surface before changing it:
   - every SDK export, field, type, source-path declaration, and test that references runtime events, the runtime facade, channel clients, or hook watchers;
   - CLI command registrations, unknown-command suggestions, menu entries, doctor/TUI renderers, and the retained `spag uninstall` instructions;
   - both plugin package names, plugin IDs, the marketplace name/source, and the root marketplace manifest;
   - `ws` and `@types/ws` ownership in SDK/CLI manifests and the lockfile;
   - root, SDK, and CLI changelogs plus current README/site references and dated historical documents;
   - the current active-session and doctor call graph.
2. Implement `lib/plugin-leftovers.ts` and its tri-state report exactly as defined above.
3. Implement `packages/cli/src/lib/plugin-cleanup.ts` as the temporary Phase 1 mutation boundary. Its pure planner consumes a probe report, target selection, and authorization mode and returns ordered argument-vector operations plus required postconditions; its executor is injected so unit tests cannot invoke the real Claude executable. `plugin-leftovers.ts` must not import this module.
4. Unit-test the probe and planner against temporary fake homes and captured structured-CLI fixtures. Cover:
   - user installed, enabled-only, both, and neither;
   - marketplace-only, expected-source, and source-mismatched registrations;
   - multiple installation records and visible project/local scopes;
   - missing, malformed, unreadable, unsupported, and fallback-command failure states.
     Put tests under the current `packages/cli/src/__tests__/*.test.ts` discovery path or deliberately expand the CLI test script; the gate must show the named probe/planner tests executed, not merely a green command that skipped them.
5. Capture `claude --version` and help/capability fixtures, verify explicit user-scope support for every mutating command plus uninstall data preservation, and record the minimum supported cleanup version. Unsupported CLIs must take the non-mutating exit-1 path.
6. Document and test the exact environment mechanism used to point a real Claude CLI process at an isolated temporary configuration. The test must assert sentinel files in the developer's real `~/.claude` are untouched.
7. Move active-session reading and process-liveness helpers out of `planes/`, preserve their public exports, and update their tests and call sites.
8. Create `docs/rfcs/007-removal-manifest.md` with four explicit categories:
   - `delete-in-phase-3` production paths/imports;
   - `compatibility-until-phase-3` SDK barrels, CLI registrations, commands, and views that must remain through the deprecation release;
   - `retain-diagnostic` plugin IDs, marketplace identity, doctor/manual-command references, and `spag uninstall` instructions;
   - `retain-history` dated documents that receive supersession notes.
     Every inventory hit belongs to exactly one category.
9. Rehearse local marketplace registration and both plugin installs from a clean checkout of the current commit in an isolated Claude environment. This proves the tag-based recovery mechanism before the deprecation tag exists.

### Exit gate

- Doctor still reports active indexing after the relocation.
- The standalone probe distinguishes every required present/absent/unknown, scope, enabled-only, and source-mismatch fixture without false-clean output.
- The probe has no dependency on the command that Phase 3 deletes.
- The pure planner produces only argument vectors, implements the normative ordering and marketplace guard, and has no real subprocess side effects in unit tests.
- The verified minimum Claude cleanup version/capabilities are recorded in this RFC and package docs before Phase 1 begins; an unsupported CLI performs no mutation.
- The isolated real-CLI mechanism and local-checkout marketplace rehearsal pass without touching the developer's real Claude state.
- The removal manifest has no uncategorized production or current-document reference.
- Baseline `pnpm build && pnpm typecheck && pnpm test` is green.

No deprecation messaging ships until this gate passes.

---

## Phase 1 — Deprecation and guided uninstall release

### CLI behavior

- Add `--yes` to the `plugin` command registration and implement `spag plugin uninstall` through the Phase 0 probe, pure planner, and injected executor.
- The no-target command is the full user-scope cleanup: disable/uninstall both plugin IDs, verify them absent, then remove only the matching user marketplace.
- A targeted command disables/uninstalls only the selected user-scope plugin and never removes the shared marketplace.
- `spag plugin install` remains functional from the canonical marketplace throughout the deprecation window but prints the exact removal version and the immutable-tag recovery limitation.
- `spag hooks`, `spag chat`, and `spag plugin install|status` emit the normative deprecation warning on stderr.
- Update `spag uninstall` to consume the read-only probe and, when expected user-scope leftovers are present, put `spag plugin uninstall --yes` before the npm uninstall step. Unknown/source-mismatch state is diagnostic only. Explain that removing the npm CLI alone does not stop installed Claude plugins. Replace the broad `rm -rf ~/.spaghetti` default suggestion with cache-specific paths; any full data purge is a separate, explicit warning outside the retirement flow.
- Doctor and TUI consume `lib/plugin-leftovers.ts` directly in this phase. Remove their install/enable calls to action and render the normative leftover states.

### SDK deprecation

- Add published `@deprecated` annotations, with the exact removal version, to every `delete-in-phase-3` SDK declaration, including:
  - `SpaghettiAPI.runtime` and `SpaghettiRuntime`;
  - `createRuntimeBridge`, `RuntimeBridge`, and `CreateRuntimeBridgeOptions`;
  - `RuntimeEvent` and its guards;
  - hook watcher, channel registry/client/manager, hook/channel wire types and helpers;
  - hook/channel source-path fields.
- Do not deprecate the relocated active-session helpers or `ActiveSessionFile`.
- Verify the generated SDK declaration artifact contains the annotations. Runtime hook/channel messages say “no replacement; transcript ingest/query/live updates are unaffected,” while active-session methods point to the retained helpers.

### Product messaging

- Implement the normative doctor and once-per-process TUI notice behavior without adding persisted state.
- Update the root, SDK, and CLI changelogs with the deprecation version, exact removal version, deprecation Git tag, `spag plugin uninstall --yes`, raw Claude fallback commands, and immutable recovery runbook. Record the tag's resolved commit SHA in the post-tag canonical release evidence.
- Mark the SDK runtime surface, `spag hooks`, `spag chat`, `spag plugin`, and the two TUI views deprecated in current package READMEs and the site; publish the Phase 0 minimum Claude cleanup version/capability requirement and state that uninstall preserves persistent data.
- Preserve dated architecture/audit documents unchanged until Phase 3 adds supersession notes.

### Recovery artifact

- Publish the deprecation Git tag from the exact release commit.
- From a clean checkout of that tag, register the absolute checkout path as a user marketplace in an isolated Claude environment and install both plugins.
- Record the tag, commit SHA, deprecation package version, and successful rehearsal in the release evidence.

### Verification matrix

Run unit cases with fake homes/executors and repeat the successful and partial-failure cases against the isolated real Claude environment:

| Initial state                                      | Invocation/mode                   | Expected mutation/result                                      | Exit |
| -------------------------------------------------- | --------------------------------- | ------------------------------------------------------------- | ---- |
| clean                                              | no target, `--yes`                | none; clean no-op                                             | 0    |
| user hooks only                                    | no target, `--yes`                | hooks disabled if needed, uninstalled                         | 0    |
| user channel only                                  | no target, `--yes`                | channel disabled if needed, uninstalled                       | 0    |
| both user plugins                                  | no target, `--yes`                | hooks then channel removed                                    | 0    |
| expected marketplace only                          | no target, `--yes`                | user marketplace removed                                      | 0    |
| both user plugins + expected marketplace           | no target, `--yes`                | plugins verified absent, then marketplace removed             | 0    |
| enabled-setting-only plugin + expected marketplace | no target, `--yes`                | setting disabled, then marketplace removed if fully clean     | 0    |
| hooks + channel + marketplace                      | target hooks, `--yes`             | hooks only; channel and marketplace retained                  | 0    |
| user + project/local hooks                         | target hooks, `--yes`             | user hooks removed; non-user copy retained and reported       | 0    |
| executable plan                                    | interactive confirmation accepted | exact previewed plan executes                                 | 0    |
| executable plan                                    | interactive confirmation declined | none; explicit cancelled result                               | 2    |
| executable plan                                    | non-interactive, no `--yes`       | none; exact commands and authorization requirement printed    | 2    |
| visible project/local installation                 | no target, `--yes`                | none; scope/context and manual commands reported              | 2    |
| source-mismatched `spaghetti` marketplace          | no target, `--yes`                | none; mismatch reported, registration retained                | 2    |
| malformed/unreadable/unsupported relevant state    | no target, `--yes`                | none; affected path/command reported as unknown               | 2    |
| Claude executable unavailable or unsupported       | non-empty plan, `--yes`           | none; compatible manual guidance printed                      | 1    |
| middle command fails or postcondition remains      | no target, `--yes`                | fail-fast; no marketplace removal; exact remaining work shown | 1    |

### Release gate

- The deprecation CLI and SDK packages are published and installable, and their generated help/declarations contain the promised warning and deprecation metadata.
- Guided uninstall passes the matrix, including exit codes, argument vectors, confirmation modes, fail-fast behavior, and post-command re-probes.
- Every uninstall argument vector contains explicit `--scope user` and `--keep-data`; sentinel plugin-data and `~/.spaghetti` hook/channel files survive both fake and isolated-real cleanup cases.
- The supported and unsupported Claude capability fixtures both pass; unsupported versions perform no mutation.
- Doctor/TUI behavior is verified for clean, plugin-only, marketplace-only, source-mismatch, and unknown states.
- `spag uninstall` prints plugin cleanup before npm removal and does not present recursive `~/.spaghetti` deletion as cache-only cleanup.
- The immutable tag is present on the canonical remote and the clean-checkout plugin install rehearsal passes.
- A pre-cutover smoke test proves the canonical default-branch marketplace still installs both plugins.
- Release notes identify the exact deprecation version as the last version containing `spag plugin uninstall` and the exact next-minor removal version.

Phase 3 may be prepared after the published packages, immutable tag, changelogs, and recovery rehearsal exist, but its deletion commit cannot merge to the canonical marketplace branch until the Phase 4 removal cutover. The waiting period is one published minor release, not a calendar guess.

---

## Phase 2 — Detach retained functionality

This phase may be developed and tested during the deprecation window, but no published version disables the bridge before the Phase 3 removal release.

### SDK work

- Rewire doctor/index-live consumers to the relocated active-session module without routing through `RuntimeBridge`.
- Put runtime-bridge construction behind one package-private composition seam and exercise bridge-disabled mode in tests. The seam is not an exported option, environment variable, user setting, or generated declaration. The shipped default remains enabled until Phase 3 removes the public surface.
- Prove config, analytics, transcript ingest, query, and live transcript updates do not import Plane 3.
- Keep the deprecated SDK barrel exports working through the deprecation release; classify those edges as `compatibility-until-phase-3` rather than trying to detach them early.

### CLI work

- Rewire doctor to the relocated active-session reader.
- Verify the Phase 1 doctor/TUI leftover rendering imports only `lib/plugin-leftovers.ts` and shared presentation helpers, never hooks/chat/plugin command modules.
- Keep retiring command/view registrations working through the deprecation release and classify their imports as `compatibility-until-phase-3`.

### Exit gate

- No retained non-Plane-3 implementation module imports a `delete-in-phase-3` module. The only remaining production edges into retiring code originate from the deprecated SDK barrels and the explicitly retiring CLI commands/views recorded as `compatibility-until-phase-3`.
- TUI boot, doctor, and active-index reporting work with the bridge construction disabled.
- Doctor and `spag uninstall` still render leftover cleanup with all hooks/chat/plugin command modules and `lib/plugin-cleanup.ts` unavailable in the test graph.
- Every removal-manifest entry remains in its declared category; `retain-diagnostic` and `retain-history` references are explicitly excluded from deletion grep failures.
- No uncategorized Plane 3 import or current-document reference remains.

---

## Phase 3 — Remove Plane 3

Develop and verify this phase on the removal release branch. Passing its exit gate makes the commit a release candidate; it does not authorize merging the plugin deletion to the canonical marketplace branch before Phase 4.

### SDK deletions

| Path                                                                           | Reason                    |
| ------------------------------------------------------------------------------ | ------------------------- |
| `packages/sdk/src/planes/runtime-bridge.ts`                                    | bridge factory            |
| `packages/sdk/src/runtime/spaghetti-runtime.ts` and remaining `runtime/` files | `api.runtime` facade      |
| `packages/sdk/src/events/runtime-event.ts` and remaining `events/` files       | runtime event union       |
| `packages/sdk/src/io/hook-event-watcher.ts`                                    | hook JSONL tail           |
| `packages/sdk/src/io/channel-registry.ts`                                      | channel discovery watcher |
| `packages/sdk/src/io/channel-client.ts`                                        | websocket channel client  |
| `packages/sdk/src/io/channel-manager.ts`                                       | client-fleet manager      |
| `packages/sdk/src/types/spaghetti/`                                            | hook/channel wire types   |
| `packages/sdk/src/types/hook-events.ts`                                        | deprecated shim           |
| `packages/sdk/src/types/channel-messages.ts`                                   | deprecated shim           |

### SDK edits

- Remove runtime fields and construction from `api.ts`, `create.ts`, and `app-service.ts`.
- Remove runtime event, bridge, watcher, channel, and wire-type exports from SDK barrels while preserving the relocated active-session exports.
- Remove only `hookEventsFile`, `channelSessionsDir`, and `channelMessagesDir` from `sources/types.ts` and the Claude/Codex/Grok path implementations. Retain `sessionsDir` for active-session/index reporting and retain unrelated settings/config paths.
- Remove both `ws` and `@types/ws` from the SDK manifest and regenerate the lockfile.
- Delete runtime-bridge tests, remove the Plane 3 assertion/import from `sources/__tests__/claude-code-source.test.ts`, update source-path tests for Claude/Codex/Grok, and retain relocated active-session tests.
- Remove the Phase 2 internal construction seam with the bridge; do not leave a dead feature flag or test-only option.

### CLI deletions and edits

- Delete `commands/hooks.ts`, `commands/chat.ts`, and `commands/plugin.ts`.
- Delete `lib/plugins.ts` and the temporary `lib/plugin-cleanup.ts` planner/executor plus mutation tests; retain `lib/plugin-leftovers.ts` and its read-only probe tests.
- Delete `views/hooks-monitor-view.tsx` and `views/chat-view.tsx`; keep the unrelated React list-navigation helper named `views/hooks.ts`.
- Unregister the commands, aliases, known-command suggestions, menu entries, plugin stats, and all hook/channel doctor collectors and renderers; remove the corresponding navigation discriminants from `views/types.ts`.
- Keep active-index doctor reporting plus the read-only leftover section. Its manual commands are raw `claude plugin disable/uninstall --keep-data/marketplace remove` commands, never the now-removed `spag plugin` command.
- Update retained `spag uninstall` to consume the read-only probe, run before npm removal, and print raw user-scope cleanup commands only for identities proven to belong to Spaghetti. Source-mismatch/unknown cases print diagnostics and refer to `spag doctor` without a destructive marketplace command. Cache-only instructions name cache paths rather than all of `~/.spaghetti`. It remains instructional and performs no Claude mutation.
- Remove both `ws` and `@types/ws` from the CLI manifest.

### Plugin packages

- Delete `packages/claude-code-hooks-plugin/`.
- Delete `packages/claude-code-channels-plugin/`.
- Remove both plugin entries from the root `.claude-plugin/marketplace.json`; delete the manifest/directory only if nothing else uses it.
- Regenerate `pnpm-lock.yaml`.
- Do not delete, move, or retag the immutable deprecation recovery tag.

### Docs and site

- Move the current architecture content from `docs/THREE-PLANE-INGEST-ARCHITECTURE.md` to `docs/TWO-PLANE-INGEST-ARCHITECTURE.md` and rewrite it for the retained static/live-disk planes. Leave the old path as a short supersession pointer so dated RFC/plan links remain valid; update root/current README links and retained SDK source comments to the new path.
- Remove Plane 3, hooks, chat, and plugin command sections from the site and current READMEs.
- Update `docs/coverage/claude-code.md` and its machine source `scripts/coverage/claude_code/claim.json` so active sessions point to the relocated reader rather than `api.runtime`; update other current design documents found by the removal manifest.
- Add a supersession note to `docs/PR-PLAN-THREE-PLANE-SHAPE.md` and other dated architecture records.
- Keep dated audits as history.
- Add root, SDK, and CLI removal entries while retaining the earlier deprecation entries, exact cleanup commands, and immutable-tag rollback runbooks.

### Exit gate

- `pnpm build && pnpm typecheck && pnpm test` passes.
- TUI boots and doctor renders without removed sections.
- Doctor still reports active indexing and all clean/present/source-mismatch/unknown leftover states without importing a removed module.
- `spag uninstall` gives usable raw cleanup instructions without referring to `spag plugin`.
- Removed commands and aliases produce the normal unknown-command response and are not suggested as known commands.
- Filtered `pnpm why ws` and `pnpm why @types/ws` show neither dependency in the SDK or CLI.
- The lockfile has no importer for either deleted plugin package.
- Built SDK declarations and package exports contain no removed Plane 3 symbol or source-path field.
- Repository dependency search finds no production import of a deleted module. String searches allow only `retain-diagnostic` and `retain-history` manifest entries.
- Package-content inspection finds no runtime bridge, hook/channel implementation, plugin package, marketplace manifest, `ws`, or `@types/ws` in the packed SDK/CLI release-candidate artifacts.

---

## Phase 4 — Removal release and aftercare

1. Build and inspect the final SDK/CLI packages and site deployment from the Phase 3 release candidate while the canonical marketplace branch still contains Plane 3.
2. Re-run the canonical marketplace install smoke test immediately before cutover.
3. Merge the verified Phase 3 deletion commit to the canonical marketplace branch and publish the removal packages, site, and final manual cleanup commands as one coordinated cutover.
4. If any required publication fails, revert the deletion commit to restore the canonical marketplace before retrying; do not leave a removal-only default branch paired with unpublished packages/docs.
5. Verify upgrade from the last deprecation release with:
   - no leftovers;
   - user plugin leftovers;
   - enabled-setting-only leftovers;
   - marketplace-only leftovers;
   - a source-mismatched marketplace;
   - unknown/unreadable state;
   - a visible project/local installation.
6. After the default branch no longer contains the plugins, repeat a fresh recovery from the immutable deprecation tag in an isolated Claude environment and prove both plugins install and run.
7. Verify the published SDK/CLI package contents, generated declarations, help output, root/SDK/CLI changelogs, deployed site, and generated lockfile rather than relying on the working tree.
8. Keep the read-only doctor warning for at least one additional minor release and verify it introduces no persistent acknowledgement/config file.
9. Close the RFC only after the release evidence links the deprecation artifact, immutable tag/commit, cleanup matrix, removal artifacts, upgrade checks, cutover/revert rehearsal, and post-deletion recovery rehearsal.

---

## Rollback

- Before Phase 3 ships: revert the deletion commits; no application-data rollback exists because the release changes no ingest/database data and guided uninstall preserves plugin persistent data.
- After the removal release, users needing the SDK/CLI Plane 3 surface pin the exact last deprecation package versions recorded in the changelogs.
- Existing installed plugins are not reconstructed or silently changed by the removal release.
- A fresh post-removal plugin reinstall uses the immutable deprecation Git tag, not the pinned CLI installer:
  1. Clone the canonical repository at the recorded deprecation tag and verify its commit SHA.
  2. Inspect `claude plugin marketplace list --json`. If no user marketplace named `spaghetti` exists, add the absolute checkout path with `claude plugin marketplace add --scope user <absolute-checkout>`.
  3. If the expected default-branch user registration exists, the runbook explicitly asks the user before replacing it with the local tagged checkout. A source mismatch or project/local registration stops the automated runbook for manual resolution.
  4. Install `spaghetti-hooks@spaghetti` and `spaghetti-channel@spaghetti` with explicit `--scope user`.
- The deprecation release notes replace `<deprecation-version>`, `<deprecation-tag>`, `<commit-sha>`, and `<absolute-checkout>` placeholders with concrete values or concrete derivation instructions before Phase 3 merges.
- The retained transcript ingest/query/live planes continue independently during rollback.

---

## Overall completion criteria

- Two-release sequence completed.
- No Plane 3 runtime code or bundled plugin remains.
- No user plugin was silently removed.
- Guided cleanup preserved plugin persistent data and did not delete existing hook/channel data files.
- `spag uninstall` shows state-specific plugin cleanup before npm removal and distinguishes cache-only cleanup from an explicit full-data purge.
- The deprecation release warned every removed SDK/CLI/TUI surface and shipped a verified guided uninstall.
- Active-session doctor reporting remains functional.
- Leftover plugin, enabled-setting, marketplace, scope, source-mismatch, and unknown states remain diagnosable.
- Neither `ws` nor `@types/ws` remains owned by the SDK or CLI.
- The immutable tag supports a fresh, tested post-removal plugin recovery; pinning npm alone is never presented as sufficient.
- No ingest, database, or engine-selection behavior changed as part of this RFC.

# RFC 007: Retire the Runtime Bridge

**Status:** Accepted (v3 — single-release removal)
**Implementation readiness:** Phase 0 complete (2026-08-08); removal in progress.
**Created:** 2026-08-06
**Revised:** 2026-08-08
**Split from:** `007-retire-runtime-bridge-and-ts-ingest.md` Draft v6
**Author:** James Yong + Kimi
**Companion RFCs:** [RFC 008 — Rust Bulk Ingest Production Readiness](./008-rust-ingest-production-readiness.md) · [RFC 009 — Retire the TypeScript Bulk Ingest Engine](./009-retire-typescript-bulk-ingest.md)
**Removal manifest:** [007-removal-manifest.md](./007-removal-manifest.md)

> **v3 revision (2026-08-08).** Spaghetti is pre-1.0 with no external consumers
> of the CLI or SDK. A staged deprecation existed to protect users who do not
> exist, and its machinery — a deprecation release, a guided uninstall command,
> `@deprecated` annotations, an immutable recovery tag, a migration runbook —
> cost more than the risk it hedged. v3 deletes Plane 3 in **one release**.
>
> What survives from the staged plan is the part that is still load-bearing: the
> read-only leftover probe, so `spag doctor` can tell you whether the plugins are
> still installed in Claude Code and print the raw commands to remove them.
> Uninstalling the npm package never removed those, and that is still true.
>
> Recovery, if it is ever wanted, is ordinary git history: the plugins live at
> commit `211f4b1` and its ancestors. No special tag is cut or maintained.

---

## Summary

Retire Plane 3: the runtime bridge, hook-event and channel-session streaming surfaces, their CLI/TUI commands, and the two Claude Code plugin packages.

This RFC does one thing only. It does not change cold/warm ingest, engine selection, SQLite files, the query path, or the live transcript writer. Those concerns moved to RFCs 008 and 009 so this removal can ship independently and be reviewed as a bounded product-surface change.

The retirement ships in one release: the bridge and plugins are deleted, and a read-only doctor check for leftovers is retained so a user whose Claude Code still has the plugins registered is told, and told how to remove them.

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
6. Do not mutate Claude plugin or marketplace state at all. Spaghetti prints commands; the user runs them.
7. Do not recursively discover arbitrary project/local plugin declarations. Detected non-user state is diagnostic only.
8. Do not purge plugin persistent data or the existing `~/.spaghetti/hooks`/`~/.spaghetti/channel` files. Retirement disables/uninstalls code and registrations only.
9. Do not decide the future of the Rust engine. RFC 008 owns readiness; RFC 009 owns cutover.

---

## Decisions

| ID  | Decision                         | Choice                                                                                                                                              |
| --- | -------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------- |
| R1  | Plugin fate                      | **(v3)** Delete both bundled plugins in a single release. No deprecation window — pre-1.0, no external consumers                                     |
| R2  | Uninstall authorization          | **(v3)** No automated uninstall ships. Doctor and `spag uninstall` print raw `claude` commands; the user runs them                                   |
| R3  | Leftover detection after removal | Keep a read-only doctor probe; do not keep the plugin command                                                                                       |
| R4  | Historical documents             | Keep dated records and add supersession notes instead of rewriting history                                                                          |
| R5  | Cleanup scope                    | **(v3)** Printed commands cover user-scope Spaghetti identities only; project/local, source-mismatched, and unknown state is reported for the user  |
| R6  | Post-removal recovery            | **(v3)** Ordinary git history. The plugins remain reachable at `211f4b1` and its ancestors; no tag is cut, and none is maintained                    |
| R7  | Notice lifetime                  | **(v3)** Doctor renders current leftover state. No TUI banner, no deprecation warnings — there is no window for them to appear in                    |
| R8  | Removal cutover                  | **(v3)** One commit. Removing the plugins from the default branch and from the packages is the same event                                            |
| R9  | Plugin persistent data           | Preserve it. The printed commands use `--keep-data`; purging plugin-generated data is outside this RFC                                               |

Changing one of these decisions requires editing this table before implementation.

**Why R1/R6/R7/R8 changed in v3.** The staged plan spent a release, a bespoke
uninstall command with a planner and capability gate, ~40 `@deprecated`
annotations, an immutable tag, and a migration runbook on protecting consumers
that do not exist. The residual risk it actually hedged — someone upgrading with
plugins still registered in Claude Code — is fully covered by the retained
doctor probe, which reports the leftovers and prints the exact removal commands.
That probe is the only piece of the staged machinery worth keeping.

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

#### Phase 0 capability record (verified 2026-08-08)

`MINIMUM_CLEANUP_VERSION` is **2.1.223**, declared in `packages/cli/src/lib/plugin-cleanup.ts`.

The gate was exercised end to end against **Claude Code 2.1.226** on win32 x64. Captured fixtures live in `packages/cli/src/__tests__/fixtures/claude-cli/`; a synthetic older CLI lives in `fixtures/claude-cli-unsupported/` and must always fail the gate.

| Command                            | Required flag  | 2.1.226 behavior                                       |
| ---------------------------------- | -------------- | ------------------------------------------------------ |
| `claude plugin disable`            | `--scope user` | `-s, --scope <scope>`, default **auto-detect**         |
| `claude plugin uninstall`          | `--scope user` | `-s, --scope <scope>`, default `user`                  |
| `claude plugin uninstall`          | `--keep-data`  | preserves `~/.claude/plugins/data/{id}/`               |
| `claude plugin marketplace remove` | `--scope user` | `--scope <scope>`; **omitting it removes every scope** |

The last two rows are why the flags are non-negotiable: without `--scope`, marketplace removal reaches project and local scopes, and disable resolves a scope by inference rather than instruction.

The isolated-environment mechanism is the **`CLAUDE_CONFIG_DIR`** environment variable, verified in `packages/cli/src/__tests__/claude-cli-capabilities.test.ts`. It redirects the whole configuration root; the test asserts a sentinel file and a `~/.claude/plugins` snapshot in the developer's real home are unchanged after driving the real executable.

### Manual cleanup guidance

No command mutates Claude Code state. Doctor and `spag uninstall` print raw
commands for the user to run, and only for identities and scopes the probe
proved belong to Spaghetti:

```
claude plugin disable   --scope user <plugin-id>
claude plugin uninstall --scope user --keep-data <plugin-id>
claude plugin marketplace remove --scope user spaghetti
```

Three rules govern what may be printed:

- `--scope user` is never omitted. Without it, `claude plugin marketplace remove`
  removes the declaration from *every* scope and `claude plugin disable`
  auto-detects one. `--keep-data` is never omitted either, so plugin data
  directories survive.
- The marketplace command appears only when the registration's source normalises
  to `vibecook-dev/spaghetti`. For a source mismatch, print the inspection
  evidence and say the user must resolve it — never a removal command for a
  registration whose ownership is unproven.
- Non-user installations and `unknown` results are reported with their scope and
  path so the user can act. They are never presented as clean.

Requires Claude Code 2.1.223 or newer for the flags above; see the Phase 0
capability record.

### Notice behavior

Doctor always renders the Plane 3 leftover section: `clean`, named leftovers,
source mismatches, or `unknown` paths. It never offers install or enable
actions. There is no TUI banner and no deprecation warning anywhere — the
surfaces those would have warned about are gone in the same release.

Keep the doctor section for at least one additional minor release.


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

## Removal — detach, delete, ship

One release. Phase 0's harness is already in place; everything below lands
together.

### Detach first

- Rewire doctor and index-live consumers to the relocated active-session module
  instead of `RuntimeBridge`.
- Prove config, analytics, transcript ingest, query, and live transcript updates
  do not import Plane 3.
- Verify doctor/TUI leftover rendering imports only `lib/plugin-leftovers.ts` and
  shared presentation helpers.

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

### CLI deletions and edits

- Delete `commands/hooks.ts`, `commands/chat.ts`, and `commands/plugin.ts`.
- Delete `lib/plugins.ts`; retain `lib/plugin-leftovers.ts` and its read-only probe tests.
- Delete `views/hooks-monitor-view.tsx` and `views/chat-view.tsx`; keep the unrelated React list-navigation helper named `views/hooks.ts`.
- Unregister the commands, aliases, known-command suggestions, menu entries, and all hook/channel doctor collectors and renderers; remove the corresponding navigation discriminants from `views/types.ts`.
- Keep active-index doctor reporting plus the read-only leftover section. Its manual commands are raw `claude plugin disable/uninstall --keep-data/marketplace remove` commands, never a `spag plugin` command.
- `spag uninstall` consumes the read-only probe, lists plugin cleanup before npm removal, and prints raw user-scope commands only for identities proven to belong to Spaghetti. Source-mismatch/unknown cases print diagnostics and refer to `spag doctor`. Cache-only instructions name cache paths rather than all of `~/.spaghetti`. It performs no mutation.
- Remove both `ws` and `@types/ws` from the CLI manifest.

### Plugin packages

- Delete `packages/claude-code-hooks-plugin/`.
- Delete `packages/claude-code-channels-plugin/`.
- Remove both plugin entries from the root `.claude-plugin/marketplace.json`; delete the manifest/directory only if nothing else uses it.
- Regenerate `pnpm-lock.yaml`.

### Docs and site

- Move the current architecture content from `docs/THREE-PLANE-INGEST-ARCHITECTURE.md` to `docs/TWO-PLANE-INGEST-ARCHITECTURE.md` and rewrite it for the retained static/live-disk planes. Leave the old path as a short supersession pointer so dated RFC/plan links remain valid; update root/current README links and retained SDK source comments to the new path.
- Remove Plane 3, hooks, chat, and plugin command sections from the site and current READMEs.
- Update `docs/coverage/claude-code.md` and its machine source `scripts/coverage/claude_code/claim.json` so active sessions point to the relocated reader rather than `api.runtime`.
- Add a supersession note to `docs/PR-PLAN-THREE-PLANE-SHAPE.md` and other dated architecture records.
- Add root, SDK, and CLI changelog entries describing the removal and the manual cleanup commands.

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
- Package-content inspection finds no runtime bridge, hook/channel implementation, plugin package, marketplace manifest, `ws`, or `@types/ws` in the packed SDK/CLI artifacts.

### Aftercare

1. Verify the published package contents, generated declarations, help output, changelogs, deployed site, and generated lockfile rather than relying on the working tree.
2. Keep the read-only doctor warning for at least one additional minor release and verify it introduces no persistent acknowledgement/config file.
3. Close the RFC once the removal artifacts and the doctor leftover checks are verified.


## Rollback

- Revert the deletion commit. There is no application-data rollback to perform: the release changes no ingest or database data, and it never touched plugin state.
- Anyone who wants the Plane 3 surface back can pin the last package version that contained it, or check out `211f4b1`. Git history is the recovery mechanism; no tag is cut for the purpose.
- Existing installed plugins are not reconstructed or silently changed. If Claude Code still has them registered, they keep working from their own cache until the user removes them — doctor says so and prints the commands.
- The retained transcript ingest/query/live planes are unaffected throughout.

---

## Overall completion criteria

- No Plane 3 runtime code or bundled plugin remains.
- No user plugin was silently removed, and no plugin data was deleted.
- `spag uninstall` shows state-specific plugin cleanup before npm removal and distinguishes cache-only cleanup from an explicit full-data purge.
- Active-session doctor reporting remains functional.
- Leftover plugin, enabled-setting, marketplace, scope, source-mismatch, and unknown states remain diagnosable.
- Neither `ws` nor `@types/ws` remains owned by the SDK or CLI.
- No ingest, database, or engine-selection behavior changed as part of this RFC.

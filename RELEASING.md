# Releasing

This repository uses `release-please` as the single source of truth for releases.

## Policy

- Do not manually bump versions in the root `package.json`, `packages/cli/package.json`, `packages/sdk/package.json`, or `crates/spaghetti-napi/package.json` (or its per-platform npm shims under `crates/spaghetti-napi/npm/*/package.json`).
- Do not manually edit `.release-please-manifest.json`.
- Do not manually create release commits as part of the standard release flow.
- Do not manually tag versions unless you are explicitly repairing a broken release state.

`release-please` owns:

- version bumps
- root changelog updates
- release PR creation
- release tag creation
- GitHub release creation

The publish workflow then publishes the released package versions from the merged release commit.

## Normal Flow

1. Merge normal commits into `main`.
2. Wait for `release-please` to open or update its release PR.
3. Review that PR like any other PR.
4. **Approve its checks and wait for `NAPI Build` to pass.** Checks on release
   PRs land as `action_required` — GitHub holds workflow runs on bot-authored
   PRs until someone approves them, so they will sit indefinitely untouched.
   `NAPI Build` runs the full 6-target matrix and load checks here.
5. Merge the release PR.
6. Let the `Release` GitHub Actions workflow publish the packages.

Step 4 is the gate that matters. Merging cuts the tag, and from that moment the
version is spent whether or not anything reaches npm (see below) — so the last
opportunity to find a broken native build is while the release PR is still open.

## If a Release Fails

**Roll forward. Do not try to finish the broken version.** Land the fix on
`main`, let `release-please` open a new release PR, and release the next patch.
The failed version simply never existed on npm, which is normal and cheap.

Repairing a release in place is not possible, for two independent reasons:

- **A `workflow_dispatch` always reads its workflow definition from the ref it
  targets.** Dispatching `napi-build.yml` against a tag re-runs that workflow
  file *as it existed at that tag*, so a CI fix merged to `main` afterwards has
  no effect there. `napi-build.yml` has a `source_ref` input for exactly this —
  `gh workflow run napi-build.yml --ref main -f publish=true -f source_ref=<tag>`
  runs `main`'s workflow against the tag's source — but note it splits the two,
  so only reach for it when the fix is confined to workflow logic.
- **`release.yml`'s publish job cannot run for an existing tag.** It ships
  `@vibecook/spaghetti-sdk` and `@vibecook/spaghetti`, and is `on: push` to
  `main` gated on `release_created`; `release-please` will not re-create a
  version it has already released. Re-running the old failed run does not work
  either — its native-build watch step waits on the original run id, which is a
  completed failure, so it aborts immediately. There is no `NPM_TOKEN` to
  publish by hand; publishing is token-less OIDC only.

So `source_ref` recovers the native package alone. The SDK and CLI halves of a
burned release have no path to npm, which is why the release-PR gate above
exists and why rolling forward is the documented answer.

## Commit Conventions

`release-please` derives release notes and bump behavior from commit history.

- Use `feat:` for user-visible features.
- Use `fix:` for bug fixes.
- Use scoped conventional commits when useful, for example `feat(cli): ...` or `fix(channel): ...`.

In practice:

- `feat:` will normally drive a minor release.

### Multi-agent native (Grok)

The Rust addon (`@vibecook/spaghetti-sdk-native`) includes Grok cold/warm and live batch paths. After landing Grok native features on `main`, the next release-please publish is what ships those binaries to npm end users. Local `pnpm --filter @vibecook/spaghetti-sdk-native build` is enough for workspace testing.
- `fix:` will normally drive a patch release.

## Current Baseline

The authoritative baseline lives in `.release-please-manifest.json` (single component `.`) and is automatically advanced by `release-please` on each release PR merge. `release-please-config.json`'s `extra-files` list is what propagates that single bump into every package version in lock-step — currently the root `package.json`, `packages/cli/package.json`, `packages/sdk/package.json`, `crates/spaghetti-napi/package.json`, and the per-platform npm shims under `crates/spaghetti-napi/npm/*/package.json`.

Adding a new published workspace member (another platform crate, a new SDK subpackage, etc.) means adding its `package.json` to `extra-files` — otherwise it will drift out of the lock-step bump and fail publishing.

### Generated files and the version bump

Two files in the Rust addon are generated and embed the version. Both used to break the CI `Generated files are current` check on every release PR, because `release-please` bumps `package.json`/`Cargo.toml` but cannot run a build:

- **`Cargo.lock`** is bumped by an `extra-files` entry using the `toml` updater. Its jsonpath is
  `$.package[?(@.name.value==='spaghetti-napi')].version` — note **`.value`**. The updater parses
  TOML into position-annotated nodes (`{start, end, value}`), so the intuitive `@.name==='…'`
  matches nothing and the updater silently no-ops with only a `No entries modified` warning.
  Match by name rather than array index: `[[package]]` is sorted alphabetically, so an index would
  silently retarget a different crate when dependencies change.
- **`crates/spaghetti-napi/index.js`** is deliberately **not** committed (see `.gitignore`). `napi build`
  regenerates it and stamps the version into a per-platform guard in ~50 places, which no updater can
  track. `index.d.ts` stays committed — it is the reviewable API surface and carries no version.

  It travels as a **build artifact**: `napi-build.yml`'s build job uploads it next to the `.node`
  binary, the test job gets it from the artifact download, and the publish job lifts it out of an
  artifact before packing. Note that **neither the test job nor the publish job runs a build** — they
  consume prebuilt binaries — so nothing may assume a fresh checkout contains a loader. `napi` has no
  loader-only generator (`napi build` is the sole producer and needs the Rust toolchain), which is why
  it moves through artifacts rather than being regenerated on demand.

  Getting this wrong fails quietly in one direction: `files` in `package.json` silently skips entries
  that are absent on disk, so a missing loader would publish a package whose `main` points at nothing.
  The publish job therefore hard-fails if no loader is found and `require()`s the assembled package
  before publishing.

Future releases should continue from whatever the manifest currently records — through `release-please`, not through manual version/tag commits.

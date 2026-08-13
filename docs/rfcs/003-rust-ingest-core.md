# RFC 003: Rust Ingest Core

**Status**: Superseded by [RFC 011](./011-rust-observation-query-engine.md);
retained as historical migration context
**Created**: 2026-04-16
**Author**: James Yong + Claude

---

## Summary

Replace the Node-based ingest pipeline in `@vibecook/spaghetti-sdk` with a Rust native addon (`@vibecook/spaghetti-sdk-native`) that owns everything from JSONL read through SQLite write. The public SDK API (`AppService`, React hooks, channel plugins, types) stays pure TypeScript and unchanged. Queries continue to run on Node via `better-sqlite3` against the same SQLite file.

Target: **cold start 1.5–3s → ~600ms**, **warm start 50–200ms → ~30ms**, with no API churn for SDK consumers.

---

## Motivation

Architecture C (RFC implicit, 2026-03-21) took cold start on a 500MB `~/.claude` from 15–30s down to 1.5–3s. Remaining time breaks down roughly as:

| Stage | % of cold start |
|---|---:|
| JSONL line parse (`JSON.parse` + `Buffer.toString('utf-8')`) | ~45% |
| Worker → main IPC (`JSON.stringify` + structuredClone on every batch) | ~25% |
| SQLite writes (FTS5 triggers dominate) | ~20% |
| Filesystem syscalls, misc | ~10% |

The first two lines — ~70% of wall time — are artifacts of running ingest on V8 across worker threads. Neither is addressable from TS without leaving the runtime (WASM simd-json helps JSONL parse but not IPC). Rust addresses both simultaneously: `sonic-rs` for JSON, and a single in-process worker pool with zero boundary serialization.

The third line (SQLite + FTS5) is language-independent and is not a target of this RFC.

---

## Non-Goals

To keep scope honest, this RFC explicitly does **not**:

1. Rewrite the query side. `AppService`, `ClaudeCodeAgentDataService` read paths, `search()`, summaries, and all React hooks stay in TS. `better-sqlite3` continues to serve reads.
2. Rewrite the CLI, TUI, UI, playground, or channel plugins.
3. Change the SQLite schema or migration semantics. Rust uses the existing schema as-is.
4. Introduce a sidecar process, IPC protocol, or network boundary. Addon is in-process.
5. Replace Electron, Tauri-ify the app, or touch distribution of `@vibecook/spaghetti`.
6. Optimize FTS5 indexing or query performance. That's a separate follow-up (`RFC 00X: FTS5 deferred indexing`).
7. Add new ingest capabilities (teams/, backups/ from the 2026-03-20 audit). Those land in TS first, then get ported.

Anything not in this list is out of scope until a follow-up RFC.

---

## Architecture Overview

```
┌─────────────────────────────────────────────────────────┐
│  @vibecook/spaghetti-sdk  (TypeScript, unchanged public API)     │
│                                                          │
│   AppService  ─┬─►  ClaudeCodeAgentDataService          │
│                │                                         │
│                │   ┌─ ingest path ──────────────────┐   │
│                │   │                                 │   │
│                └──►│  NativeIngest.run(opts)  ◄──────┼──►│  @vibecook/spaghetti-sdk-native
│                    │                                 │   │  (Rust, napi-rs addon)
│                    │  (await Promise<Stats>)         │   │
│                    └─────────────────────────────────┘   │
│                                                          │
│                ┌─ query path (unchanged) ──────────┐    │
│                │  better-sqlite3 read connection    │    │
│                │  prepared statements, FTS5 MATCH   │    │
│                └────────────────────────────────────┘    │
└─────────────────────────────────────────────────────────┘
                              │
                              ▼
                    ┌──────────────────┐
                    │  spaghetti.db    │  ← single SQLite file, WAL mode
                    │  (existing file) │    Rust writes, Node reads
                    └──────────────────┘
```

**Single-writer invariant**: Rust owns the write connection end-to-end for the duration of an ingest. Node opens read-only connections. WAL mode allows concurrent reads during ingest.

---

## Repo Layout

Layout follows the pattern already proven in `/Users/jamesyong/Projects/project100/p008/truffle`: a Cargo workspace rooted at the repo, Rust crates under `crates/`, TS packages under `packages/`. The napi crate **is** the npm package — no separate "wrapper" Rust crate.

```
spaghetti/
├── Cargo.toml                    # workspace root (new)
├── Cargo.lock                    # committed
├── rust-toolchain.toml           # pins stable toolchain (new)
├── rustfmt.toml                  # (new)
├── deny.toml                     # cargo-deny config for license/security (new)
├── crates/
│   └── spaghetti-napi/           # Cargo crate + npm package (@vibecook/spaghetti-sdk-native)
│       ├── Cargo.toml            # publish = false; crate-type = ["cdylib"]
│       ├── package.json          # napi.binaryName = "spaghetti"; targets = [...]
│       ├── build.rs              # napi_build::setup()
│       ├── index.js              # generated by napi build
│       ├── index.d.ts            # generated by napi build
│       ├── src/
│       │   ├── lib.rs            # NAPI exports
│       │   ├── ingest.rs         # top-level orchestration
│       │   ├── project_parser.rs # per-project streaming parse
│       │   ├── jsonl_reader.rs   # buffer + newline scanner
│       │   ├── parse_sink.rs     # IngestEvent enum (crossbeam messages)
│       │   ├── writer.rs         # single-thread SQLite writer
│       │   ├── schema.rs         # migration + PRAGMA setup
│       │   ├── fingerprint.rs    # mtime/size diff, warm-start logic
│       │   ├── types/            # Rust mirrors of SessionMessage, etc.
│       │   └── util/
│       │       ├── fts_text.rs   # truncate(), extractMessageText() port
│       │       └── error.rs
│       ├── __test__/             # napi-rs convention: TS integration tests
│       │   └── ingest.spec.ts
│       └── npm/                  # platform stub packages (one per target)
│           ├── darwin-arm64/package.json
│           ├── darwin-x64/package.json
│           ├── linux-x64-gnu/package.json
│           ├── linux-arm64-gnu/package.json
│           └── win32-x64-msvc/package.json
├── packages/
│   ├── sdk/                      # @vibecook/spaghetti-sdk — depends on sdk-native via workspace:*
│   ├── cli/
│   ├── ui/
│   └── ...
```

### Workspace `Cargo.toml` (repo root)

Central versioning so the sole crate and any future additions share metadata (mirrors truffle's workspace setup):

```toml
[workspace]
resolver = "2"
members = ["crates/spaghetti-napi"]

[workspace.package]
edition = "2021"
license = "MIT"
repository = "https://github.com/jamesyong/spaghetti"
rust-version = "1.83"

[workspace.lints.rust]
unsafe_code = "warn"

[workspace.lints.clippy]
all = { level = "warn", priority = -1 }

[workspace.dependencies]
napi        = { version = "3", default-features = false, features = ["napi8", "async", "serde-json"] }
napi-derive = "3"
napi-build  = "2"
sonic-rs    = "0.3"
rusqlite    = { version = "0.31", features = ["bundled", "trace"] }
crossbeam-channel = "0.5"
rayon       = "1.10"
serde       = { version = "1", features = ["derive"] }
serde_json  = "1"
thiserror   = "2"
anyhow      = "1"
tracing     = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
walkdir     = "2"
regex       = "1"
once_cell   = "1"
memmap2     = { version = "0.9", optional = true }

[profile.release]
lto           = "fat"
codegen-units = 1
strip         = "symbols"

[profile.dev.package."*"]
opt-level = 2
```

### Crate `crates/spaghetti-napi/Cargo.toml`

```toml
[package]
name = "spaghetti-napi"
version = "0.6.0"                            # tracks @vibecook/spaghetti-sdk version
edition.workspace = true
license.workspace = true
repository.workspace = true
publish = false                              # npm only, not crates.io

[lib]
crate-type = ["cdylib"]

[lints]
workspace = true

[dependencies]
napi.workspace = true
napi-derive.workspace = true
sonic-rs.workspace = true
rusqlite.workspace = true
crossbeam-channel.workspace = true
rayon.workspace = true
serde.workspace = true
serde_json.workspace = true
thiserror.workspace = true
anyhow.workspace = true
tracing.workspace = true
tracing-subscriber.workspace = true
walkdir.workspace = true
regex.workspace = true
once_cell.workspace = true

[build-dependencies]
napi-build.workspace = true
```

### Crate `crates/spaghetti-napi/package.json`

```json
{
  "name": "@vibecook/spaghetti-sdk-native",
  "version": "0.6.0",
  "main": "index.js",
  "types": "index.d.ts",
  "license": "MIT",
  "repository": {
    "type": "git",
    "url": "https://github.com/jamesyong/spaghetti.git",
    "directory": "crates/spaghetti-napi"
  },
  "engines": { "node": ">=18" },
  "publishConfig": { "access": "public" },
  "sideEffects": false,
  "files": ["index.js", "index.d.ts", "*.node"],
  "napi": {
    "binaryName": "spaghetti",
    "targets": [
      "x86_64-apple-darwin",
      "aarch64-apple-darwin",
      "x86_64-unknown-linux-gnu",
      "aarch64-unknown-linux-gnu",
      "x86_64-pc-windows-msvc"
    ]
  },
  "scripts": {
    "build": "napi build --platform --release",
    "build:debug": "napi build --platform",
    "prepublishOnly": "napi prepublish -t npm",
    "artifacts": "napi artifacts"
  },
  "devDependencies": {
    "@napi-rs/cli": "^3.0.0"
  }
}
```

### Platform stub `crates/spaghetti-napi/npm/darwin-arm64/package.json`

One of five. `release-please` keeps versions in sync across all of them.

```json
{
  "name": "@vibecook/spaghetti-sdk-native-darwin-arm64",
  "version": "0.6.0",
  "cpu": ["arm64"],
  "os": ["darwin"],
  "main": "spaghetti.darwin-arm64.node",
  "files": ["spaghetti.darwin-arm64.node"],
  "license": "MIT",
  "engines": { "node": ">=18" },
  "publishConfig": { "access": "public" },
  "repository": {
    "type": "git",
    "url": "https://github.com/jamesyong/spaghetti.git",
    "directory": "crates/spaghetti-napi"
  }
}
```

---

## Public API (NAPI Surface)

The addon exposes a single ingest entry point plus a version accessor. That's it.

**`@vibecook/spaghetti-sdk-native` is an internal dependency of `@vibecook/spaghetti-sdk`. It is not part of the public SDK API and is not expected to be imported by consumers.** The only user-facing shape of ingest is `AppService.rebuildIndex()` (see below).

```typescript
// packages/sdk-native/index.d.ts (generated by napi-rs)

export interface IngestOptions {
  rootDir: string; // was claudeDir
  dbPath: string;
  mode: 'cold' | 'warm';
  progressIntervalMs?: number;   // default 100
  parallelism?: number;          // default: num_cpus, capped at 8
}

export interface IngestProgress {
  phase: 'scanning' | 'parsing' | 'writing' | 'finalizing';
  projectsDone: number;
  projectsTotal: number;
  messagesWritten: number;
  elapsedMs: number;
}

export interface IngestStats {
  durationMs: number;
  projectsProcessed: number;
  sessionsProcessed: number;
  messagesWritten: number;
  subagentsWritten: number;
  bytesRead: number;
  errors: IngestError[];
}

export interface IngestError {
  path: string;
  lineIndex?: number;
  message: string;
}

export function ingest(
  opts: IngestOptions,
  onProgress?: (p: IngestProgress) => void,
): Promise<IngestStats>;

export function nativeVersion(): string;
```

### SDK-level wrapper (public)

The only user-facing ingest API lives on `AppService`:

```typescript
interface SpaghettiAPI {
  // ...existing methods unchanged...

  /** Force a full cold-start reindex. Discards and rebuilds the SQLite file. */
  rebuildIndex(): Promise<{ durationMs: number }>;
}
```

`rebuildIndex()` internally calls `native.ingest({mode: 'cold', ...})` with known-safe options. Consumers never see `IngestOptions` or the native module.

**Key design rules for the boundary**:

1. **No per-message callbacks.** Progress is throttled to `progressIntervalMs` (default 100ms). The JS callback is never invoked more than 10×/sec regardless of message volume.
2. **Progress payload is a fixed-shape struct**, not a free-form object. napi-rs generates zero-allocation bindings for known shapes.
3. **Errors are collected**, not streamed. Fatal errors reject the promise; parse errors accumulate and return in `IngestStats.errors`.
4. **No SQLite handle crosses the boundary.** Rust opens and closes its own connection. Node opens its own read-only connection after ingest completes.
5. **No shared buffers, no `ArrayBuffer` hand-off.** Everything stays in Rust until persisted.

### Observability / tracing

Rust emits structured `tracing` events throughout ingest (per-project spans, per-phase events, error details). These are **gated behind the `SPAG_DEBUG=1` environment variable**:

- **Off by default** (production): zero runtime cost. `tracing-subscriber` is installed with a level filter that drops everything.
- **On**: writes JSON Lines to `~/.spaghetti/logs/ingest-<iso-timestamp>.jsonl`. Log rotation keeps the last 5 runs; older files are deleted on startup.

The log file path is not configurable via `IngestOptions` — it's a debug affordance, not part of the contract. Channel-plugin live-observability integration (if ever needed) is deferred to a separate RFC; consumers can tail the log file in the meantime.

---

## Data Flow

### Cold start

```
1. Node: appService.initialize()
2. Node: dataService.coldStartParallel()
3. Node: calls native.ingest({mode: 'cold', ...})
4. Rust:
   a. Open SQLite with write connection, WAL, set PRAGMAs
   b. initializeSchema() — run migrations
   c. walkdir over ~/.claude/projects/*, collect slugs
   d. rayon::par_iter over slugs:
        - Per-worker ProjectParser reads JSONL files
        - sonic-rs parses each line into SessionMessage
        - Worker pushes IngestEvent into crossbeam channel
   e. Single writer thread drains channel:
        - Begins transaction
        - Batches INSERTs via prepared statements
        - Commits every N events or at project boundary
   f. Finalize: upsert source_files fingerprints, ANALYZE
   g. Return IngestStats, close connection
5. Node: opens read-only connection, emits 'ready'
```

### Warm start

```
1. Node: appService.initialize()
2. Node: calls native.ingest({mode: 'warm', ...})
3. Rust:
   a. Open SQLite write connection
   b. Read all source_files fingerprints into a HashMap
   c. walkdir ~/.claude, stat each file, diff against fingerprints
   d. Build change set: {added, modified (with byte offsets), deleted}
   e. If empty → close, return stats (path: ~20–50ms)
   f. Else → incremental ingest of changed files only
      - Modified files: read from stored byte_position forward
      - Single writer thread, same path as cold start
   g. Return IngestStats
```

The warm-start empty-change path is the one that must be very fast. Target: **<30ms** including NAPI call overhead.

---

## IngestEvent Enum (internal)

Workers and writer communicate via a single enum pushed over `crossbeam-channel`. This replaces the current `ProjectParseSink` interface.

```rust
pub enum IngestEvent {
    Project {
        slug: String,
        original_path: String,
        sessions_index_json: String,
    },
    ProjectMemory { slug: String, content: String },
    Session {
        slug: String,
        session_id: String,
        entry: SessionIndexEntry,
    },
    Message {
        slug: String,
        session_id: String,
        index: u32,
        byte_offset: u64,
        raw_json: String,          // pre-serialized, written as-is
        fts_text: Option<String>,
    },
    Subagent {
        slug: String,
        session_id: String,
        transcript: SubagentTranscript,
    },
    ToolResult {
        slug: String,
        session_id: String,
        tool_use_id: String,
        content: String,
    },
    FileHistory { session_id: String, history: FileHistorySession },
    Todo { session_id: String, todo: TodoFile },
    Task { session_id: String, task: TaskEntry },
    Plan { slug: String, plan: PlanFile },
    SessionComplete {
        slug: String,
        session_id: String,
        message_count: u32,
        last_byte_position: u64,
    },
    ProjectComplete { slug: String, duration_ms: u32 },
    WorkerError { slug: String, error: String },
}
```

Unlike the current TS pipeline, **`raw_json` is not re-parsed** — the worker validates the shape via sonic-rs, extracts what it needs for fts_text, and stores the original JSON bytes for the `messages.data` column. No round-trip through a structured type.

---

## Schema & Migration Ownership

**Rust owns migrations.** The writer, on open, runs `schema::initialize()`, which is a direct port of `packages/sdk/src/data/schema.ts`. The `schema_meta` table's `schema_version` key is the source of truth.

Node's read-only connection does a version check on open and refuses to operate if the version is higher than the TS side expects. This guards against the case where the user downgrades `@vibecook/spaghetti-sdk` but keeps a newer DB.

New schema changes during the transition must be authored in both places until the TS ingest is deleted. Post-cutover, only Rust migrations matter.

---

## Migration Path (Phased)

The goal is to land Rust without breaking anyone mid-flight. Four phases.

### Phase 0: Skeleton & CI (week 1)

The goal of Phase 0 is **a scaffolded repo that can publish a no-op native addon to npm end-to-end**, so that every subsequent phase is pure feature work. The patterns are already proven in truffle; copy, don't invent.

Concrete checklist (rough order):

1. **Repo init**: `rust-toolchain.toml`, `rustfmt.toml`, `deny.toml`, workspace `Cargo.toml`. Copy-adapt from `truffle/` root.
2. **Crate scaffold**: `crates/spaghetti-napi/` — `Cargo.toml`, `package.json`, `build.rs`, `src/lib.rs`. Take `truffle/crates/truffle-napi/` as the starting template; rename `truffle` → `spaghetti` throughout.
3. **Platform stubs**: 5 × `crates/spaghetti-napi/npm/<target>/package.json`. Copy from `truffle/crates/truffle-napi/npm/`, swap names.
4. **No-op NAPI export**: one function `nativeVersion(): string` returning `env!("CARGO_PKG_VERSION")`. Enough to verify the binary loads.
5. **release-please config**: `release-please-config.json` + `.release-please-manifest.json`. Copy from truffle, update paths and drop entries for packages we don't have yet (react, sidecar, CLI stay out of scope).
6. **CI workflows** (drop into `.github/workflows/`):
   - `release-please.yml` — from truffle, drop dispatches we don't need (release-cli, release-sidecar, publish-crates)
   - `napi-build.yml` — from truffle, change paths `crates/truffle-napi` → `crates/spaghetti-napi` and names `truffle` → `spaghetti`
   - `release-npm.yml` — from truffle, adapt package names to `@vibecook/spaghetti-sdk`
   - `ci.yml` — new: runs `cargo test`, `cargo clippy --deny warnings`, `pnpm test` on PRs
   - Pin all action SHAs to match truffle's versions
7. **Consume from SDK**: add `"@vibecook/spaghetti-sdk-native": "workspace:*"` to `packages/sdk/package.json`. Create a thin `packages/sdk/src/native.ts` that dynamically imports `@vibecook/spaghetti-sdk-native` and exposes a typed wrapper; falls back to `null` if not installed.
8. **Feature flag**: SDK reads `SPAG_NATIVE_INGEST` env var. Default `0` (pure TS). When `1`, calls `native.ingest()` if available, else logs a warning and falls back.
9. **npm org / OIDC**: `@vibecook` org already exists (confirmed via `@vibecook/spaghetti-sdk`). Configure OIDC trusted publisher for the spaghetti repo in npm settings, one-time, to enable `--provenance` publishes without NPM_TOKEN.
10. **Publish dry-run**: cut `0.6.0-rc.0` off a branch, verify all 6 npm packages publish, the wrapper `@vibecook/spaghetti-sdk` can be installed cleanly in a scratch project, and `require('@vibecook/spaghetti-sdk-native').nativeVersion()` returns the expected string.

**Done when**: a new tag publishes all 6 packages; `npm install @vibecook/spaghetti-sdk@<rc>` in a blank project works on macOS arm64; `SPAG_NATIVE_INGEST=1` has no observable effect yet (native path is empty), TS path still runs.

### Phase 1: Cold start parity (weeks 2–4)

- Port `streaming-jsonl-reader.ts` → `jsonl_reader.rs`
- Port `project-parser.ts` → `project_parser.rs` (no plan-index caching trick yet; do simplest thing first)
- Port `ingest-service.ts` write path → `writer.rs`
- FTS text extraction → `util/fts_text.rs`
- No rayon yet; single-threaded. Goal is *correctness*, not speed.
- **Gate**: cold-start ingest on a 500MB fixture produces a bit-identical SQLite file to the TS path (excluding `updated_at` timestamps).

### Phase 2: Parallelism + perf (week 5)

- Introduce `rayon::par_iter` over projects
- Tune channel capacity, batch sizes, PRAGMAs
- Benchmark against TS on the 500MB fixture; target ≤1s
- Profile with `samply` + `cargo flamegraph`

### Phase 3: Warm start + fingerprint (week 6)

- Port fingerprint diff logic from `ingest-service.ts`
- Incremental file reads from stored byte offsets
- **Gate**: warm start with 0 changes <30ms on the fixture

### Phase 4: Cutover (week 7)

- Flip `SPAG_NATIVE_INGEST` default to `1`
- Ship in next minor release with migration notes
- Keep TS path in the codebase for one release cycle as fallback (`SPAG_NATIVE_INGEST=0`)
- Release `0.7.0` with "now powered by Rust"

### Phase 5: Cleanup (one release later)

- Delete TS ingest path: `workers/`, `parser/project-parser.ts`, `io/streaming-jsonl-reader.ts`, `data/ingest-service.ts`
- Trim `@vibecook/spaghetti-sdk` bundle size — worker_threads code goes
- Update docs

**Total: ~6–7 weeks at one full-time engineer's pace.** Realistic with some Rust learning overhead: 8–10 weeks.

---

## Correctness Verification

Running two ingest implementations side-by-side requires a rigorous diff strategy.

### Test fixtures

Commit three fixtures to `packages/sdk-native/fixtures/`:
1. `small/` — 3 projects, 20 sessions, ~5MB (unit test speed)
2. `medium/` — 50 projects, ~50MB (PR CI)
3. `large/` — snapshot of a real `~/.claude` at 500MB, gitignored, fetched from a bucket (release CI only)

### Diff harness

```typescript
// scripts/ingest-diff.ts
async function diffIngest(fixture: string) {
  const tsDb = await runTsIngest(fixture);
  const rustDb = await runNativeIngest(fixture);

  const tsRows = dumpAllTables(tsDb);
  const rustRows = dumpAllTables(rustDb);

  return deepDiff(tsRows, rustRows, {
    ignore: ['updated_at', 'source_files.mtime_ms'],
  });
}
```

CI fails the PR if Phase 1/2/3 produces any non-ignored row diff on the small or medium fixture.

### Canary rollout

After cutover, include a `SPAG_INGEST_SHADOW=1` mode for one release: runs both, compares row counts, logs any mismatches. Removed in 0.8.

---

## Distribution

This section is modeled directly on the working truffle release pipeline (`/Users/jamesyong/Projects/project100/p008/truffle`) — a sibling project that ships a Rust-backed npm package using the same conventions. Paths below reference the truffle files that serve as templates.

### Package topology

- **`@vibecook/spaghetti-sdk-native`** — the napi crate's npm face. `optionalDependencies` to five platform packages. Consumed by `@vibecook/spaghetti-sdk` as a dependency.
- **`@vibecook/spaghetti-sdk-native-<platform>`** — binary-only stub packages, one per target. `os` + `cpu` fields restrict install.
- **`@vibecook/spaghetti-sdk`** — pure-TS wrapper. In the monorepo, depends on `@vibecook/spaghetti-sdk-native` via `workspace:*`. At publish time, `pnpm publish` rewrites this to a concrete version range.

### CI workflow topology (5 files in `.github/workflows/`)

Directly adapted from truffle:

| File | Trigger | What it does |
|---|---|---|
| `release-please.yml` | push to main | Creates release PR. On release, dispatches the other workflows via `gh workflow run` (GITHUB_TOKEN releases don't trigger `release: created` events — hence the explicit dispatch). Template: `truffle/.github/workflows/release-please.yml`. |
| `napi-build.yml` | workflow_dispatch (from release-please) | 5-target build matrix → test matrix on 3 hosts → publish platform packages → publish main `@vibecook/spaghetti-sdk-native` package. OIDC trusted publishing (`id-token: write`) + `--provenance`. Template: `truffle/.github/workflows/napi-build.yml`. |
| `release-npm.yml` | workflow_dispatch | Waits (up to 15 min, 30 × 30s) for `@vibecook/spaghetti-sdk-native@X.Y.Z` to appear on npm, then `pnpm publish` the TS wrapper (so `workspace:*` gets rewritten). Template: `truffle/.github/workflows/release-npm.yml`. |
| `ci.yml` | PR + push | Build + test TS packages; cargo test + clippy on the napi crate. |
| `security-audit.yml` | schedule + PR | `cargo audit`, `cargo deny`, `pnpm audit`. |

### Release orchestration (the sequencing that matters)

```
release-please creates PR → merge
    │
    ▼
release-please.yml detects release_created
    │
    ├──► napi-build.yml
    │        ├─ build (5 targets in parallel)
    │        ├─ test bindings (3 hosts)
    │        └─ publish
    │            ├─ platform packages (5 × npm publish --provenance)
    │            └─ @vibecook/spaghetti-sdk-native (main)
    │
    └──► release-npm.yml (dispatched concurrently)
             ├─ wait-for-npm loop on @vibecook/spaghetti-sdk-native@X.Y.Z
             └─ pnpm publish @vibecook/spaghetti-sdk (workspace:* → ^X.Y.Z)
```

The wait-for-npm step is critical — npm registry indexing can take several minutes after publish, and `pnpm install` on a downstream consumer will fail if the native package hasn't propagated.

### release-please config (`release-please-config.json`)

Uses `extra-files` to bump versions atomically across: the napi `package.json`, all 5 platform stubs, the SDK's `package.json`, and any other published TS packages. Template: `truffle/release-please-config.json`. Relevant entries:

```json
{
  "packages": {
    ".": {
      "release-type": "simple",
      "extra-files": [
        { "type": "json", "path": "crates/spaghetti-napi/package.json", "jsonpath": "$.version" },
        { "type": "json", "path": "crates/spaghetti-napi/npm/darwin-arm64/package.json", "jsonpath": "$.version" },
        { "type": "json", "path": "crates/spaghetti-napi/npm/darwin-x64/package.json", "jsonpath": "$.version" },
        { "type": "json", "path": "crates/spaghetti-napi/npm/linux-arm64-gnu/package.json", "jsonpath": "$.version" },
        { "type": "json", "path": "crates/spaghetti-napi/npm/linux-x64-gnu/package.json", "jsonpath": "$.version" },
        { "type": "json", "path": "crates/spaghetti-napi/npm/win32-x64-msvc/package.json", "jsonpath": "$.version" },
        { "type": "generic", "path": "crates/spaghetti-napi/Cargo.toml" }
      ]
    }
  }
}
```

### Gotchas already solved in truffle — pre-borrow the solutions

1. **`setup-node@v5` fails in post-step** if `packageManager: pnpm@X` is in root package.json but pnpm isn't on PATH. Fix: `npm install -g pnpm@10.20.0` before `setup-node`, and set `package-manager-cache: false` on jobs that use `npm install` (not pnpm). See `napi-build.yml` L64–70.
2. **`napi prepublish` breaks OIDC.** Its spawned `npm publish` children don't inherit the workflow's OIDC context → no provenance attestation. Fix: manually sync platform package versions and publish each one individually. See `napi-build.yml` L183–205.
3. **Must use `pnpm publish`, not `npm publish`**, for the TS wrapper. `npm publish` leaves `workspace:*` literal → package uninstallable outside the monorepo. `pnpm publish` rewrites to a concrete range. See `release-npm.yml` L65–73.
4. **Cross-compilation to aarch64-linux** needs `gcc-aarch64-linux-gnu` + the `CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER` env var. See `napi-build.yml` L77–91.
5. **macOS deployment target**: `MACOSX_DEPLOYMENT_TARGET: '10.13'` at workflow level to avoid version-skew warnings. See `napi-build.yml` L5.
6. **Pinned action SHAs**: truffle pins every GitHub Action to a commit SHA (Dependabot-managed), not `@v5`. Adopt the same policy.

### Versioning: lock-step

`@vibecook/spaghetti-sdk-native` is versioned **in lock-step with `@vibecook/spaghetti-sdk`** — every SDK release bumps both, and `sdk` pins `sdk-native` at an exact version. Rationale:

- One mental model: "what version are you on?" has one answer
- Zero mismatch risk; no peer-dependency range gymnastics
- Schema changes always couple sdk ↔ sdk-native anyway; "independent" versioning would largely be fiction
- CI cost of republishing prebuilds per SDK patch (~15 min, ~18 MB) is acceptable at current release cadence
- This is exactly what truffle does today — `release-please` bumps all versions atomically via `extra-files`

Revisit at 1.0 if CI time becomes a bottleneck. Escape hatch is straightforward: relax the exact pin to `^` and publish native only when it changes.

### Binary size budget

| Target | Estimated size |
|---|---:|
| darwin-arm64 | ~3 MB |
| darwin-x64 | ~3 MB |
| linux-x64-gnu | ~4 MB (glibc) |
| linux-arm64-gnu | ~4 MB |
| win32-x64-msvc | ~4 MB |
| **Total published** | ~18 MB across 5 packages |
| **User download** | ~3–4 MB (only their platform) |

Compared to the current `better-sqlite3` footprint (~5MB prebuild), this is in-line.

---

## Risks & Open Questions

### R1: napi-rs + better-sqlite3 opening the same DB file

Both use SQLite's C library via their own bindings. Shouldn't conflict under WAL, but must verify:
- No lock file leaks when Rust panics mid-write
- Schema version check on Node's read-only open refuses to run if `PRAGMA user_version` is newer than it knows

**Mitigation**: integration test that kills the Rust process mid-ingest, then opens from Node and verifies WAL recovery works.

### R2: sonic-rs on macOS

sonic-rs uses SIMD intrinsics; on arm64 it uses NEON, on x86 AVX2. Occasionally fails on older CPUs. Fallback to `serde_json` via a cargo feature flag.

**Mitigation**: `default = ["sonic"]`, but ship a `serde_json` fallback build for users on old hardware. Detect at runtime if needed.

### R3: Error taxonomy mismatch

TS throws `Error` with string messages. Rust's `thiserror` types don't cross NAPI cleanly. Need a stable error-code enum shared via the TS `.d.ts`.

**Mitigation**: define `IngestErrorCode` in `types/error.rs`, mirror in TS, map at the boundary.

### R4: React DevTools, TUI debugging

The TS path emits events that the channel plugins forward to the TUI for live progress. Progress callbacks must reach the same codepath. Verify in Phase 2.

### R5: Rust learning curve

If James is primary author and not fluent in Rust, weeks 1–3 will be slower than estimated. A pair-programming pass through the parser port would de-risk this.

**Mitigation**: start with the smallest port (`jsonl_reader.rs`, ~150 lines) as a warm-up before touching the worker pool.

### R6: Keeping two implementations in sync during Phases 1–4

Any schema change, any new field in `SessionMessage`, any new tool name — must be added to both. High friction.

**Mitigation**: the phase is short (6 weeks). Freeze non-critical schema changes during this window. Critical changes get authored in both languages in the same PR.

### Resolved decisions

1. **`native.ingest()` is internal**, not part of any public SDK surface. User-facing ingest control is `AppService.rebuildIndex()` only. Can be exposed later if demand materializes.
2. **Tracing output**: Rust emits `tracing` events gated by `SPAG_DEBUG=1`, writing JSON Lines to `~/.spaghetti/logs/ingest-<timestamp>.jsonl` (last 5 runs retained). No channel-plugin live-observability integration in this RFC; deferred to a follow-up if needed.
3. **Versioning**: lock-step with `@vibecook/spaghetti-sdk` via exact-version pin. Revisit at 1.0.

---

## Success Criteria

Shipping 0.7.0 with the Rust ingest core is successful if all of the following hold on a 500MB fixture:

- [ ] Cold start (empty DB) completes in **≤800ms** (today: 1.5–3s)
- [ ] Warm start with 0 changes completes in **≤40ms** (today: 50–200ms)
- [ ] Warm start with 5 modified files completes in **≤150ms** (today: 200–500ms)
- [ ] Bit-identical SQLite output vs TS ingest (ignoring `updated_at`) on the small + medium fixtures
- [ ] No regressions in any existing SDK consumer (CLI, TUI, playground, channel plugins)
- [ ] `@vibecook/spaghetti-sdk` still installs and runs on platforms without a prebuild, via TS fallback
- [ ] Binary size per platform ≤6MB
- [ ] CI build time for all 5 targets ≤15 minutes

---

## Appendix A: Why sonic-rs over simd-json

| | sonic-rs | simd-json |
|---|---|---|
| License | Apache-2.0 | Apache-2.0 |
| Small-doc perf (our JSONL lines) | faster | slower |
| Lazy field access | ✓ (big win — we skip ~half the fields per line) | no |
| Input buffer requirements | immutable, any bytes | requires mutable, padded |
| API ergonomics | serde-compatible | custom |
| Platform support | x86_64, arm64 | x86_64, arm64 |

sonic-rs's lazy API lets us skip parsing `toolUseResult.content` (which we truncate for FTS anyway) and `message.content` array items we don't care about. Estimated 20–30% faster on real JSONL lines than simd-json's greedy parse.

## Appendix B: Why not tokio

Ingest is CPU-bound: parsing JSON, inserting into SQLite. File reads are fast (page cache) and sequential. Async's value is multiplexing many slow I/O operations; we have few, fast ones. `rayon` over `par_iter` is the right primitive — it maps 1:1 to "parse N projects in parallel", which is already how the TS worker pool is structured.

Adding tokio would mean an executor, `async fn` everywhere, Pin<Box<dyn Future>> at the NAPI boundary, and ~300kb of binary bloat for no measurable win.

## Appendix C: Why not replace better-sqlite3 on the read side

The query path is already fast (~2–10ms per query). Native bindings to the same SQLite engine won't make it faster in any way users perceive. Replacing better-sqlite3 would:
- Double the surface area of this RFC
- Require re-implementing every `prepare()` and `all()/get()` call in `agent-data-service.ts`
- Risk introducing subtle semantic differences in how rows are shaped for consumers

Keep it. Revisit only if a specific query is identified as a bottleneck.

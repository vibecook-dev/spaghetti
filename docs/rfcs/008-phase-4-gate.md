# RFC 008 Phase 4 — Performance and Platform Readiness Exit Gate

**Companion to:** [RFC 008](./008-rust-ingest-production-readiness.md) · [Phase 4A decision](./008-phase-4a-warm-strategy.md) · [Supported platforms](../SUPPORTED-PLATFORMS.md)
**Captured:** 2026-08-09
**Status:** Phase 4 exit gate met.

Two independent halves: a warm-strategy decision that needed a measurement
nobody had taken, and a platform surface with two gaps that both looked like
"unsupported platform" from the outside.

---

## 1. Warm strategy — accepted, no incremental work

Decision **P1** deferred the choice pending measurement. Measured on a
1,404-session / 44 MB corpus:

| Scenario      | Rust   | TS     | Threshold | Margin     |
| ------------- | ------ | ------ | --------- | ---------- |
| Unchanged     | 60 ms  | 427 ms | 3 s       | 50× under  |
| Growth        | 2.61 s | 6.65 s | 13.3 s    | 5.1× under |
| Deletion      | 2.59 s | 6.63 s | 13.3 s    | 5.1× under |
| Forced repair | 2.57 s | n/a    | —         | —          |

**Rust's full-source rebuild is 2.5× faster than TS's incremental path.** The
optimisation held in reserve would have been optimising something already
faster than its own comparison target, while adding exactly the per-project
incremental state Phases 1 and 2 showed to be where correctness risk lives.

Full samples, hardware, and what would reopen the decision are in
[`008-phase-4a-warm-strategy.md`](./008-phase-4a-warm-strategy.md).

### The comparison had to be built

Phase 0 recorded this comparison as unmeasured: the harness had no TS warm
mode, because `runTsOnce` cleaned the database every iteration and so measured
a cold start. `--mode warm` was gated to `--only rust` for that reason.

`--scenario` was added alongside, because "warm" alone conflates the fast path
with a full rebuild — numbers two orders of magnitude apart answering different
questions. Mutating scenarios copy the fixture first, so a benchmark can never
leave the committed fixture or a real `~/.claude` modified.

**The repair scenario needed a second pass**, and the way it failed is worth
recording: its first version wrote `WHERE materialization =` — not a column —
inside a `try/catch`, so the UPDATE errored, the scenario silently did nothing,
and the run reported 52 ms fast-path timings as though they were a forced
rebuild. It now throws when no row is invalidated. A scenario that quietly
fails to apply is worse than one that crashes.

---

## 2. Platform coverage

### musl

Alpine is x64 Linux with no glibc. With only GNU artifacts published it fell
back to the TypeScript engine silently — indistinguishable from an unsupported
platform. `x86_64-unknown-linux-musl` and `aarch64-unknown-linux-musl` now
ship, and are load-tested **inside `node:24-alpine`**, not on the Ubuntu host:
a glibc host proves nothing about a musl build.

**Verified by dispatching the release matrix build-only** (`publish=false`),
because that workflow does not run on ordinary PRs — it is gated to
release-please PRs and tags, so the musl work would otherwise have shipped
unexercised. All 8 targets build and all 8 load-test green.

That verification was worth the cycles: the first three attempts failed, each
for a different reason.

| Attempt                    | Failure                                                                                                                                                            |
| -------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `musl-tools` cross-compile | `musl-gcc` cannot link a cdylib — it passes `-lgcc_s` and musl has no such library                                                                                 |
| musl.cc cross toolchain    | the host was unreachable, putting a third party in the release path                                                                                                |
| job-level `container:`     | GitHub supports JavaScript actions in Alpine containers only on x64, so `actions/checkout` fails on ARM; and the napi-rs image's Node was too old for the napi CLI |

The working shape is an explicit `docker run` with checkout and caching on the
host, so no JavaScript action executes inside the container and the image is
chosen freely. The last fix was one line: an explicit `--target` makes Rust
look for the cross-named linker `aarch64-linux-musl-gcc` even when the host
already _is_ that target, and Alpine ships it as plain `gcc`.

### A pinned glibc baseline

The build host's glibc becomes the artifact's floor, so `ubuntu-latest` moves
the requirement whenever GitHub moves the label. The GNU builds are pinned to
`ubuntu-22.04`, setting the documented minimum at **glibc 2.35** — Ubuntu
22.04+, Debian 12+, RHEL 9+, Fedora 36+. `ubuntu-latest` is currently 24.04 and
would demand 2.39, excluding Ubuntu 22.04 LTS and Debian 12.

If that runner is retired the build fails loudly rather than quietly shipping a
narrower artifact.

### The fallback is no longer silent

`loadNativeAddon` swallowed the require failure with a bare `catch`, so three
different situations were indistinguishable — the addon missing, the platform
having no prebuilt binary, and the binary being present but unloadable. The
engine became `ts` and nothing said why.

`EngineUnavailableError` carries platform, architecture, libc, the expected
addon version, and an install hint. libc comes from `process.report`'s
`glibcVersionRuntime` — present on glibc, absent on musl — and matters because
it separates "unsupported platform" from "wrong artifact for a supported one".

`createSpaghetti` reports it to the error sink when `rs` was requested and did
not load. The run still succeeds and produces the same index, so this is a
diagnostic, not a thrown error. `resolveActiveEngine()` already reported the
engine that actually ran; that stays the source of truth for any UI.

---

## 3. Exit gate

| Gate                                                               | Status                                                                                                                           |
| ------------------------------------------------------------------ | -------------------------------------------------------------------------------------------------------------------------------- |
| Warm strategy decision and benchmark results committed to this RFC | ✅ §1, decision recorded inline in RFC 008 with full samples in the companion                                                    |
| All advertised artifacts load on their target smoke environment    | ✅ eight targets, eight load tests; musl inside Alpine, Windows-on-ARM on `windows-11-arm`, darwin-x64 under Rosetta             |
| Supported-platform documentation matches published packages        | ✅ [`SUPPORTED-PLATFORMS.md`](../SUPPORTED-PLATFORMS.md), reconciled against the workflow matrix rather than written from memory |
| Missing-addon behavior is loud and actionable                      | ✅ §2, `EngineUnavailableError` with platform/arch/libc/version/hint, routed to the error sink                                   |

**Performance work weakened no correctness test.** Nothing in the ingest path
changed for this phase — the benchmark harness, the release matrix, and the
loader diagnostics did.

**Phase 5 may begin:** dual-engine soak, keeping the TypeScript engine
selectable while the Rust behaviour ships, and the readiness report that hands
off to RFC 009.

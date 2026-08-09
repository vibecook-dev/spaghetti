# Supported platforms

The native ingest addon (`@vibecook/spaghetti-sdk-native`) ships prebuilt
binaries. This page lists exactly what is published and what each artifact
requires — RFC 008 Phase 4 requires it to match the release matrix, so it is
generated from the same target list in
[`.github/workflows/napi-build.yml`](../.github/workflows/napi-build.yml) and
`crates/spaghetti-napi/package.json`.

## Published artifacts

| Platform | Arch  | libc         | Target triple                | Built on                | Load-tested on                         |
| -------- | ----- | ------------ | ---------------------------- | ----------------------- | -------------------------------------- |
| macOS    | arm64 | —            | `aarch64-apple-darwin`       | `macos-latest`          | `macos-latest`                         |
| macOS    | x64   | —            | `x86_64-apple-darwin`        | `macos-latest`          | `macos-latest` (Rosetta 2)             |
| Linux    | x64   | glibc ≥ 2.35 | `x86_64-unknown-linux-gnu`   | `ubuntu-22.04`          | `ubuntu-latest`                        |
| Linux    | arm64 | glibc ≥ 2.35 | `aarch64-unknown-linux-gnu`  | `ubuntu-22.04`          | `ubuntu-24.04-arm`                     |
| Linux    | x64   | musl         | `x86_64-unknown-linux-musl`  | `ubuntu-latest`         | `node:24-alpine` on `ubuntu-latest`    |
| Linux    | arm64 | musl         | `aarch64-unknown-linux-musl` | `ubuntu-latest` (cross) | `node:24-alpine` on `ubuntu-24.04-arm` |
| Windows  | x64   | —            | `x86_64-pc-windows-msvc`     | `windows-latest`        | `windows-latest`                       |
| Windows  | arm64 | —            | `aarch64-pc-windows-msvc`    | `windows-latest`        | `windows-11-arm`                       |

Every artifact is `require()`-loaded on a matching host before publish. A
binary that cannot load never ships.

## The glibc minimum is 2.35

The build host's glibc becomes the artifact's floor, so the GNU builds are
pinned to `ubuntu-22.04` (glibc 2.35) rather than `ubuntu-latest`. A floating
host silently raises the requirement the day GitHub moves the label —
`ubuntu-latest` is 24.04, which would demand glibc 2.39 and exclude Ubuntu
22.04 LTS and Debian 12.

2.35 covers Ubuntu 22.04+, Debian 12+, RHEL 9+, and Fedora 36+.

**Older or non-glibc systems use the musl build.** Alpine in particular is x64
Linux with no glibc at all; before the musl artifacts existed it silently fell
back to the TypeScript engine, which is indistinguishable from an unsupported
platform.

## When the addon does not load

Ingest falls back to the TypeScript engine. It is slower — see
[the Phase 4A measurements](./rfcs/008-phase-4a-warm-strategy.md) — but it
produces the same index, so this is a degradation, not a failure.

The fallback is **not silent**. The SDK reports an `EngineUnavailableError` to
its error sink naming the platform, architecture, libc, the addon version it
expected, and an install hint. `resolveActiveEngine()` returns the engine that
actually ran, not the one that was configured — use it for any UI that
displays the engine, because `resolveEngine()` reports only the preference and
reads `rs` even when the run fell back.

```ts
import { resolveActiveEngine, nativeLoadFailure } from '@vibecook/spaghetti-sdk';

const { engine, preference, nativeVersion } = resolveActiveEngine();
if (engine !== preference) {
  console.warn(nativeLoadFailure()?.message);
}
```

Automatic fallback is available for as long as the TypeScript engine ships.
RFC 009 removes it.

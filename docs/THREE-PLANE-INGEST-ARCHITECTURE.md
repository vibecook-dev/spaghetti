# Three-Plane Ingest Architecture (superseded)

**Superseded:** 2026-08-08 by [`TWO-PLANE-INGEST-ARCHITECTURE.md`](./TWO-PLANE-INGEST-ARCHITECTURE.md).

Plane 3 — process-adjacent runtime state from the `spaghetti-hooks` and
`spaghetti-channel` Claude Code plugins — was removed in
[RFC 007](./rfcs/007-retire-runtime-bridge.md). The runtime bridge never wrote to
the index, duplicated transcript content at lower latency, and carried two plugin
packages plus a WebSocket dependency for surfaces that went unused.

Spaghetti now has two ingest planes: static disk and live disk deltas. The index
is a pure function of files on disk.

This file remains so that dated RFCs and plan documents linking here keep
resolving. Its content lives at the two-plane path above; the three-plane
version is in git history at `211f4b1`.

# RFC 006 — Normalized Message Model (multi-agent, step 3/3)

**Status:** Proposed
**Created:** 2026-07-13
**Depends on:** step 1 (source dimension, schema v5 — shipped) · step 2 (classifier is a property of `AgentSource` — shipped)
**Companion:** `docs/TWO-PLANE-INGEST-ARCHITECTURE.md` §7 (longer-term multi-agent)
**Empirical grounding:** `docs/rfcs/006-appendix-agent-survey.md` — a five-agent survey (Codex, Gemini CLI, Grok Build, OpenCode, Cursor) that inspected real installs / primary source. It confirms the thin-core direction and forces the amendments folded into §3.1, §4, §6 and §8 below. Read it before implementing.

---

## 1. Why this RFC exists

Steps 1 and 2 gave the index a **source dimension** and moved **file classification** behind `AgentSource`. Two agents' files can now be stored side by side and routed by source-declared rules. What remains Claude-shaped is the thing those two steps deliberately did not touch: **how a raw transcript line becomes the extracted columns the product reads** (`msg_type`, `text_content`, the token counts). Today that extraction assumes Anthropic's message envelope and content-block kinds, in two engines.

This is the decision that sets the cost of every future adapter. Get the normalized/raw boundary right and a Codex or Gemini source is a small extractor. Get it wrong — over-normalize to Claude's shape, or under-normalize so the product can't read a second agent — and every adapter fights the schema. So this is an RFC, not a patch.

## 2. What is already generic (don't re-solve)

- **The `messages` table** stores the verbatim source line in `data TEXT` plus extracted columns (`msg_type`, `uuid`, `timestamp`, the four token columns, `text_content`, `byte_offset`). Storage is already format-agnostic — a raw line of any shape fits.
- **`source_id`** (step 1) tags every row, so extraction can dispatch on it.
- **`Category`** (step 2, `live/router.ts`) is already the normalized bucket every source maps into (`session`/`subagent`/`tool_result`/…).

The gap is not storage and not routing. It is **extraction**: the function `raw line → { msg_type, text_content, tokens }`, which lives inside `parser/project-parser.ts` (TS) and `project_parser.rs` / `parse_sink.rs` (Rust) and knows Claude's content blocks.

## 3. The decision: a thin normalized core over a raw passthrough

**Normalize the minimum the product actually queries; keep everything else raw.**

Two layers, explicit:

1. **Raw passthrough (unchanged).** `data TEXT` stays the verbatim source line. Any consumer needing full fidelity (rendering a specific agent's content blocks, tool-call structure, thinking blocks) parses `data` itself *knowing `source_id`*. Spaghetti does not normalize rich structure — that is where Claude-shape leaked in, and it is the leak we are stopping.

2. **Normalized core (formalized).** A small, agent-agnostic shape every source must produce per message, because the product's cross-agent surfaces (list, search/FTS, token stats, timelines) depend on exactly these and nothing more:

   ```ts
   interface NormalizedMessage {
     msgType: 'user' | 'assistant' | 'system' | 'summary' | 'other';
     text: string;          // FTS/preview text, already flattened — excludes reasoning + tool payloads
     seq: number;           // source-supplied monotonic ordinal — ordering does NOT rely on timestamp
     uuid?: string;
     timestamp?: string;    // optional: reliable per-message in only 4/6 surveyed agents (§4)
     tokens?: { input: number; output: number; cacheCreation: number; cacheRead: number };
     // raw native record (message + its parts) retained separately as `data`
   }
   ```

   `msgType` is a **normalized enum**, not Claude's 14-variant union — Claude's app-specific variants (`bridge-session`, `pr-link`, `custom-title`, `permission-mode`, …) collapse to `system`/`other` in the core and stay fully available in `data` for anyone who wants them. The survey confirms this is mandatory, not cosmetic: **0 of 6 agents expose a copyable role field** — it is `role` vs `type`, string vs *numeric enum* (Cursor: `1`/`2`) vs variant-name (Codex `event_msg`), with disjoint value sets (`user/model`, `user/assistant`, `developer`, `system/reasoning/tool_result`, `info/error/warning`). Each source *maps into* the enum; none copies it.

   **`seq` is new (added post-survey).** Ordering cannot rely on `timestamp`: Cursor has no reliable per-message time and orders by array position; Grok Build's timestamps live on a side stream. Every source can supply a monotonic ordinal (JSONL line number, SQLite rowid, Codex's `ordinal`, OpenCode's monotonic `msg_` id). Making ordering source-supplied keeps within-source and cross-source sequencing deterministic where time is absent.

   **`text` is always derived, never copied** — 0 of 6 agents store the turn as a single prose string; it is split across content blocks / a 12-variant `parts[]` / merged `text`+`codeBlocks`+`richText`. The extractor concatenates prose only and **excludes reasoning and tool payloads** (both universally kept separate; reasoning CoT is *encrypted* in Codex and Grok — only a summary is legible). Those live in `data`.

### 3.1 The seam below extraction — source-owned record production

RFC 006's `extract(rawLine)` (§3, below) presumes the engine reads a source's files and hands the extractor one line at a time. **The survey breaks that presumption: 2 of 5 agents have no lines.** OpenCode migrated its transcript into SQLite (`opencode.db`); Cursor never used files (chat lives in `state.vscdb` KV blobs). For these, "the raw line" does not exist — there is a row, or a KV entry the source must assemble.

So the `AgentSource` must own **how raw records are produced**, not just where its files are. Three strategies observed: `JSONL-tail` (Claude, Codex, Gemini, Grok), `SQL-query` (OpenCode), `KV-scan` (Cursor). The engine consumes an abstract stream of **source-native records**; `MessageExtractor` operates on a *record*, not a *line*. This is the single most important finding — RFC 006 (extraction) is *necessary but not sufficient* without this reader seam. It is an `AgentSource` responsibility alongside `classify` and `messages`, and it also governs `LiveDiskIngest`'s incremental step (file-tail vs watch-DB-and-re-query-by-rowid — see §6 and the appendix §4.6).

The seam is a per-source **`MessageExtractor`**, owned by the `AgentSource` (the same way `classify` is now):

```ts
interface MessageExtractor {
  extract(rawLine: unknown): NormalizedMessage | null; // null = skip (not a message row)
}
// AgentSource gains: readonly messages: MessageExtractor
```

`createClaudeCodeSource` provides today's Anthropic-envelope extractor (lifted verbatim from `project-parser.ts`, behavior-identical). A second source provides its own. The ingest engines call `source.messages.extract(line)` and write the returned core columns + the raw `data`; they stop knowing what a content block is.

## 4. Why thin, not rich

The tempting alternative is a rich normalized model (normalize tool calls, content-block kinds, thinking, attachments into shared tables). Rejected, for three reasons:

1. **It re-centers on Claude.** A "generic" rich model is inevitably Claude's model with the serial numbers filed off; the next agent whose structure differs then fights it. The thin core has little to fight — role and text are effectively universal (in *derived*, normalized form), and `tokens`/`time` are the two remaining fields.

   **Survey correction (do not overclaim universality):** only role and text are truly universal, and only after normalization. **Per-message `tokens` exist in just 3 of 6 agents** (Claude, Gemini, OpenCode); Codex counts periodically, Grok only at session level, Cursor effectively not at all. **Reliable per-message `timestamp` exists in 4 of 6** (absent/side-stream in Grok, weak in Cursor). The `?` types already anticipated this; the honesty fix is to stop calling them universal, treat **absent ≠ zero** (per-message zeros mean "this source does not attribute", not "0 tokens used"), and carry a per-source **granularity hint** (`tokens: 'message'|'session'|'none'`, `time: 'message'|'derived'|'none'`) so token-stat and timeline surfaces do not silently mislead. Token *bucket shapes* also diverge (Claude 4, Codex 5, Gemini 6) and **cost in USD** appears in OpenCode/Codex/Grok but not Claude — left in `data`, a candidate column only if a cross-agent cost view is ever wanted.
2. **The raw line already has it.** `data` is lossless. Rich cross-agent structure is a *query-time* concern for the rare consumer that wants it, not an ingest-time normalization tax on every row.
3. **It keeps adapters cheap.** A new source implements one small `extract()` returning five fields. That is the whole point of the exercise.

The cost we accept: cross-agent features that need rich structure (e.g. a unified tool-call timeline spanning Claude + Codex) parse `data` per source at query time. That is the right place for that cost — paid only when used, by the feature that wants it.

## 5. Schema impact — likely none

The v5 `messages` columns already match the normalized core (`msg_type`, `text_content`, the token columns, `data`). This RFC is mostly a **code relocation**, not a migration: the extraction logic moves from the parsers into a source-owned `MessageExtractor`. If the core enum for `msg_type` is tightened, that is a value convention, not a DDL change. **No schema version bump is expected** — which is a sign the boundary is falling where the storage layer already anticipated it.

## 5.1 Storage layout — one index, `source_id` column, **not** per-source DB

The survey raises an obvious question: OpenCode and Cursor store their transcripts in SQLite — so should Spaghetti keep a database per agent kind? **No.** One shared index, with the `source_id` column step 1 already added to every table (`source_files`, `projects`, `sessions`, `messages`). The source dimension is a **column, not a database**. This is a settled decision; recording the rationale so it is not relitigated.

**Don't conflate the source's storage with Spaghetti's.** These are two different databases with two different roles:

- **The agent's own store** — Cursor's `state.vscdb`, OpenCode's `opencode.db` — is a **read-only input**. The adapter's *reader* (§3.1) queries it. A source happening to use SQLite says nothing about how Spaghetti lays out its index.
- **Spaghetti's index** — one unified, `source_id`-tagged store — is the **output** every adapter writes into via the shared writer.

```
Cursor  state.vscdb   ─┐
OpenCode opencode.db  ─┤──►  [per-source reader + extractor]  ──►  ONE Spaghetti index
Claude/Codex JSONL    ─┘                                            (rows tagged by source_id)
```

**Why one index, not per-source:**

1. **The product is a cross-agent browser.** List / search / timeline across all agents is the reason the thing exists. With one index that is a `WHERE source_id = ?` (or no clause) and a single FTS table. Per-source DBs turn every unified read into `ATTACH` + `UNION ALL` across N databases and N merged FTS indexes — a fan-out-and-merge tax paid forever on the headline feature.
2. **The normalized core is identical across sources.** RFC 006 normalizes every source's rows to the *same* shape. Per-source DBs would give N copies of one schema for zero schema benefit, while scattering homogeneous rows that are always read together.
3. **Heterogeneity is already handled per row.** The verbatim native record lives in `data TEXT` (opaque, source-tagged); source-specific structure never needs a source-specific table or column.

**Isolated rebuild — the one real merit of per-DB — is captured without splitting.** The only thing per-source databases buy is blast-radius isolation: Cursor's format drifts (`_v` bumps) and a Cursor-extractor fix should not force Claude to re-ingest. Inside one index that is **per-source re-ingest** — `DELETE FROM … WHERE source_id = 'cursor'`, then re-read only Cursor — plus a per-source format-version in a small `source_state` table. Ingest is already incremental per file (byte-offset / mtime), so per-source rebuild falls out of the existing model. Global DDL bumps still drop-and-rebuild everything (cheap — the index is a pure function of disk); a single adapter's fix re-ingests only its own `source_id`.

**One prerequisite before a second source lands:** `projects.slug` is still the PK, so two sources with a colliding project slug would clash in the shared table. Fix is the composite **`(source_id, slug)`** PK already tracked in §8. Cheap now, a migration headache later.

## 6. The Rust question

The Rust crate re-implements Claude extraction for the bulk path (`project_parser.rs`, `types/`). Two honest options:

- **(A) Rust stays Claude-only; second sources are TS-only first.** A new `AgentSource` ships with a TS `MessageExtractor` and no native path; its ingest runs on the TS engine (correct, just slower on large histories). Native parity is a later, per-source add. **Recommended — and the survey strengthens this to near-certain:** no surveyed second source has a history volume that justifies native ingest yet, and **two of them (OpenCode, Cursor) are SQLite-backed** — a native path for those means an entirely different Rust reader (SQL/KV), not a tweak to the JSONL parser. Reimplementing five heterogeneous readers in Rust up front would be pure speculation. TS-first per source, native only where a specific source's volume later demands it.
- **(B) Generalize the Rust extractor into a trait up front.** Cleaner long-term, but it is speculative work before a second source's shape is even known. Defer until a second source exists and its volume justifies native ingest.

Either way, the parity harness (`test:ingest-diff`) continues to compare the two engines **for `claude-code`**; a TS-only source is simply outside its scope until it gets a native path.

## 7. Plan (when this RFC is accepted)

1. Define `NormalizedMessage` + `MessageExtractor` in `sources/types.ts`; add `messages: MessageExtractor` to `AgentSource`.
2. Extract Claude's logic from `parser/project-parser.ts` into `sources/claude-code/message-extractor.ts` — behavior-identical, verified by the existing parser tests + `test:ingest-diff` staying zero-diff.
3. Route the TS ingest/live writers through `source.messages.extract`.
4. Leave Rust as the `claude-code` native path (option A).
5. Only then: write a second `AgentSource` (Codex) — classifier rules (step 2 seam) + `MessageExtractor` (this seam) + its `source_id` (step 1 seam). No engine or schema change.

## 8. Open questions

- **`msgType` enum membership.** Is `summary` core or does it collapse to `system`? (Leaning: keep `summary` — it is cross-agent meaningful for list previews.) **Survey adds a live sub-question:** several agents make `tool_result` and `reasoning` *first-class record types*, not content nested in an assistant message (Grok `type: tool_result`/`reasoning`; Codex separate `function_call_output`; OpenCode `ToolPart`/`ReasoningPart`). Decide whether these become their own `messages` rows (needs `tool`/`reasoning` enum members) or are folded into the parent turn and left in `data`. Leaning: fold for now (keep the enum small), revisit if a cross-agent tool/reasoning timeline is built.
- **Token model.** ~~a source without them returns zeros~~ — refined by the survey: **absent ≠ zero.** Per-message tokens exist in only 3/6 agents; the DEFAULT-0 columns are fine for storage but a consumer must not read `0` as "0 tokens used" for a source that doesn't attribute per-message. Carry a per-source granularity hint (§4) and confirm no consumer treats absent/zero cache tokens as an error.
- **Record production seam (NEW, §3.1).** Where does the reader live — a `read()`/`enumerate()` on `AgentSource`, or a separate `SourceReader` the ingest planes take? This is the decision that actually prices SQLite sources (OpenCode, Cursor). Resolve it *with* the RFC-006 implementation, not after — a Claude-only relocation that hardcodes "engine reads lines" will have to be reopened the moment a DB source lands.
- **Ordering / `seq` provenance (NEW).** Confirm every source can produce a stable monotonic ordinal cheaply (line number / rowid / native id). Cursor's implicit array-position ordering is the fragile case — a corrupt `fullConversationHeadersOnly` spine loses sequence.
- **Projects PK.** `projects.slug` is still the PK; two sources with a colliding slug would clash. Out of scope here (step-1 follow-up: composite `(source_id, slug)`), but note it before a second source lands on overlapping paths.

## 9. One-line charter

> **Normalize only a derived role + flattened text (with an optional seq/time/tokens); keep the raw native record lossless in `data`; let each `AgentSource` own how it *reads* records (file-tail / SQL / KV) and how it *extracts* them (`MessageExtractor`), the way it already owns `classify` — so a new agent is one small reader + one small extractor, not a schema fight.**

*(Charter updated post-survey: "raw line" → "raw native record" and the reader seam added, because 2 of 6 agents have no line to read. See §3.1 and `006-appendix-agent-survey.md`.)*

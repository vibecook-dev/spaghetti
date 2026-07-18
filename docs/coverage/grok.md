# Grok CLI (xAI) — coverage claim (human summary)

**Updated:** 2026-07-18
**Root:** `~/.grok`
**Machine claim:** `scripts/coverage/grok/claim.json`
**Related:** `docs/rfcs/006-appendix-agent-survey.md`

## Honest framing

Grok is a **rich transcript source**: raw canonical records stay lossless, while the database-backed timeline exposes genuine human turns, assistant prose, thinking, every embedded/local/backend tool call, and paired tool results. Internal prompts and Grok-injected `user` context remain inspectable in raw storage but do not pollute transcript counts or search.

## Ground truth (scanner)

- All `sessions/**/chat_history.jsonl` lines, bucketed by `type`
- Embedded `assistant.tool_calls[]` names, user/context classes, and content block types
- Sibling files per session: `summary.json`, `events.jsonl`, `signals.json`, `updates.jsonl`
- Compaction segment counts, declared turns, and actually rendered verbatim turns
- Top-level `~/.grok/*` presence + size

```bash
python3 scripts/coverage/run_scan.py grok
python3 scripts/coverage/validate_claim.py grok
```

## Ingested (product)

| Surface | Engines | Notes |
|---|---|---|
| `chat_history` file discovery | TS + RS | Cold/warm native default |
| `user` | TS + RS | Genuine queries unwrapped; synthetic reminders/project instructions/user-info hidden |
| `assistant.tool_calls[]` | TS + RS | Every call becomes an independently filterable tool row |
| `tool_result` | TS + RS | Paired to calls by `tool_call_id` |
| `backend_tool_call` | TS + RS | Standalone backend tools such as `web_search` |
| `reasoning` | TS + RS | Readable summaries become thinking; empty summaries are hidden |
| `system` / synthetic context | TS + RS | Raw storage only; excluded from product transcript and FTS |
| Live chat_history tail | TS watch + RS `liveIngestBatch` when `engine=rs` | |

## Partial

| Surface | Notes |
|---|---|
| `summary.json` | Session meta (cwd, title, times, branch) only |
| `events.jsonl` | Turn-scoped timestamps; compaction resets select the latest valid counter epoch |
| `signals.json` | `contextTokensUsed` → last assistant + `tokens_estimated` |

## Deliberately not merged into the transcript

- `updates.jsonl`: high-volume ACP/UI stream. Canonical prose/tool I/O comes from chat history; plans, tasks, retries, subagent lifecycle and progress remain future enrichment surfaces.
- `compaction/segment_*.md`: inventoried, but often contains fewer rendered turn sections than its declared turn count and overlaps current chat history. Merging it would invent completeness and deduplication guarantees the source does not provide.
- Derived SQLite / misc root files

## Compaction behavior

Grok may rewrite `chat_history.jsonl` during compaction while retaining older `events.jsonl` epochs. Cold ingest joins only the latest valid event-counter epoch. Live ingest detects transcript truncation, clears the old session rows, and replays from byte zero so stale tail messages cannot survive the rewrite.

## Engines

| | TS | RS |
|---|---|---|
| Grok cold/warm | yes (fallback) | **yes** (`native.ingest({ sourceId: 'grok' })`) |
| Grok live disk | yes | **yes** (writeBatch → liveIngestBatch) |
| Timestamps / session tokens | yes | yes |

## How to re-verify

```bash
pnpm test:ingest-diff:grok
python3 scripts/coverage/run_scan.py grok
python3 scripts/coverage/validate_claim.py grok
```

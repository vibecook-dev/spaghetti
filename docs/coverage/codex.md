# Codex CLI — coverage claim (human summary)

**Updated:** 2026-07-18

**Root:** `~/.codex`

**Machine claim:** `scripts/coverage/codex/claim.json`

**Related:** `docs/rfcs/006-appendix-agent-survey.md`

## Honest framing

Codex **is not a shallow agent** on disk. Real rollouts are dominated by **tool calls + reasoning**, not chat. Spaghetti retains the canonical rich `response_item` stream and normalizes it into the same database-backed timeline used by Claude sessions.

## Ground truth (scanner)

- All `sessions/**/rollout-*.jsonl` lines, bucketed as:
  - `response_item/<payload.type>`
  - `event_msg/<payload.type>`
  - other top-level `type`s
- Top-level `~/.codex/*` presence + size (config, history, sqlite DBs, memories, …)

## Ingested (product)

| Surface                             | Engine  | Notes                                                                                |
| ----------------------------------- | ------- | ------------------------------------------------------------------------------------ |
| Rollout files                       | TS + RS | `CodexReader`                                                                        |
| `response_item/message`             | TS + RS | Chat turns → `messages` + FTS                                                        |
| `response_item/*_call` + results    | TS + RS | Calls/results retained; timeline joins on `call_id` (including `tool_search_output`) |
| `response_item/reasoning`           | TS + RS | Raw record retained; readable `summary[]` renders as thinking                        |
| Project/session from `session_meta` | TS + RS | cwd → slug; partial (peek only)                                                      |

## Partial

| Surface                 | Notes                                                                  |
| ----------------------- | ---------------------------------------------------------------------- |
| `event_msg/token_count` | Attribute tokens onto assistant; not stored as rows; else tiktoken `~` |

## Ignored (present, not productized)

**High volume (typical installs):**

- Encrypted reasoning bodies without readable summaries (raw retained, no blank UI row)
- Most other `event_msg/*` (duplicate UI projection; prefer `response_item` as SoT)
- `turn_context`, `compacted`, `world_state`, …

**Side artifacts:**

- `config.toml`, `history.jsonl`, `memories*`, `shell_snapshots`, skills/plugins/rules
- Derived SQLite: `state_*.sqlite`, `logs_*.sqlite`, `goals_*.sqlite` (not transcript SoT)

## Out of scope

`auth.json`, tmp/IDE noise, installation ids.

## Engines

|                                         | TS             | RS                                                        |
| --------------------------------------- | -------------- | --------------------------------------------------------- |
| Codex cold/warm                         | yes (fallback) | **yes** (`native.ingest({ sourceId: 'codex' })`)          |
| Codex live disk                         | yes            | not yet (TS live watch; Grok live does use RS writeBatch) |
| Tiktoken estimate when no `token_count` | yes            | not yet (official attribution only)                       |

## How to re-verify

```bash
python3 scripts/coverage/run_scan.py codex
python3 scripts/coverage/validate_claim.py codex
# Expect: rich response_item records covered; duplicate event projections remain ignored
```

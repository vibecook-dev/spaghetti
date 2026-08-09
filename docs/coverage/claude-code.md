# Claude Code — coverage claim (human summary)

**Updated:** 2026-07-13  
**Root:** `~/.claude`  
**Machine claim:** `scripts/coverage/claude_code/claim.json`  
**Related:** `docs/PARSER-UNPARSED-DATA.md`, `docs/PARSER-PIPELINE.md`

## What we treat as ground truth

- Primary: `projects/**/*.jsonl` session files (UUID names)  
- Sidecars: MEMORY.md, sessions-index, subagents, tool-results, workflows  
- Secondary trees: todos, plans, tasks, file-history, teams  
- Config: settings.json / settings.local.json  
- Runtime-only: PID sessions under `sessions/` (not durable index)

## Ingested (product)

| Surface | Engines | Notes |
|---|---|---|
| Session JSONL lines | TS + RS | Row per line |
| Project memory | TS + RS | MEMORY.md |
| Subagents / workflows | TS + RS | |
| Tool-result files | TS + RS | |
| Todos / plans / tasks / file-history | TS + RS | |
| Settings (+ local) | TS | Config domain |

## Partial

| Surface | Notes |
|---|---|
| Teams | Cold TS parse; no FTS; no RS; no live watch |
| Active PID sessions | `listActiveSessionsFromDir` only |
| Plugins / statsig / session-env | Limited readers |

## Ignored (documented)

debug, telemetry, paste-cache, backups (type only), shell-snapshots, cache, ide, root CLAUDE.md, hook *scripts* (hook *event* capture was removed in RFC 007).

## Out of scope

`.credentials.json` and similar secrets.

## How to re-verify

```bash
python3 scripts/coverage/run_scan.py claude-code
python3 scripts/coverage/validate_claim.py claude-code
```

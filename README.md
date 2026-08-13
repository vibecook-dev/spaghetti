# Spaghetti

**Turn your local agent history into a searchable workspace.**

Spaghetti is a local-first CLI and SDK for coding-agent data. Its Rust observation engine indexes **Claude Code** (`~/.claude`), **OpenAI Codex** (`~/.codex`), and **Grok CLI** (`~/.grok`) into one SQLite store so you can search conversations, browse projects and sessions, review artifacts, and build tools on the same canonical index.

[![npm version](https://img.shields.io/npm/v/@vibecook/spaghetti.svg?label=@vibecook/spaghetti)](https://www.npmjs.com/package/@vibecook/spaghetti)
[![npm version](https://img.shields.io/npm/v/@vibecook/spaghetti-sdk.svg?label=@vibecook/spaghetti-sdk)](https://www.npmjs.com/package/@vibecook/spaghetti-sdk)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![Node](https://img.shields.io/badge/node-%3E%3D18-brightgreen.svg)](https://nodejs.org/)
[![CI](https://github.com/vibecook-dev/spaghetti/actions/workflows/ci.yml/badge.svg)](https://github.com/vibecook-dev/spaghetti/actions/workflows/ci.yml)
[![Docs](https://img.shields.io/badge/docs-GitHub%20Pages-2dd4bf)](https://vibecook-dev.github.io/spaghetti/)

**Docs:** [https://vibecook-dev.github.io/spaghetti/](https://vibecook-dev.github.io/spaghetti/) · [API reference](https://vibecook-dev.github.io/spaghetti/api.html) · [CLI commands](https://vibecook-dev.github.io/spaghetti/commands.html)

```text
╭ Spaghetti v0.5.17 ─────────────────────────────────────────────────────╮
│                                                                        │
│  ▄▀▀ █▀█ ▄▀▄ █▀▀ █ █ █▀▀ ▀█▀ ▀█▀ █      Projects           79         │
│  ▀▄▄ █▀▀ █▀█ █ █ █▀█ █▀   █   █  █      Sessions        1,247         │
│  ▄▄▀ █   █ █ ▀▀▀ ▀ ▀ ▀▀▀  ▀   ▀  ▀      Messages       86,412         │
│                                            Tokens          66.3M      │
│  untangle your agent history               ──────────────────────      │
│                                            /search  /stats  /help      │
│  ~/.claude + ~/.codex · 512 MB · 28ms                                  │
│                                                                        │
╰────────────────────────────────────────────────────────────────────────╯
```

## Why people use it

- **Find anything fast** with full-text search over multi-agent message history.
- **Browse Claude and Codex side by side** — agent tabs in the TUI, Agent column in lists, source-scoped sessions so the same repo never mixes agents.
- **Artifacts, not just chat**: projects, sessions, messages, plans, todos, memory, subagents, workflows (Claude), rollouts + token usage (Codex).
- **Stay local-first** with a SQLite index under `~/.spaghetti` — no cloud, no accounts.
- **Build on top of it** with `@vibecook/spaghetti-sdk` and optional React exports.

## Quick start

```bash
npm install -g @vibecook/spaghetti
spag
```

Or run a one-off command without installing globally:

```bash
npx @vibecook/spaghetti search "worker pool"
```

If `~/.codex/sessions` exists, Codex is auto-detected and indexed alongside Claude Code (zero config).

### Prebuilt Rust observation engine

Spaghetti ships the RFC 011 Rust observation/query engine as a platform-specific
prebuilt binary. It owns source discovery, live reconciliation, SQLite writes,
queries, and durable change replay. There is no production TypeScript ingest
fallback; an unsupported or missing native binary fails startup with an
actionable installation error.

This required Node `>=22.13.0`; earlier versions are out of support.

## What you get

| Surface | Best for |
|---|---|
| [`@vibecook/spaghetti`](https://www.npmjs.com/package/@vibecook/spaghetti) | Interactive TUI plus one-shot CLI commands |
| [`@vibecook/spaghetti-sdk`](https://www.npmjs.com/package/@vibecook/spaghetti-sdk) | Scripts, apps, and custom tooling over the same index |
| Rust observation engine | One owner for Claude, Codex, and Grok ingestion, queries, and live changes |
| [Docs site](https://vibecook-dev.github.io/spaghetti/) | Product overview, architecture, CLI & API reference |

## Common commands

```bash
spag                         # launch the multi-agent TUI
spag projects                # list projects (Agent column)
spag sessions .              # sessions for the current repo
spag messages . latest       # latest session transcript
spag search "refactor parser"
spag plan . latest
spag todos . latest
spag doctor
```

## What Spaghetti indexes

- **Claude Code** — projects/sessions under `~/.claude`, messages, plans, todos, memory, subagents, workflows, teams, hooks/channels
- **OpenAI Codex** — rollouts under `~/.codex/sessions/**`, chat turns, official `token_count` usage (tiktoken estimate when events are missing)
- **Grok CLI (xAI)** — `~/.grok/sessions/**/chat_history.jsonl`, conversational turns, turn-scoped timestamps (`events.jsonl`), session token aggregates (`signals.json`); tool I/O and `updates.jsonl` deliberately skipped
- One Rust-owned local SQLite index under `~/.spaghetti/cache`

## Built for two audiences

### Terminal users

Launch `spag` for the full-screen TUI (agent tabs when multiple sources are present), or use subcommands when you just want a fast answer in the shell.

### Tool builders

Use the SDK when you want the same indexed data from scripts or apps:

```ts
import { homedir } from 'node:os';
import { join } from 'node:path';
import { createObservationService } from '@vibecook/spaghetti-sdk';

const api = createObservationService({
  dbPath: join(homedir(), '.spaghetti/cache/spaghetti-rs.db'),
  sources: [
    { adapterId: 'claude-code', roots: [join(homedir(), '.claude')] },
    { adapterId: 'codex', roots: [join(homedir(), '.codex')] },
    { adapterId: 'grok', roots: [join(homedir(), '.grok')] },
  ],
  live: true,
});
await api.initialize();

const projects = await api.getProjectList();
const member = projects[0].members[0];
const sessions = await api.getSessionList(projects[0], {
  sourceId: member.sourceId,
});
const results = await api.search({ text: 'worker thread' });

await api.dispose();
```

## Docs

| Link | Contents |
|---|---|
| [Product site](https://vibecook-dev.github.io/spaghetti/) | Overview, architecture, install |
| [CLI commands](https://vibecook-dev.github.io/spaghetti/commands.html) | Full command reference |
| [API reference](https://vibecook-dev.github.io/spaghetti/api.html) | SDK methods, multi-source, live/runtime |
| [`site/`](site/) | Source for GitHub Pages (preview: `npx serve site`) |

## Repo map

- [`packages/cli`](packages/cli) — published CLI package
- [`packages/sdk`](packages/sdk) — async product API, canonical client transports, and React exports
- [`crates/spaghetti-napi`](crates/spaghetti-napi) — Rust observation, projection, query, and adapter engine
- [`apps/playground`](apps/playground) — Electron demo app
- [`site`](site) — official documentation website
- [`docs`](docs) — RFCs, design notes, and deeper implementation details

## Requirements

- Node.js `>=22.13.0` for end users
- `~/.claude`, `~/.codex`, and/or `~/.grok` for real data
- `pnpm` + Node.js 24 for local workspace development

## Learn more

- [CLI README](packages/cli/README.md)
- [SDK README](packages/sdk/README.md)
- [Releasing guide](RELEASING.md)
- [Rust observation engine RFC](docs/rfcs/011-rust-observation-query-engine.md)

## License

[MIT](LICENSE) — James Yong

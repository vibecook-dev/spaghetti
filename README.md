# Spaghetti

**Turn your local agent history into a searchable workspace.**

Spaghetti is a local-first CLI and SDK for coding-agent data. It indexes **Claude Code** (`~/.claude`) and **OpenAI Codex** (`~/.codex`) into one SQLite store so you can search conversations, browse projects and sessions, review plans/todos/subagents, and build tools on top of the same index.

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

### On npm 12: allow one install script

npm 12 blocks package install scripts by default. Spaghetti stores its index in
SQLite via `better-sqlite3`, which downloads its prebuilt native binding from a
postinstall — so under a stock npm 12 the install completes but every command
that touches the index fails with `Could not locate the bindings file`.

```bash
npm install -g --allow-scripts=better-sqlite3 @vibecook/spaghetti
```

Already installed? Approving records the permission but does **not** run the
script — `rebuild` is what executes it, so you need both:

```bash
npm install-scripts approve better-sqlite3
npm rebuild better-sqlite3
```

Installing Spaghetti as a project dependency rather than globally? npm refuses
the `--allow-scripts` flag there (`EALLOWSCRIPTS`); declare it in your
`package.json` instead:

```json
"allowScripts": { "better-sqlite3": true }
```

npm 11 and earlier run install scripts by default and need none of this. pnpm
users list it under `onlyBuiltDependencies` (this repo already does).

## What you get

| Surface | Best for |
|---|---|
| [`@vibecook/spaghetti`](https://www.npmjs.com/package/@vibecook/spaghetti) | Interactive TUI plus one-shot CLI commands |
| [`@vibecook/spaghetti-sdk`](https://www.npmjs.com/package/@vibecook/spaghetti-sdk) | Scripts, apps, and custom tooling over the same index |
| Native Rust ingest | Faster Claude cold starts by default, with automatic TypeScript fallback |
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
- One local SQLite index under `~/.spaghetti/cache` with a `source_id` column (schema v7+)

Native Grok cold/warm + live batch ship in the Rust addon (default `engine=rs`). Published npm builds pick this up on the next release after these commits land; local workspace builds already include it.

## Built for two audiences

### Terminal users

Launch `spag` for the full-screen TUI (agent tabs when multiple sources are present), or use subcommands when you just want a fast answer in the shell.

### Tool builders

Use the SDK when you want the same indexed data from scripts or apps:

```ts
import { createSpaghettiService, createCodexSource } from '@vibecook/spaghetti-sdk';

const api = createSpaghettiService({
  // optional; CLI auto-detects Codex + Grok when their session dirs exist
  additionalSources: [createCodexSource()],
});
await api.initialize();

const projects = api.getProjectList();
const sessions = api.getSessionList(projects[0].slug, {
  sourceId: projects[0].sourceId, // always scope multi-source drill-downs
});
const results = api.search({ text: 'worker thread' });

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
- [`packages/sdk`](packages/sdk) — parsing, indexing, query APIs, and React exports
- [`crates/spaghetti-napi`](crates/spaghetti-napi) — native Rust ingest engine (Claude bulk path)
- [`apps/playground`](apps/playground) — Electron demo app
- [`site`](site) — official documentation website
- [`docs`](docs) — RFCs, design notes, and deeper implementation details

## Requirements

- Node.js `>=18` for end users
- `~/.claude` and/or `~/.codex` for real data
- On npm 12, `--allow-scripts=better-sqlite3` at install time ([why](#on-npm-12-allow-one-install-script))
- `pnpm` + Node.js 24 for local workspace development

## Learn more

- [CLI README](packages/cli/README.md)
- [SDK README](packages/sdk/README.md)
- [Releasing guide](RELEASING.md)
- [Two-plane architecture](docs/TWO-PLANE-INGEST-ARCHITECTURE.md)

## License

[MIT](LICENSE) — James Yong

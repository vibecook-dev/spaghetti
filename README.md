# Spaghetti

**Turn your local agent history into a searchable workspace.**

Spaghetti is a local-first CLI and SDK for coding-agent data. Its Rust observation engine indexes **Claude Code** (`~/.claude`), **OpenAI Codex** (`~/.codex`), and **Grok CLI** (`~/.grok`) into one SQLite store so you can search conversations, browse projects and sessions, review artifacts, and build tools on the same canonical index.

[![npm version](https://img.shields.io/npm/v/@vibecook/spaghetti.svg?label=@vibecook/spaghetti)](https://www.npmjs.com/package/@vibecook/spaghetti)
[![npm version](https://img.shields.io/npm/v/@vibecook/spaghetti-sdk.svg?label=@vibecook/spaghetti-sdk)](https://www.npmjs.com/package/@vibecook/spaghetti-sdk)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![Node](https://img.shields.io/badge/node-%3E%3D22.13-brightgreen.svg)](https://nodejs.org/)
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
- **See your library immediately** — projects and sessions are listable in about a second while history, usage, and search are still indexing.
- **Trust the token numbers** — usage is counted per agent response, so a streamed reply is one response, not fifty.
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

## Startup: the library comes first

Spaghetti no longer waits for a complete index before it will show you
anything. On startup it runs one bounded discovery pass per configured source
and commits a **catalog** of projects and sessions — on a large real corpus that
is about 120 ms cold and under 10 ms warm. History, usage, artifacts, and
full-text search converge in the background.

While that happens, `spag projects` and `spag sessions` already work. Rows show
what the native surface claims and how far decoding has got; a count nothing has
proven yet is shown as unknown rather than as zero. Search is labelled
unavailable until its index finishes.

`spag doctor` prints the **readiness vector** — six independent fields
(`catalog`, `history`, `usage`, `capabilities`, `artifacts`, `search`), each
`pending`, `indexing`, `ready`, `degraded`, or `unavailable`, with the commit
its evidence was read at. A source that cannot be read completely is reported
`degraded` with the reason and keeps the rows it has.

> **First run after upgrading to 0.8.0 rebuilds the index.** The schema changed,
> so the whole corpus is re-read. The catalog is back in about a second, but
> history and search take as long as a first-ever index — on a large corpus,
> currently hours. Nothing is lost; the database is a pure function of your
> agent files.

## Token usage is counted per response

Usage totals are **response-level**: one agent response contributes once,
however many times the transcript revised its counters as the reply streamed.

This corrects them downward. On a large Claude corpus, 0.7.x reported 78.52B
tokens and 0.8.0 reports 36.88B — **2.13× lower** — because the old accounting
added every streamed-response repeat as if it were new consumption. 362,043
native usage rows are 158,118 actual responses. The new number is the right
one; nothing was lost.

Each of the four buckets (input, output, cache creation, cache read) is
qualified independently. A bucket the source never asserted stays *unknown* and
is never summed as zero, so `spag stats` labels a total `exact`, `estimated`,
or `mixed` rather than implying a precision it does not have.

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

### Watching one session live

`observeSession` attaches to a single session tree and yields typed events. It
opens no database, enumerates no unrelated sessions, and follows the root
transcript plus its subagent transcripts and declared sidecars:

```ts
import { observeSession, isSemanticEvent } from '@vibecook/spaghetti-sdk';

const observer = observeSession({
  adapter_id: 'claude-code',
  agent_root: join(homedir(), '.claude'),
  transcript_path: transcriptPath, // may not exist yet
});

for await (const event of observer) {
  if (event.type === 'bootstrap_complete') commitStagedEpoch(event.scope_epoch);
  else if (isSemanticEvent(event)) apply(event);
}
```

Every event shape is generated from Rust, carries a deterministic `event_id`
and a `scope_epoch`, and — for semantic events — the same
`semantic_revision_ref` a durable query returns for that revision. Losing
continuity is explicit: the observer says so and replaces the whole epoch with
a fresh snapshot rather than dropping events quietly.

Full reference: **[SDK README → Watching one session](packages/sdk/README.md#watching-one-session-observesession)**.
Migrating an existing consumer:
**[docs/integration/chopsticks-observe-session.md](docs/integration/chopsticks-observe-session.md)**.

> **`watchSessionTranscript` is deprecated.** It still ships and still works,
> and it is removed one release after downstream consumers migrate. It reads
> raw lines from one file; `observeSession` gives you reduced revisions from
> the whole session tree, explicit resets, and continuity guarantees. The
> porting table is in the SDK README.

Building an aggregator over stable identity and durable watermarks?
See **[docs/integration/vibefield-phase-a.md](docs/integration/vibefield-phase-a.md)**.

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
- [RFC 012 umbrella](docs/rfcs/012-evidence-backed-adapters-and-progressive-readiness.md) — catalog-first, response-level usage, and the scoped observer; the children are [012A](docs/rfcs/012a-agent-adaptation-and-engine-boundaries.md) (adapters and identity), [012B](docs/rfcs/012b-catalog-readiness-and-progressive-startup.md) (catalog and readiness), [012C](docs/rfcs/012c-runtime-semantics-and-usage-v2.md) (runtime facts and usage), [012D](docs/rfcs/012d-session-scoped-observation.md) (the observer)

## License

[MIT](LICENSE) — James Yong

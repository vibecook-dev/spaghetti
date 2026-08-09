# @vibecook/spaghetti

**Inspect, search, and navigate Claude Code data from the terminal.**

Part of [Spaghetti](https://github.com/vibecook-dev/spaghetti). The CLI ships both a full-screen Ink TUI and one-shot commands. It indexes `~/.claude` into local SQLite via [`@vibecook/spaghetti-sdk`](https://www.npmjs.com/package/@vibecook/spaghetti-sdk).

[![npm](https://img.shields.io/npm/v/@vibecook/spaghetti.svg)](https://www.npmjs.com/package/@vibecook/spaghetti)
[![node](https://img.shields.io/badge/node-%3E%3D18-brightgreen.svg)](https://nodejs.org/)

```text
╭ Spaghetti v0.5.0 ──────────────────────────────────────────────────────╮
│                                                                        │
│  ▄▀▀ █▀█ ▄▀▄ █▀▀ █ █ █▀▀ ▀█▀ ▀█▀ █      Projects           79         │
│  ▀▄▄ █▀▀ █▀█ █ █ █▀█ █▀   █   █  █      Sessions        1,247         │
│  ▄▄▀ █   █ █ ▀▀▀ ▀ ▀ ▀▀▀  ▀   ▀  ▀      Messages       86,412         │
│                                            Tokens          66.3M      │
│  untangle your claude code history         ──────────────────────      │
│                                            /search  /stats  /help      │
╰────────────────────────────────────────────────────────────────────────╯
```

## Install

```bash
npm install -g @vibecook/spaghetti
spag
```

Or run without installing:

```bash
npx @vibecook/spaghetti
```

Binaries: `spaghetti` and the shorter `spag`.

## Modes

- **Bare `spag` in a TTY** launches the Ink TUI.
- **`spag --json` or piped `spag`** prints a JSON summary.
- **Subcommands** (`projects`, `messages`, `search`, …) run as one-off terminal commands.

## TUI

```text
Home
 ├─ Projects
 │   ├─ Sessions
 │   │   ├─ Messages
 │   │   ├─ Todos
 │   │   ├─ Plan
 │   │   └─ Subagents
 │   └─ Memory
 ├─ Stats
 ├─ Help
 └─ Doctor
```

The TUI initializes the core service lazily and shows a boot screen with progress while the parser/indexer runs.

## Commands

| Command | Alias | Purpose |
|---|---|---|
| `projects` | `p` | List indexed projects |
| `sessions [project]` | `s` | List sessions for a project |
| `messages [project] [session]` | `m` | Read session messages |
| `search <query>` |  | Full-text search |
| `stats` | `st` | Aggregate usage and store stats |
| `memory [project]` | `mem` | Show project `MEMORY.md` |
| `todos [project] [session]` | `t` | Show session todos |
| `subagents [project] [session] [agent]` | `sub` | Inspect subagent transcripts |
| `plan [project] [session]` | `pl` | Show a session plan |
| `export [project]` | `x` | Export project/session data (JSON or Markdown) |
| `doctor` |  | Health-check data paths and leftover Claude Code plugins |
| `update` |  | Check for and install updates |
| `uninstall` |  | Show uninstall instructions |

### Removed: hooks, chat, and plugins

`spag hooks`, `spag chat`, `spag plugin`, the Hooks Monitor and Chat TUI views,
and the `spaghetti-hooks` / `spaghetti-channel` Claude Code plugins were removed
in 0.6.0. Transcript ingest, search, and live updates are unaffected.

Uninstalling this CLI never removed plugins that Claude Code had already
installed, so `spag doctor` still reports them and prints the removal commands:

```bash
claude plugin disable   --scope user spaghetti-hooks@spaghetti
claude plugin uninstall --scope user --keep-data spaghetti-hooks@spaghetti
claude plugin disable   --scope user spaghetti-channel@spaghetti
claude plugin uninstall --scope user --keep-data spaghetti-channel@spaghetti
claude plugin marketplace remove --scope user spaghetti
```

`--scope user` and `--keep-data` are not optional: without a scope,
`marketplace remove` drops the declaration from every scope, and `--keep-data`
is what preserves the plugins' data directories. Needs Claude Code 2.1.223+.

### Flexible resolution

Project and session args accept:

- exact names
- fuzzy prefixes
- numeric indexes
- `.` for current working directory
- `latest` / `last` for the newest session
- partial UUIDs for session selection

### Examples

```bash
spag projects
spag sessions .
spag messages . latest
spag search "refactor parser"
spag export . --format markdown --output session.md
spag doctor
```

## Requirements

- Node.js `>=18`
- A local Claude Code data directory at `~/.claude`

## Library usage

If you want to build on top of the same data pipeline, use [`@vibecook/spaghetti-sdk`](https://www.npmjs.com/package/@vibecook/spaghetti-sdk) directly.

## License

[MIT](https://github.com/vibecook-dev/spaghetti/blob/main/LICENSE) — James Yong

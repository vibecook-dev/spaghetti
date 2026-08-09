# @vibecook/spaghetti

## Removed in 0.6.0

`spag hooks`, `spag chat`, and `spag plugin` are gone, along with the Hooks
Monitor and Chat TUI views and the two bundled Claude Code plugins. Transcript
ingest, search, and live updates are unaffected.

- **`spag doctor`** keeps a read-only section reporting leftover plugin state in
  Claude Code — installed, enabled-only, marketplace registration, non-user
  scopes, and inputs it could not read — and prints the raw `claude` commands to
  remove what it can prove belongs to Spaghetti.
- **`spag uninstall`** lists that cleanup *before* the npm removal, because
  removing the CLI never stopped plugins Claude Code had already installed. It
  also names cache paths individually instead of suggesting a blanket
  `rm -rf ~/.spaghetti`, which would have deleted hook and channel history too.
- `ws` and `@types/ws` are no longer dependencies.

Cleanup commands need Claude Code 2.1.223+ for `--scope user` and `--keep-data`.
See [RFC 007](../../docs/rfcs/007-retire-runtime-bridge.md).

## 0.4.0

### Minor Changes

- Live-session support and operational tooling for Claude Code:
  - `spag chat` for active channel sessions
  - `spag doctor` plus a Doctor TUI view for plugin/data-path health checks
  - plugin management expanded to install and manage both `spaghetti-hooks` and `spaghetti-channel`

### Patch Changes

- Updated dependencies []:
  - @vibecook/spaghetti-core@0.4.0

## 0.3.0

### Minor Changes

- TUI redesign: Ink view stack with menu home, tabs, search bar, and scrollbar
  - `spag` (bare) now launches an interactive TUI built with Ink (React for CLIs)
  - Menu home screen: Projects / Stats / Help with branded welcome panel
  - Project-level tabs: Sessions | Memory (←→ to switch)
  - Session-level tabs: Messages | Todos | Plan | Subagents (←→ to switch)
  - Pill-style tab badges: bg-filled active tab, rounded-border inactive tabs
  - Search bar: press `/` to search, context-aware (scopes to current project/session)
  - Message filters: 1-6 toggles for user/claude/thinking/tools/system/internal
  - Scrollbar track on messages view with stable thumb size
  - Boot screen with wordmark and progress bar during initialization
  - 256-color message blocks with selection bars (▐ right for user, ▌ left for claude)
  - Multi-line message previews (3 lines user, 2 lines claude)
  - All subcommands (`spag p`, `spag s`, etc.) remain static one-off commands for agents/scripts

### Patch Changes

- Updated dependencies []:
  - @vibecook/spaghetti-core@0.3.0

## 0.2.2

### Patch Changes

- [`0b1f98f`](https://github.com/jamesyong-42/spaghetti/commit/0b1f98f0c59aa7ffa872f93e24d6c027b69481f7) Thanks [@jamesyong-42](https://github.com/jamesyong-42)! - Interactive TUI browser for `spag p` with arrow-key navigation through projects, sessions, and messages. Features include tool_use/tool_result merging, thinking block visualization, message type filters (1-6 keys), ANSI 256-color message blocks, and chat-app style layout.

- Updated dependencies []:
  - @vibecook/spaghetti-core@0.2.2

## 0.2.1

### Patch Changes

- [`a2944a1`](https://github.com/jamesyong-42/spaghetti/commit/a2944a18261f9e40426a51a850839e4cdd57053d) Thanks [@jamesyong-42](https://github.com/jamesyong-42)! - Truffle-style update command, cross-platform fixes, eslint + prettier
  - `spaghetti update` command — checks npm registry and installs latest version
  - Background update check notifies on startup (24h interval, non-blocking)
  - Windows cross-platform compatibility (path.sep, CRLF, pager fallback)
  - ESLint + Prettier configured with CI integration
  - Cross-platform CI matrix (ubuntu, macOS, Windows)

- Updated dependencies [[`a2944a1`](https://github.com/jamesyong-42/spaghetti/commit/a2944a18261f9e40426a51a850839e4cdd57053d)]:
  - @vibecook/spaghetti-core@0.2.1

## 0.2.0

### Minor Changes

- Initial public release of Spaghetti CLI and core library.
  - 10 CLI commands: projects, sessions, messages, search, stats, memory, todos, subagents, plan, export
  - Architecture C: dedicated SQLite tables, persistent FTS5, streaming parser, worker threads
  - Auto-update, cross-platform (macOS, Linux, Windows)
  - Data recovery from legacy databases

### Patch Changes

- Updated dependencies []:
  - @vibecook/spaghetti-core@0.2.0

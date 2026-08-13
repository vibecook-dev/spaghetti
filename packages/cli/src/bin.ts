/**
 * Spaghetti CLI — Entry point
 *
 * Local-first agent history explorer for your terminal.
 *
 * Bare command (`spag`) on a TTY launches the Ink TUI.
 * Bare command piped (`spag | cat`) outputs summary JSON.
 * Any subcommand (`spag p`, `spag s .`, etc.) falls through to commander.
 */

import { createProgram } from './index.js';
import { initService, shutdownService, registerService, disposeService, createService } from './lib/init.js';
import { handleError } from './lib/error.js';
import { checkForUpdates } from './lib/updater.js';

// Alternate-screen restore, hoisted to module scope so the signal and
// rejection handlers below can leave the alt buffer BEFORE printing —
// otherwise a clean error message lands on the half-drawn TUI canvas.
// Idempotent (guarded by the flag) and a no-op outside the TUI path.
let altScreenActive = false;
function leaveAltScreen(): void {
  if (!altScreenActive) return;
  altScreenActive = false;
  try {
    process.stdout.write('\x1b[?1049l');
  } catch {
    // stdout may already be closed on abrupt exit
  }
}

// Graceful shutdown on SIGINT — prefer awaitable dispose so live pipeline
// drains and checkpoints flush (TUI + one-shots both register via init).
// In the TUI, Ink's exitOnCtrlC normally owns Ctrl-C (raw mode delivers it as
// an input byte, not a signal), so this rarely fires there; it covers the
// non-TUI paths and any case where SIGINT does arrive, restoring the alt
// screen before exit just like the normal TUI teardown does.
process.on('SIGINT', () => {
  void disposeService().finally(() => {
    leaveAltScreen();
    process.exit(0);
  });
});

// Last-resort handlers: never let an unhandled rejection or exception dump a
// raw stack over the alt-screen TUI. Restore the terminal first, then print a
// clean message and exit nonzero via the shared handler.
process.on('unhandledRejection', (reason) => {
  leaveAltScreen();
  handleError(reason);
});
process.on('uncaughtException', (err) => {
  leaveAltScreen();
  handleError(err);
});

async function main(): Promise<void> {
  checkForUpdates();

  const args = process.argv.slice(2);

  // Detect whether this is a bare command (no subcommand).
  // A "subcommand" is a non-flag first argument (e.g. `p`, `sessions`).
  // Flags like --version and --help start with '-' and fall through to commander.
  const hasSubcommand = args.length > 0 && !args[0].startsWith('-');
  const hasJsonFlag = args.includes('--json');
  const isBareCommand = !hasSubcommand;

  if (isBareCommand && !hasJsonFlag && args.length === 0) {
    if (process.stdin.isTTY && process.stdout.isTTY) {
      // TTY: launch the Ink TUI
      const { render } = await import('ink');
      const React = await import('react');
      const { Shell } = await import('./views/shell.js');

      // Enter the alternate screen BEFORE Ink's first render. If we
      // let Shell's `useAlternateScreen` hook do it via useEffect, the
      // first-paint output lands on the main screen and the alt-screen
      // switch-over wipes it — producing a blank canvas until the next
      // state update (often after warm-start completes, i.e. never on
      // a fast machine). Restore on exit via the handler below.
      process.stdout.write('\x1b[?1049h');
      altScreenActive = true;
      process.on('exit', leaveAltScreen);

      // The TUI and one-shot commands now share the same persistent Rust
      // observation owner; durable replay keeps long-lived views fresh.
      const service = createService();
      registerService(service);
      // Don't initialize here — let Shell handle it with BootView
      const { waitUntilExit } = render(React.createElement(Shell, { api: service }), { exitOnCtrlC: true });

      await waitUntilExit();
      await disposeService();
      leaveAltScreen();
      return;
    } else {
      // Piped: output summary JSON
      const { summaryJSON } = await import('./commands/dashboard.js');
      const api = await initService({ silent: true });
      await summaryJSON(api);
      shutdownService();
      return;
    }
  }

  // `spag --json` (bare command with --json flag) → summary JSON
  if (isBareCommand && hasJsonFlag) {
    const { summaryJSON } = await import('./commands/dashboard.js');
    const api = await initService({ silent: true });
    await summaryJSON(api);
    shutdownService();
    return;
  }

  // Has subcommand or other flags (--version, --help): fall through to commander
  const program = createProgram();

  try {
    await program.parseAsync(process.argv);
  } catch (err) {
    handleError(err);
  }
}

// Top-level catch: the TTY and piped/JSON branches await init/summary outside
// their own try/catch, so a failure there would otherwise reject unhandled.
// Restore the terminal and print a clean error (same handler the commander
// branch uses), exiting nonzero.
main().catch((err) => {
  leaveAltScreen();
  handleError(err);
});

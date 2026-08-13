/**
 * Service initialization — sole-owner Rust observation with progress display
 *
 * Uses direct stderr writes instead of setInterval-based spinners,
 * because the core initialization blocks the event loop during
 * synchronous parsing. A spinner relying on setInterval would freeze.
 */

import {
  createObservationService,
  type ObservationHostSource,
  type ObservationService,
} from '@vibecook/spaghetti-sdk/observation';
import { existsSync } from 'node:fs';
import { homedir } from 'node:os';
import { join } from 'node:path';
import pc from 'picocolors';
import { isTTY } from './terminal.js';

/**
 * Auto-detect other installed agents on this machine and return their
 * native adapter roots to observe alongside Claude Code.
 * Only sources whose data actually exists are added, so a machine without
 * Codex pays nothing.
 */
export function detectAdditionalSources(): ObservationHostSource[] {
  const sources: ObservationHostSource[] = [];
  // Codex CLI: ~/.codex/sessions
  const codexRoot = join(homedir(), '.codex');
  if (existsSync(join(codexRoot, 'sessions'))) {
    sources.push({ adapterId: 'codex', roots: [codexRoot] });
  }
  // Grok CLI (xAI): ~/.grok/sessions
  const grokRoot = join(homedir(), '.grok');
  if (existsSync(join(grokRoot, 'sessions'))) {
    sources.push({ adapterId: 'grok', roots: [grokRoot] });
  }
  return sources;
}

let _service: ObservationService | null = null;

export interface InitOptions {
  silent?: boolean;
  /** Primary agent root (default Claude `~/.claude`). */
  rootDir?: string;
  /** @deprecated Use {@link rootDir}. */
  claudeDir?: string;
  dbPath?: string;
}

// Spinner frames for animation between progress events
const SPINNER = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

class ProgressDisplay {
  private frame = 0;
  private lastMessage = '';
  private startTime = Date.now();
  private enabled: boolean;

  constructor(enabled: boolean) {
    this.enabled = enabled;
  }

  update(message: string, current?: number, total?: number): void {
    if (!this.enabled) return;

    this.lastMessage = message;
    const elapsed = ((Date.now() - this.startTime) / 1000).toFixed(1);
    const spinner = pc.cyan(SPINNER[this.frame % SPINNER.length]);
    this.frame++;

    // Shorten long slug-style names to just the last meaningful part
    // e.g., "Parsed -Users-jamesyong-Projects-spaghetti" → "Parsed spaghetti"
    const shortMessage = message.replace(/(?:Parsed\s+)-[A-Za-z]+-[A-Za-z]+-.*?-([^-\s(]+)/, 'Parsed $1');

    const cols = process.stderr.columns ?? 80;

    let line: string;
    if (total && total > 0) {
      const barWidth = 20;
      const filled = Math.round((barWidth * (current ?? 0)) / total);
      const bar = pc.green('█'.repeat(filled)) + pc.dim('░'.repeat(barWidth - filled));
      line = `  ${spinner} ${bar} ${pc.dim(`${current}/${total}`)} ${shortMessage} ${pc.dim(`${elapsed}s`)}`;
    } else {
      line = `  ${spinner} ${shortMessage} ${pc.dim(`${elapsed}s`)}`;
    }

    // Truncate to terminal width to prevent wrapping, then pad to clear leftovers
    const plainLen = stripAnsi(line).length;
    if (plainLen > cols) {
      // Truncate the visible text to fit terminal width
      const excess = plainLen - cols + 3; // room for "..."
      line = `  ${spinner} ${shortMessage.slice(0, shortMessage.length - excess)}${pc.dim('…')} ${pc.dim(`${elapsed}s`)}`;
    }
    const finalLen = stripAnsi(line).length;
    const padding = Math.max(0, cols - finalLen);
    process.stderr.write(`\r${line}${' '.repeat(padding)}`);
  }

  success(durationMs: number): void {
    if (!this.enabled) return;
    const duration = (durationMs / 1000).toFixed(1);
    const padding = Math.max(0, (process.stderr.columns ?? 80) - 30);
    process.stderr.write(`\r  ${pc.green('✔')} Ready in ${pc.bold(`${duration}s`)}${' '.repeat(padding)}\n`);
  }

  error(message: string): void {
    if (!this.enabled) return;
    const padding = Math.max(0, (process.stderr.columns ?? 80) - 20);
    process.stderr.write(`\r  ${pc.red('✖')} ${message}${' '.repeat(padding)}\n`);
  }
}

function stripAnsi(str: string): string {
  // eslint-disable-next-line no-control-regex
  return str.replace(/\x1b\[[0-9;]*m/g, '');
}

export function createService(opts?: InitOptions): ObservationService {
  const claudeRoot = opts?.rootDir ?? opts?.claudeDir ?? join(homedir(), '.claude');
  const dbPath = opts?.dbPath ?? join(homedir(), '.spaghetti', 'cache', 'spaghetti-rs.db');
  return createObservationService({
    dbPath,
    sources: [{ adapterId: 'claude-code', roots: [claudeRoot] }, ...detectAdditionalSources()],
    ownerLabel: 'spaghetti-cli',
    live: true,
  });
}

export async function initService(opts?: InitOptions): Promise<ObservationService> {
  const service = createService(opts);

  registerService(service);

  const showProgress = !opts?.silent && isTTY();
  const progress = new ProgressDisplay(showProgress);

  progress.update('Initializing...');

  const startTime = Date.now();

  const unsub = service.onProgress((p: any) => {
    progress.update(p.message, p.current, p.total);
  });

  try {
    await service.initialize();
  } catch (err) {
    progress.error('Initialization failed');
    unsub();
    throw err;
  }

  progress.success(Date.now() - startTime);
  unsub();

  return service;
}

/**
 * Register an externally-constructed service (e.g. the TUI in `bin.ts`)
 * so it participates in `disposeService()` / `shutdownService()`. The
 * SIGINT handler can then tear down the live pipeline cleanly without
 * needing every entry point to wire up its own signal handler.
 *
 * Replaces any previously-registered service — only one is tracked at
 * a time, matching the single-service-per-process assumption baked
 * into `_service` since the file's inception.
 */
export function registerService(service: ObservationService): void {
  _service = service;
}

export function shutdownService(): void {
  if (_service) {
    _service.shutdown();
    _service = null;
  }
}

/**
 * Awaitable teardown — preferred over `shutdownService()` whenever the
 * caller can `await`. Routes through `SpaghettiAPI.dispose()` so the
 * live pipeline stops, the writer loop drains, and SQLite closes in
 * the right order. SIGINT in `bin.ts` calls this before
 * `process.exit` so checkpoint state is never lost on Ctrl-C.
 */
export async function disposeService(): Promise<void> {
  if (_service) {
    const svc = _service;
    _service = null;
    await svc.dispose();
  }
}

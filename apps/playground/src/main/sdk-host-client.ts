/** Thin Electron-main broker for the SDK UtilityProcess. No SDK/SQLite work runs here. */

import { utilityProcess, type UtilityProcess } from 'electron';
import { existsSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import type { IngestEngine } from '@vibecook/spaghetti-sdk';
import {
  deserializeError,
  isSdkHostMessage,
  type SdkHostCommand,
  type SdkHostEvent,
  type SdkRpcArgs,
  type SdkRpcMethod,
  type SdkRpcResult,
} from '../shared/sdk-protocol.js';

const __dirname = dirname(fileURLToPath(import.meta.url));
const DEFAULT_REQUEST_TIMEOUT_MS = 120_000;
const MAINTENANCE_TIMEOUT_MS = 10 * 60_000;
const SHUTDOWN_TIMEOUT_MS = 15_000;
const RESTART_DELAYS_MS = [250, 1_000, 3_000] as const;

interface PendingRequest {
  resolve(value: unknown): void;
  reject(error: unknown): void;
  timer: ReturnType<typeof setTimeout>;
}

export interface SdkHostClientOptions {
  dbPath: string;
  engine: IngestEngine;
  rootDir?: string;
  onEvent(event: SdkHostEvent): void;
}

function resolveSdkHostPath(): string {
  const candidates = [join(__dirname, 'sdk-host.mjs'), join(__dirname, 'sdk-host.js')];
  for (const candidate of candidates) {
    if (existsSync(candidate)) return candidate;
  }
  return candidates[0]!;
}

export class SdkHostClient {
  private process: UtilityProcess | null = null;
  private startPromise: Promise<void> | null = null;
  private resolveStart: (() => void) | null = null;
  private rejectStart: ((error: unknown) => void) | null = null;
  private readonly pending = new Map<number, PendingRequest>();
  private nextRequestId = 1;
  private shuttingDown = false;
  private restartAttempts = 0;
  private restartTimer: ReturnType<typeof setTimeout> | null = null;

  constructor(private readonly options: SdkHostClientOptions) {}

  start(): Promise<void> {
    if (this.shuttingDown) return Promise.reject(new Error('SDK utility client is shutting down'));
    if (this.process && this.startPromise) return this.startPromise;
    if (this.restartTimer) {
      clearTimeout(this.restartTimer);
      this.restartTimer = null;
    }

    const startPromise = new Promise<void>((resolve, reject) => {
      this.resolveStart = resolve;
      this.rejectStart = reject;
    });
    this.startPromise = startPromise;

    let proc: UtilityProcess;
    try {
      proc = utilityProcess.fork(resolveSdkHostPath(), [], {
        serviceName: 'spaghetti-sdk',
        stdio: 'pipe',
        env: {
          ...process.env,
          SPAGHETTI_DB_PATH: this.options.dbPath,
          SPAGHETTI_ENGINE: this.options.engine,
          ...(this.options.rootDir ? { SPAGHETTI_ROOT_DIR: this.options.rootDir } : {}),
        },
      });
    } catch (error) {
      const rejectStart = this.rejectStart;
      this.startPromise = null;
      this.resolveStart = null;
      this.rejectStart = null;
      // Reject the promise returned to every concurrent start/request caller.
      rejectStart?.(error);
      return startPromise;
    }
    this.process = proc;
    proc.stdout?.on('data', (data) => process.stdout.write(`[sdk-host] ${data}`));
    proc.stderr?.on('data', (data) => process.stderr.write(`[sdk-host] ${data}`));
    proc.on('message', (message) => this.handleMessage(proc, message));
    proc.on('exit', (code) => this.handleExit(proc, code));
    return startPromise;
  }

  async request<K extends SdkRpcMethod>(method: K, ...args: SdkRpcArgs<K>): Promise<SdkRpcResult<K>> {
    await this.start();
    const proc = this.process;
    if (!proc) throw new Error('SDK utility is unavailable');
    const id = this.nextRequestId++;
    const timeoutMs =
      method === 'rebuildIndex' || method === 'retryInit' ? MAINTENANCE_TIMEOUT_MS : DEFAULT_REQUEST_TIMEOUT_MS;
    const result = new Promise<unknown>((resolve, reject) => {
      const timer = setTimeout(() => {
        this.pending.delete(id);
        reject(new Error(`SDK utility request timed out: ${method}`));
      }, timeoutMs);
      this.pending.set(id, { resolve, reject, timer });
    });

    try {
      proc.postMessage({ type: 'request', id, method, args } as SdkHostCommand);
    } catch (error) {
      this.rejectPending(id, error);
    }
    return (await result) as SdkRpcResult<K>;
  }

  async dispose(): Promise<void> {
    if (this.shuttingDown) return;
    this.shuttingDown = true;
    if (this.restartTimer) {
      clearTimeout(this.restartTimer);
      this.restartTimer = null;
    }
    const proc = this.process;
    if (!proc) return;

    const id = this.nextRequestId++;
    const completion = new Promise<void>((resolve) => {
      const timer = setTimeout(() => {
        this.pending.delete(id);
        try {
          proc.kill();
        } catch {
          /* already exited */
        }
        resolve();
      }, SHUTDOWN_TIMEOUT_MS);
      this.pending.set(id, { resolve: () => resolve(), reject: () => resolve(), timer });
    });
    try {
      proc.postMessage({ type: 'shutdown', id });
    } catch {
      this.resolvePending(id, undefined);
    }
    await completion;
  }

  kill(): void {
    this.shuttingDown = true;
    if (this.restartTimer) clearTimeout(this.restartTimer);
    this.restartTimer = null;
    try {
      this.process?.kill();
    } catch {
      /* already exited */
    }
    this.process = null;
    this.rejectAll(new Error('SDK utility was terminated'));
  }

  private handleMessage(proc: UtilityProcess, value: unknown): void {
    if (proc !== this.process || !isSdkHostMessage(value)) return;
    switch (value.type) {
      case 'host-ready':
        this.resolveStart?.();
        this.resolveStart = null;
        this.rejectStart = null;
        return;
      case 'response':
        if (value.ok) this.resolvePending(value.id, value.result);
        else this.rejectPending(value.id, deserializeError(value.error));
        return;
      case 'event':
        if (value.data.event === 'ready') this.restartAttempts = 0;
        this.options.onEvent(value.data);
        return;
      case 'shutdown-complete':
        this.resolvePending(value.id, undefined);
        return;
      case 'fatal':
        this.options.onEvent({ event: 'init-error', payload: deserializeError(value.error).message });
        try {
          proc.kill();
        } catch {
          /* exit handler performs cleanup/restart */
        }
    }
  }

  private handleExit(proc: UtilityProcess, code: number | null): void {
    if (proc !== this.process) return;
    this.process = null;
    const error = new Error(`SDK utility exited${code === null ? '' : ` with code ${code}`}`);
    this.rejectStart?.(error);
    this.resolveStart = null;
    this.rejectStart = null;
    this.startPromise = null;
    this.rejectAll(error);
    if (this.shuttingDown) return;

    const delay = RESTART_DELAYS_MS[this.restartAttempts];
    if (delay === undefined) {
      this.options.onEvent({ event: 'init-error', payload: 'SDK utility repeatedly crashed and could not restart.' });
      return;
    }
    this.restartAttempts += 1;
    this.options.onEvent({
      event: 'progress',
      payload: { phase: 'parsing', message: `SDK utility exited — restarting in ${delay}ms…` },
    });
    this.restartTimer = setTimeout(() => {
      this.restartTimer = null;
      void this.start().catch((restartError: unknown) => {
        this.options.onEvent({ event: 'init-error', payload: String(restartError) });
      });
    }, delay);
  }

  private resolvePending(id: number, value: unknown): void {
    const entry = this.pending.get(id);
    if (!entry) return;
    this.pending.delete(id);
    clearTimeout(entry.timer);
    entry.resolve(value);
  }

  private rejectPending(id: number, error: unknown): void {
    const entry = this.pending.get(id);
    if (!entry) return;
    this.pending.delete(id);
    clearTimeout(entry.timer);
    entry.reject(error);
  }

  private rejectAll(error: unknown): void {
    for (const [id, entry] of this.pending) {
      this.pending.delete(id);
      clearTimeout(entry.timer);
      entry.reject(error);
    }
  }
}

/**
 * Worker Pool — Manages a pool of parse workers for parallel project parsing
 *
 * Distributes project slugs across workers using a queue-based approach.
 * When a worker completes a project, it receives the next one from the queue.
 * All messages from workers are forwarded to the caller's onMessage callback
 * for SQLite ingestion on the main thread (single-writer constraint).
 */

import { Worker } from 'node:worker_threads';
import * as os from 'node:os';
import * as path from 'node:path';
import { fileURLToPath } from 'node:url';
import type { WorkerToMainMessage, MainToWorkerMessage } from './worker-types.js';

// ═══════════════════════════════════════════════════════════════════════════
// INTERFACE
// ═══════════════════════════════════════════════════════════════════════════

export interface WorkerPoolOptions {
  /** Maximum number of worker threads. Defaults to min(cpus - 1, 4). */
  maxWorkers?: number;
  /** Path to the compiled parse-worker.js script. */
  workerScript?: string;
}

export interface WorkerPool {
  /**
   * Parse multiple projects in parallel using worker threads.
   *
   * @param rootDir - Path to the .claude directory
   * @param slugs - Project slugs to parse
   * @param onMessage - Callback for each message from any worker (main thread handles SQLite writes)
   * @returns Promise that resolves when ALL projects are complete
   */
  parseProjects(rootDir: string, slugs: string[], onMessage: (msg: WorkerToMainMessage) => void): Promise<void>;

  /** Shut down all workers. */
  shutdown(): void;
}

// ═══════════════════════════════════════════════════════════════════════════
// IMPLEMENTATION
// ═══════════════════════════════════════════════════════════════════════════

class WorkerPoolImpl implements WorkerPool {
  private maxWorkers: number;
  private workerScript: string;
  private workers: Worker[] = [];

  constructor(options?: WorkerPoolOptions) {
    // Leave 1 core for main thread SQLite writes, cap at 8 (up from 4)
    this.maxWorkers = options?.maxWorkers ?? Math.min(os.cpus().length - 1, 8);
    if (this.maxWorkers < 1) this.maxWorkers = 1;

    // Resolve the worker script next to this module. fileURLToPath, not
    // URL.pathname: the latter yields "/C:/..." on Windows, which Worker
    // cannot open.
    //
    // Deliberately NOT `new URL('./parse-worker.js', import.meta.url)`.
    // That is bundler syntax for an asset reference, and in library mode
    // vite resolves it by inlining the whole worker as a
    // `data:text/javascript;base64,...` URL. `fileURLToPath` then throws
    // ERR_INVALID_URL_SCHEME on it, so every `createWorkerPool()` call from
    // the built package threw in the constructor and cold start silently
    // fell back to sequential parsing. Joining a dirname to a plain string
    // is opaque to the bundler's asset scanner, so the path survives the
    // build and points at the `parse-worker` entry emitted alongside it.
    this.workerScript =
      options?.workerScript ?? path.join(path.dirname(fileURLToPath(import.meta.url)), 'parse-worker.js');
  }

  async parseProjects(rootDir: string, slugs: string[], onMessage: (msg: WorkerToMainMessage) => void): Promise<void> {
    if (slugs.length === 0) return;

    const workerCount = Math.min(this.maxWorkers, slugs.length);
    const queue = [...slugs];
    const totalCount = slugs.length;
    let completedCount = 0;
    // Slug currently being parsed per live worker; `null` = idle.
    const inFlight = new Map<Worker, string | null>();
    // Slugs already re-queued once after a worker crash. A slug that kills
    // two workers is counted lost instead of retried forever.
    const retried = new Set<string>();
    // Slugs abandoned after repeated crashes. Losing SOME is graceful
    // degradation; losing ALL means the worker system itself is broken
    // (e.g. unloadable script) and must fail loudly, not resolve empty.
    let crashLostCount = 0;
    let settled = false;

    return new Promise<void>((resolve, reject) => {
      const settle = (err?: Error): void => {
        if (settled) return;
        settled = true;
        if (err) reject(err);
        else resolve();
      };

      const checkDone = (): void => {
        if (completedCount < totalCount) return;
        if (crashLostCount >= totalCount) {
          settle(new Error(`[worker-pool] All ${totalCount} project(s) lost to worker crashes — nothing was parsed.`));
        } else {
          settle();
        }
      };

      const assignNext = (worker: Worker): void => {
        const slug = queue.shift();
        if (slug === undefined) {
          inFlight.set(worker, null);
          return;
        }
        inFlight.set(worker, slug);
        worker.postMessage({
          type: 'parse-project',
          rootDir,
          slug,
        } satisfies MainToWorkerMessage);
      };

      const removeWorker = (worker: Worker): void => {
        const idx = this.workers.indexOf(worker);
        if (idx >= 0) this.workers.splice(idx, 1);
        inFlight.delete(worker);
        void worker.terminate().catch(() => {});
      };

      /**
       * Shared crash path for the 'error' event and hard deaths that only
       * surface as a non-zero 'exit'. Retries the in-flight slug once on a
       * fresh worker, and fails the whole run loudly if no workers remain —
       * a hang here would stall cold-start ingest forever.
       */
      const handleCrash = (worker: Worker, err: unknown): void => {
        console.error(`[worker-pool] Worker crashed:`, err);
        const lostSlug = inFlight.get(worker) ?? null;
        removeWorker(worker);

        if (lostSlug !== null) {
          if (!retried.has(lostSlug)) {
            retried.add(lostSlug);
            queue.unshift(lostSlug);
          } else {
            completedCount++;
            crashLostCount++;
            console.error(`[worker-pool] Project "${lostSlug}" lost after repeated worker crashes.`);
          }
        }

        if (queue.length > 0) {
          const replacement = spawnWorker();
          if (replacement) assignNext(replacement);
        }

        if (this.workers.length === 0 && completedCount < totalCount) {
          settle(
            new Error(
              `[worker-pool] All workers died with ${totalCount - completedCount} of ${totalCount} project(s) unfinished.`,
            ),
          );
          return;
        }
        checkDone();
      };

      // Every worker — initial or replacement — gets FRESH handlers bound to
      // itself. (A previous version cloned the dead worker's listeners onto
      // replacements; the cloned closures kept assigning work to the dead
      // worker and the pool hung.)
      const setupWorker = (worker: Worker): void => {
        worker.on('message', (msg: WorkerToMainMessage) => {
          onMessage(msg);

          // Both outcomes free the worker: assign the next slug or finish.
          if (msg.type === 'project-complete' || msg.type === 'worker-error') {
            if (msg.type === 'worker-error') {
              console.error(`[worker-pool] Error parsing project "${msg.slug}": ${msg.error}`);
            }
            completedCount++;
            assignNext(worker);
            checkDone();
          }
        });

        worker.on('error', (err) => {
          if (settled) return;
          handleCrash(worker, err);
        });

        worker.on('exit', (code) => {
          if (settled) return;
          // Still tracked in `inFlight` means the 'error' handler never ran —
          // a hard death (segfault/OOM kill). Route it through the same path.
          if (code !== 0 && inFlight.has(worker)) {
            handleCrash(worker, new Error(`worker exited with code ${code}`));
          }
        });
      };

      const spawnWorker = (): Worker | null => {
        let worker: Worker;
        try {
          worker = new Worker(this.workerScript);
        } catch {
          return null;
        }
        this.workers.push(worker);
        setupWorker(worker);
        return worker;
      };

      // Spawn the initial fleet.
      for (let i = 0; i < workerCount; i++) {
        const worker = spawnWorker();
        if (!worker) {
          if (this.workers.length === 0) {
            settle(new Error(`Failed to create any worker thread. Worker script: ${this.workerScript}`));
            return;
          }
          break; // partial fleet is fine — the live workers drain the queue
        }
        assignNext(worker);
      }
    });
  }

  shutdown(): void {
    for (const worker of this.workers) {
      try {
        worker.postMessage({ type: 'shutdown' } satisfies MainToWorkerMessage);
      } catch {
        // Worker may already be terminated
      }
    }
    // Force terminate after a brief delay; unref so a one-shot process
    // isn't pinned open for the grace period.
    const timer = setTimeout(() => {
      for (const worker of this.workers) {
        void worker.terminate().catch(() => {});
      }
      this.workers = [];
    }, 100);
    timer.unref();
  }
}

// ═══════════════════════════════════════════════════════════════════════════
// FACTORY & UTILITIES
// ═══════════════════════════════════════════════════════════════════════════

/**
 * Check if worker_threads is available in the current runtime.
 * Since we import Worker at the top of this module, if we got here
 * without error, worker_threads is available.
 */
export function isWorkerThreadsAvailable(): boolean {
  try {
    // The Worker class is imported at the top of this module from 'node:worker_threads'.
    // If that import succeeded, worker_threads is available.
    return typeof Worker === 'function';
  } catch {
    return false;
  }
}

/**
 * Create a new worker pool for parallel project parsing.
 */
export function createWorkerPool(options?: WorkerPoolOptions): WorkerPool {
  return new WorkerPoolImpl(options);
}

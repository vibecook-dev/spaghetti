import { useEffect, useState } from 'react';

export interface AsyncSnapshot<T> {
  value: T;
  isLoading: boolean;
  error: unknown;
}

interface AsyncSnapshotRequest<T> {
  generation: number;
  request: () => Promise<T>;
  resolve: (value: T) => void;
  reject: (error: unknown) => void;
}

/** Pure request-generation primitive shared by the hook and its regression test. */
export class AsyncSnapshotCoordinator<T> {
  private generation = 0;
  private active = true;
  private running = false;
  private queued: AsyncSnapshotRequest<T> | undefined;

  load(request: () => Promise<T>, resolve: (value: T) => void, reject: (error: unknown) => void): void {
    if (!this.active) return;
    const next = { generation: ++this.generation, request, resolve, reject };
    if (this.running) {
      this.queued = next;
      return;
    }
    this.run(next);
  }

  dispose(): void {
    this.active = false;
    this.generation += 1;
    this.queued = undefined;
  }

  private run(job: AsyncSnapshotRequest<T>): void {
    this.running = true;
    void Promise.resolve()
      .then(job.request)
      .then(
        (value) => {
          if (this.active && job.generation === this.generation) job.resolve(value);
        },
        (error: unknown) => {
          if (this.active && job.generation === this.generation) job.reject(error);
        },
      )
      .finally(() => {
        this.running = false;
        const queued = this.queued;
        this.queued = undefined;
        if (this.active && queued) this.run(queued);
      });
  }
}

/**
 * Load one async snapshot and refresh it after invalidation. A monotonically
 * increasing generation suppresses late results from superseded scopes.
 */
export function useAsyncSnapshot<T>(
  initialValue: T,
  load: () => Promise<T>,
  subscribe: (invalidate: () => void) => () => void,
): AsyncSnapshot<T> {
  const [snapshot, setSnapshot] = useState<AsyncSnapshot<T>>({
    value: initialValue,
    isLoading: true,
    error: null,
  });

  useEffect(() => {
    const coordinator = new AsyncSnapshotCoordinator<T>();
    let active = true;
    setSnapshot({ value: initialValue, isLoading: true, error: null });

    const refresh = (): void => {
      if (!active) return;
      setSnapshot((current) => ({ ...current, isLoading: true, error: null }));
      coordinator.load(
        load,
        (value) => {
          setSnapshot({ value, isLoading: false, error: null });
        },
        (error: unknown) => {
          setSnapshot((current) => ({ ...current, isLoading: false, error }));
        },
      );
    };

    const unsubscribe = subscribe(refresh);
    refresh();
    return () => {
      active = false;
      coordinator.dispose();
      unsubscribe();
    };
  }, [initialValue, load, subscribe]);

  return snapshot;
}

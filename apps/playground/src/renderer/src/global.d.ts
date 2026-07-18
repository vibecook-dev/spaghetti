import type { SpaghettiBridge } from '@shared/ipc';

declare global {
  interface Window {
    spaghetti: SpaghettiBridge;
    mille?: {
      openWorkspace(path: string): Promise<{ ok: true; root: string }>;
      closeWorkspace(): Promise<{ ok: true }>;
    };
  }
}

export {};

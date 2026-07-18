import type { SpaghettiBridge } from '@shared/ipc';

declare global {
  interface Window {
    spaghetti: SpaghettiBridge;
    mille?: {
      openWorkspace(path: string): Promise<{ ok: true; root: string }>;
      closeWorkspace(): Promise<{ ok: true }>;
    };
    windowControls?: {
      minimize(): Promise<void>;
      toggleFullScreen(): Promise<void>;
      close(): Promise<void>;
    };
    fileViewer?: {
      openHtmlInBrowser(path: string, browser: 'default' | 'safari' | 'chrome' | 'firefox'): Promise<{ ok: true }>;
    };
  }
}

export {};

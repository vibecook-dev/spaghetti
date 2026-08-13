import React, { createContext, useContext } from 'react';
import type { SpaghettiAPI } from '../api.js';
import type { SettingsFile } from '../types/index.js';

type ReactQueryMethod =
  | 'getProjectList'
  | 'getSessionList'
  | 'getSessionMessages'
  | 'getProjectMemory'
  | 'getToolResult'
  | 'getSessionSubagents'
  | 'getSubagentMessages'
  | 'search'
  | 'getStats'
  | 'onProgress'
  | 'onReady'
  | 'onChange';

/** Portable asynchronous surface consumed by the published React package. */
export interface SpaghettiReactClient extends Pick<SpaghettiAPI, ReactQueryMethod> {
  /** Embedded owners may answer synchronously; IPC clients resolve remotely. */
  isReady(): boolean | Promise<boolean>;
  /** Optional presentation setting read for hosts that expose one. */
  getSettings?(): Promise<SettingsFile | null>;
}

const SpaghettiContext = createContext<SpaghettiReactClient | null>(null);

export interface SpaghettiProviderProps {
  client: SpaghettiReactClient;
  children: React.ReactNode;
}

export function SpaghettiProvider({ client, children }: SpaghettiProviderProps) {
  return <SpaghettiContext.Provider value={client}>{children}</SpaghettiContext.Provider>;
}

export function useSpaghettiClient(): SpaghettiReactClient {
  const client = useContext(SpaghettiContext);
  if (!client) {
    throw new Error('useSpaghettiClient must be used within a SpaghettiProvider');
  }
  return client;
}

/** @deprecated Use `useSpaghettiClient`; this alias is asynchronous too. */
export const useSpaghettiAPI = useSpaghettiClient;

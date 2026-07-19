/** Typed messages exchanged between Electron main and the SDK UtilityProcess. */

import type { ActiveSessionChange, SpaghettiIPC } from './ipc.js';
import type { InitProgress, SegmentChangeBatch } from '@vibecook/spaghetti-sdk';

export type SdkRpcMethod = keyof SpaghettiIPC;

export type SdkRpcArgs<K extends SdkRpcMethod> = Parameters<SpaghettiIPC[K]>;
export type SdkRpcResult<K extends SdkRpcMethod> = Awaited<ReturnType<SpaghettiIPC[K]>>;

export type SdkRpcRequest = {
  [K in SdkRpcMethod]: {
    type: 'request';
    id: number;
    method: K;
    args: SdkRpcArgs<K>;
  };
}[SdkRpcMethod];

export interface SerializedError {
  name: string;
  message: string;
  stack?: string;
  code?: string;
}

export type SdkHostCommand = SdkRpcRequest | { type: 'shutdown'; id: number };

export type SdkHostEvent =
  | { event: 'progress'; payload: InitProgress }
  | { event: 'ready'; payload: { durationMs: number } }
  | { event: 'change'; payload: SegmentChangeBatch }
  | { event: 'active-session-change'; payload: ActiveSessionChange }
  | { event: 'init-error'; payload: string };

export type SdkHostMessage =
  | { type: 'host-ready' }
  | { type: 'response'; id: number; ok: true; result: unknown }
  | { type: 'response'; id: number; ok: false; error: SerializedError }
  | { type: 'event'; data: SdkHostEvent }
  | { type: 'shutdown-complete'; id: number }
  | { type: 'fatal'; error: SerializedError };

export function serializeError(error: unknown): SerializedError {
  if (error instanceof Error) {
    const code = (error as NodeJS.ErrnoException).code;
    return {
      name: error.name || 'Error',
      message: error.message,
      ...(error.stack ? { stack: error.stack } : {}),
      ...(code ? { code } : {}),
    };
  }
  return { name: 'Error', message: String(error) };
}

export function deserializeError(error: SerializedError): Error {
  const result = new Error(error.message);
  result.name = error.name;
  if (error.stack) result.stack = error.stack;
  if (error.code) (result as NodeJS.ErrnoException).code = error.code;
  return result;
}

export function isSdkHostMessage(value: unknown): value is SdkHostMessage {
  if (!value || typeof value !== 'object') return false;
  const message = value as Record<string, unknown>;
  switch (message.type) {
    case 'host-ready':
      return true;
    case 'response':
      return typeof message.id === 'number' && typeof message.ok === 'boolean';
    case 'event': {
      const data = message.data;
      return (
        !!data &&
        typeof data === 'object' &&
        ['progress', 'ready', 'change', 'active-session-change', 'init-error'].includes(
          String((data as Record<string, unknown>).event),
        )
      );
    }
    case 'shutdown-complete':
      return typeof message.id === 'number';
    case 'fatal':
      return !!message.error && typeof message.error === 'object';
    default:
      return false;
  }
}

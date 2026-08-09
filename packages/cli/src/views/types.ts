/**
 * View types — shared type definitions for the TUI view stack
 */

import type React from 'react';
import type { ProjectListItem, SessionListItem } from '@vibecook/spaghetti-sdk';

// ─── View Types ────────────────────────────────────────────────────────

export type ViewType =
  | 'boot'
  | 'menu'
  | 'projects'
  | 'project-tabs'
  | 'sessions'
  | 'session-tabs'
  | 'messages'
  | 'detail'
  | 'search'
  | 'stats'
  | 'memory'
  | 'todos'
  | 'plan'
  | 'subagents'
  | 'help'
  | 'doctor';

export interface ViewEntry {
  type: ViewType;
  component: React.FC;
  breadcrumb: string;
  hints?: string;
}

// ─── Navigation ────────────────────────────────────────────────────────

export interface ViewNav {
  push(view: ViewEntry): void;
  pop(): void;
  replace(view: ViewEntry): void;
  /** Pop the current view and push multiple views in a single state update (for search→navigate) */
  popAndPush(...views: ViewEntry[]): void;
  quit(): void;
  enterSearchMode(): void;
  context: ViewContext;
  /** True when the search input overlay is active — views should suppress their key handling */
  searchMode: boolean;
}

export interface ViewContext {
  project?: ProjectListItem;
  session?: SessionListItem;
  sessionIndex?: number;
}

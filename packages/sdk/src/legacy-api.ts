/**
 * Repository-only synchronous API contract used by the retired TypeScript
 * differential oracle. Production package entries must never export this file.
 */

import type { SpaghettiAPI } from './api.js';
import type { SpaghettiLive } from './live/spaghetti-live.js';

type LegacyQueryKey =
  | 'getSourceIds'
  | 'getProjectList'
  | 'getProjectTokenActivity'
  | 'getSessionList'
  | 'getSessionMessages'
  | 'getSessionTimelineFacets'
  | 'getSessionTimeline'
  | 'getProjectMemory'
  | 'getSessionTodos'
  | 'getSessionPlan'
  | 'getSessionTask'
  | 'getToolResult'
  | 'getSessionSubagents'
  | 'getSessionWorkflows'
  | 'getWorkflowSubagents'
  | 'getSubagentMessages'
  | 'getSubagentTimeline'
  | 'search'
  | 'getStats'
  | 'getTeams';

type SynchronousMethod<T> = T extends (...args: infer Args) => Promise<infer Result>
  ? (...args: Args) => Result
  : never;

export type LegacySpaghettiAPI = Omit<SpaghettiAPI, LegacyQueryKey> & {
  [Key in LegacyQueryKey]: SynchronousMethod<SpaghettiAPI[Key]>;
} & {
  readonly live?: SpaghettiLive;
};

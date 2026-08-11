/**
 * TypeScript interfaces for data structures found in:
 *   ~/.claude/teams/
 */

// Team configuration (config.json in each team directory)
export interface TeamConfig {
  name: string;
  description?: string;
  createdAt: number;
  leadAgentId: string;
  leadSessionId: string;
  members: TeamMember[];
}

export interface TeamMember {
  /** `{name}@{team}` */
  agentId: string;
  name: string;
  /** Set on the lead ('team-lead'); absent on spawned members */
  agentType?: string;
  model?: string;
  prompt?: string;
  color?: string;
  planModeRequired?: boolean;
  joinedAt: number;
  /** '' | 'leader' | 'in-process' | a real tmux pane id */
  tmuxPaneId: string;
  cwd: string;
  subscriptions: string[];
  backendType?: string;
}

// Inbox messages (inboxes/*.json files)
export interface InboxMessage {
  from: string;
  text: string;
  summary?: string;
  timestamp: string;
  color?: string;
  read: boolean;
  /** Native stable identity on newer Claude inbox entries. */
  msg_id?: string;
  /** Native inbox message schema version (currently 1 when present). */
  msgV?: number;
  /** Native envelope kind (currently `message` when present). */
  type?: string;
}

// Task assignment payload (embedded in inbox message text as JSON)
export interface TaskAssignmentPayload {
  type: 'task_assignment';
  taskId: string;
  subject: string;
  description: string;
  assignedBy: string;
  timestamp: string;
}

// Top-level team directory entry
export interface TeamDirectory {
  teamId: string;
  config: TeamConfig | null;
  inboxes: Record<string, InboxMessage[]>;
}

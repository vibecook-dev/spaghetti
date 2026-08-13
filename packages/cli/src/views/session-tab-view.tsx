/**
 * SessionTabView — Tab container showing Messages + Todos + Plan + Subagents + Team + Workflow.
 *
 * Wraps MessagesView (tab 0) and inline panels for Todos, Plan, Subagents, Team, Workflow (tabs 1-5).
 * The Team tab lists members of the team(s) this session leads; Enter opens a member's inbox.
 * The Workflow tab lists this session's agent-orchestration runs; Enter drills into a run's grouped
 * subagent transcripts, and Enter again opens a transcript.
 * Left/Right arrows switch tabs. Esc pops back to the sessions list.
 */

import React, { useState, useMemo } from 'react';
import { Box, Text, useInput, useStdout } from 'ink';
import type {
  ProjectListItem,
  SessionListItem,
  SubagentListItem,
  WorkflowListItem,
  TeamDirectory,
  InboxMessage,
} from '@vibecook/spaghetti-sdk';
import { useViewNav } from './context.js';
import { useApi } from './shell.js';
import { TabBar } from './tab-bar.js';
import { HRule } from './chrome.js';
import { MessagesView } from './messages-view.js';
import { formatNumber, formatRelativeTime } from '../lib/format.js';
import type { ViewEntry } from './types.js';
import { useAsyncValue } from './hooks.js';

// ─── Constants ────────────────────────────────────────────────────────

const TABS = ['Messages', 'Todos', 'Plan', 'Subagents', 'Team', 'Workflow'] as const;

// ─── Todo Helpers ─────────────────────────────────────────────────────

interface TodoItem {
  content: string;
  status: string;
}

function parseTodo(item: unknown): TodoItem {
  if (typeof item === 'object' && item !== null) {
    const obj = item as Record<string, unknown>;
    return {
      content: typeof obj.content === 'string' ? obj.content : String(obj.content ?? ''),
      status: typeof obj.status === 'string' ? obj.status : 'unknown',
    };
  }
  return { content: String(item), status: 'unknown' };
}

// ─── Plan Helpers ─────────────────────────────────────────────────────

function extractPlanContent(plan: unknown): string | null {
  if (plan === null || plan === undefined) return null;
  if (typeof plan === 'string') return plan;
  if (typeof plan === 'object') {
    const obj = plan as Record<string, unknown>;
    if (typeof obj.content === 'string') return obj.content;
    if (typeof obj.plan === 'string') return obj.plan;
    if (typeof obj.text === 'string') return obj.text;
    return JSON.stringify(plan, null, 2);
  }
  return String(plan);
}

function renderMarkdownLines(content: string): React.ReactElement[] {
  return content.split('\n').map((line, i) => {
    const trimmed = line.trimStart();
    if (trimmed.startsWith('#')) {
      const text = trimmed.replace(/^#+\s*/, '');
      return (
        <Text key={i} bold>
          {' '}
          {text}
        </Text>
      );
    }
    return (
      <Text key={i} dimColor={trimmed === ''}>
        {' '}
        {line}
      </Text>
    );
  });
}

// ─── TodosPanel ───────────────────────────────────────────────────────

interface TodosPanelProps {
  rawTodos: unknown[];
  scrollOffset: number;
  viewportHeight: number;
}

function TodosPanel({ rawTodos, scrollOffset, viewportHeight }: TodosPanelProps): React.ReactElement {
  const { stdout } = useStdout();
  const cols = stdout?.columns ?? 80;

  const todos = useMemo(() => rawTodos.map(parseTodo), [rawTodos]);

  if (todos.length === 0) {
    return (
      <Box flexDirection="column" paddingLeft={2}>
        <Text dimColor>No todos in this session.</Text>
      </Box>
    );
  }

  const visibleTodos = todos.slice(scrollOffset, scrollOffset + viewportHeight);

  return (
    <Box flexDirection="column">
      {visibleTodos.map((todo, i) => {
        const maxLen = Math.max(cols - 8, 20);
        const text = todo.content.length > maxLen ? todo.content.slice(0, maxLen - 1) + '\u2026' : todo.content;

        switch (todo.status) {
          case 'completed':
            return (
              <Text key={scrollOffset + i}>
                {' '}
                <Text color="green">{'\u2713'}</Text> <Text dimColor>{text}</Text>
              </Text>
            );
          case 'in_progress':
            return (
              <Text key={scrollOffset + i}>
                {' '}
                <Text color="yellow">{'\u25D0'}</Text> <Text color="yellow">{text}</Text>
              </Text>
            );
          case 'pending':
            return (
              <Text key={scrollOffset + i}>
                {' '}
                <Text color="white">{'\u25CB'}</Text> <Text>{text}</Text>
              </Text>
            );
          default:
            return (
              <Text key={scrollOffset + i}>
                {' '}
                <Text dimColor>{'\u00B7'}</Text> <Text dimColor>{text}</Text>
              </Text>
            );
        }
      })}
    </Box>
  );
}

// ─── PlanPanel ────────────────────────────────────────────────────────

interface PlanPanelProps {
  rawPlan: unknown | null;
  scrollOffset: number;
  viewportHeight: number;
}

function PlanPanel({ rawPlan, scrollOffset, viewportHeight }: PlanPanelProps): React.ReactElement {
  const content = useMemo(() => extractPlanContent(rawPlan), [rawPlan]);
  const lines = useMemo(() => (content ? renderMarkdownLines(content) : []), [content]);

  if (!content) {
    return (
      <Box flexDirection="column" paddingLeft={2}>
        <Text dimColor>No plan in this session.</Text>
      </Box>
    );
  }

  const visibleLines = lines.slice(scrollOffset, scrollOffset + viewportHeight);

  return <Box flexDirection="column">{visibleLines}</Box>;
}

// ─── SubagentsPanel ───────────────────────────────────────────────────

interface SubagentsPanelProps {
  subagents: SubagentListItem[];
  scrollOffset: number;
  selectedIndex: number;
  viewportHeight: number;
}

function SubagentsPanelCard({ agent, selected }: { agent: SubagentListItem; selected: boolean }): React.ReactElement {
  const prefix = selected ? '\x1b[35m\u258E\x1b[0m' : ' '; // magenta or space
  const typeName = selected ? `\x1b[1m\x1b[37m${agent.agentType}\x1b[0m` : `\x1b[35m${agent.agentType}\x1b[0m`;
  const agentId = `\x1b[2m${agent.agentId}\x1b[0m`;
  const msgCount = `\x1b[2m${formatNumber(agent.messageCount)} messages\x1b[0m`;

  return (
    <Box flexDirection="column">
      <Text>{`${prefix} ${typeName}  ${agentId}`}</Text>
      <Text>{`    ${msgCount}`}</Text>
      <Text> </Text>
    </Box>
  );
}

function SubagentsPanel({
  subagents,
  scrollOffset,
  selectedIndex,
  viewportHeight,
}: SubagentsPanelProps): React.ReactElement {
  if (subagents.length === 0) {
    return (
      <Box flexDirection="column" paddingLeft={2}>
        <Text dimColor>No subagents in this session.</Text>
      </Box>
    );
  }

  const viewportItems = Math.max(1, Math.floor(viewportHeight / 3));
  const visibleAgents = subagents.slice(scrollOffset, scrollOffset + viewportItems);

  return (
    <Box flexDirection="column">
      {visibleAgents.map((agent, i) => {
        const actualIndex = scrollOffset + i;
        return <SubagentsPanelCard key={agent.agentId} agent={agent} selected={actualIndex === selectedIndex} />;
      })}
    </Box>
  );
}

// ─── Team Helpers ─────────────────────────────────────────────────────

/**
 * Teams a session is bound to. `leadSessionId` is the primary key;
 * the two teamId fallbacks catch config-less orphan dirs written under
 * the lead session UUID and the implicit `session-{8hex}` scaffolds.
 */
function matchTeamsForSession(all: TeamDirectory[], sessionId: string): TeamDirectory[] {
  return all.filter(
    (t) =>
      t.config?.leadSessionId === sessionId ||
      t.teamId === sessionId ||
      t.teamId === `session-${sessionId.slice(0, 8)}`,
  );
}

interface TeamMemberRow {
  teamName: string;
  memberName: string;
  agentId: string;
  model?: string;
  color?: string;
  isLead: boolean;
  inbox: InboxMessage[];
}

function buildTeamRows(teams: TeamDirectory[]): TeamMemberRow[] {
  const rows: TeamMemberRow[] = [];

  for (const team of teams) {
    const teamName = team.config?.name ?? team.teamId;
    const seen = new Set<string>();

    for (const member of team.config?.members ?? []) {
      seen.add(member.name);
      rows.push({
        teamName,
        memberName: member.name,
        agentId: member.agentId,
        model: member.model,
        color: member.color,
        isLead: member.agentId === team.config?.leadAgentId,
        inbox: team.inboxes[member.name] ?? [],
      });
    }

    // Inbox files without a roster entry (departed members, orphan dirs).
    for (const [name, messages] of Object.entries(team.inboxes)) {
      if (seen.has(name)) continue;
      rows.push({
        teamName,
        memberName: name,
        agentId: `${name}@${teamName}`,
        isLead: false,
        inbox: messages,
      });
    }
  }

  return rows;
}

// ─── TeamPanel ────────────────────────────────────────────────────────

interface TeamPanelProps {
  teams: TeamDirectory[];
  rows: TeamMemberRow[];
  scrollOffset: number;
  selectedIndex: number;
  viewportHeight: number;
}

function TeamPanelCard({ row, selected }: { row: TeamMemberRow; selected: boolean }): React.ReactElement {
  const nameColor = row.color ?? 'magenta';
  const lead = row.isLead ? ' (lead)' : '';
  const detail = [row.model, `${formatNumber(row.inbox.length)} inbox message${row.inbox.length === 1 ? '' : 's'}`]
    .filter(Boolean)
    .join(' · ');

  return (
    <Box flexDirection="column">
      <Text>
        {selected ? <Text color="magenta">{'▎'}</Text> : ' '}{' '}
        {selected ? (
          <Text bold color="white">
            {row.memberName}
          </Text>
        ) : (
          <Text color={nameColor}>{row.memberName}</Text>
        )}
        {'  '}
        <Text dimColor>
          {row.agentId}
          {lead}
        </Text>
      </Text>
      <Text>
        {'    '}
        <Text dimColor>{detail}</Text>
      </Text>
      <Text> </Text>
    </Box>
  );
}

function TeamPanel({ teams, rows, scrollOffset, selectedIndex, viewportHeight }: TeamPanelProps): React.ReactElement {
  if (rows.length === 0) {
    return (
      <Box flexDirection="column" paddingLeft={2}>
        <Text dimColor>No team in this session.</Text>
      </Box>
    );
  }

  const first = teams[0];
  const totalMessages = rows.reduce((sum, r) => sum + r.inbox.length, 0);
  const headerName = teams.length === 1 ? (first.config?.name ?? first.teamId) : `${teams.length} teams`;
  const description = teams.length === 1 ? first.config?.description : undefined;

  const viewportItems = Math.max(1, Math.floor((viewportHeight - 2) / 3));
  const visibleRows = rows.slice(scrollOffset, scrollOffset + viewportItems);

  return (
    <Box flexDirection="column">
      <Text>
        {' '}
        <Text bold>{headerName}</Text>
        <Text dimColor>
          {' · '}
          {rows.length} member{rows.length === 1 ? '' : 's'}
          {' · '}
          {formatNumber(totalMessages)} inbox message{totalMessages === 1 ? '' : 's'}
        </Text>
        {description ? (
          <Text dimColor>
            {' '}
            {'—'} {description}
          </Text>
        ) : null}
      </Text>
      <Text> </Text>
      {visibleRows.map((row, i) => {
        const actualIndex = scrollOffset + i;
        return (
          <TeamPanelCard key={`${row.teamName}/${row.memberName}`} row={row} selected={actualIndex === selectedIndex} />
        );
      })}
    </Box>
  );
}

// ─── TeamInboxView ────────────────────────────────────────────────────

function TeamInboxView({ row }: { row: TeamMemberRow }): React.ReactElement {
  const nav = useViewNav();
  const { stdout } = useStdout();
  const termRows = stdout?.rows ?? 24;
  const viewportHeight = Math.max(termRows - 8, 5);

  const [scroll, setScroll] = useState(0);

  const lines = useMemo(() => {
    const out: React.ReactElement[] = [];
    row.inbox.forEach((msg, m) => {
      out.push(
        <Text key={`${m}-h`}>
          {' '}
          {!msg.read && <Text color="yellow">{'● '}</Text>}
          <Text color={msg.color ?? 'cyan'}>{msg.from}</Text>
          <Text dimColor>
            {' '}
            {'·'} {formatRelativeTime(msg.timestamp)}
          </Text>
        </Text>,
      );
      if (msg.summary) {
        out.push(
          <Text key={`${m}-s`} bold>
            {'   '}
            {msg.summary}
          </Text>,
        );
      }
      msg.text.split('\n').forEach((line, l) => {
        out.push(
          <Text key={`${m}-t${l}`} dimColor={line.trim() === ''}>
            {'   '}
            {line}
          </Text>,
        );
      });
      out.push(<Text key={`${m}-b`}> </Text>);
    });
    return out;
  }, [row.inbox]);

  const maxScroll = Math.max(0, lines.length - viewportHeight);

  useInput(
    (_input, key) => {
      if (key.upArrow) {
        setScroll((prev) => Math.max(0, prev - 1));
      } else if (key.downArrow) {
        setScroll((prev) => Math.min(maxScroll, prev + 1));
      } else if (key.escape) {
        nav.pop();
      }
    },
    { isActive: !nav.searchMode },
  );

  if (row.inbox.length === 0) {
    return (
      <Box flexDirection="column" paddingLeft={2} marginTop={1}>
        <Text dimColor>Inbox for {row.memberName} is empty.</Text>
      </Box>
    );
  }

  return <Box flexDirection="column">{lines.slice(scroll, scroll + viewportHeight)}</Box>;
}

// ─── WorkflowPanel ────────────────────────────────────────────────────

interface WorkflowPanelProps {
  workflows: WorkflowListItem[];
  scrollOffset: number;
  selectedIndex: number;
  viewportHeight: number;
}

function WorkflowPanelCard({ wf, selected }: { wf: WorkflowListItem; selected: boolean }): React.ReactElement {
  const statusColor = wf.status === 'completed' ? 'green' : wf.status === 'failed' ? 'red' : 'yellow';
  const plural = (n: number, unit: string) => `${formatNumber(n)} ${unit}${n === 1 ? '' : 's'}`;
  const detail = [
    plural(wf.agentCount, 'agent'),
    plural(wf.subagentCount, 'transcript'),
    plural(wf.totalTokens, 'token'),
    plural(wf.totalToolCalls, 'tool call'),
  ].join(' · ');

  return (
    <Box flexDirection="column">
      <Text>
        {selected ? <Text color="magenta">{'▎'}</Text> : ' '}{' '}
        {selected ? (
          <Text bold color="white">
            {wf.name}
          </Text>
        ) : (
          <Text color="magenta">{wf.name}</Text>
        )}
        {'  '}
        <Text color={statusColor}>{wf.status}</Text>
        {'  '}
        <Text dimColor>{wf.workflowId}</Text>
      </Text>
      <Text>
        {'    '}
        <Text dimColor>{detail}</Text>
      </Text>
      <Text> </Text>
    </Box>
  );
}

function WorkflowPanel({
  workflows,
  scrollOffset,
  selectedIndex,
  viewportHeight,
}: WorkflowPanelProps): React.ReactElement {
  if (workflows.length === 0) {
    return (
      <Box flexDirection="column" paddingLeft={2}>
        <Text dimColor>No workflows in this session.</Text>
      </Box>
    );
  }

  const viewportItems = Math.max(1, Math.floor(viewportHeight / 3));
  const visible = workflows.slice(scrollOffset, scrollOffset + viewportItems);

  return (
    <Box flexDirection="column">
      {visible.map((wf, i) => (
        <WorkflowPanelCard key={wf.workflowId} wf={wf} selected={scrollOffset + i === selectedIndex} />
      ))}
    </Box>
  );
}

// ─── WorkflowDetailView — a run's grouped subagents ───────────────────

function WorkflowDetailView({
  projectSlug,
  sessionId,
  workflow,
}: {
  projectSlug: string;
  sessionId: string;
  workflow: WorkflowListItem;
}): React.ReactElement {
  const nav = useViewNav();
  const api = useApi();
  const { stdout } = useStdout();
  const viewportHeight = Math.max((stdout?.rows ?? 24) - 8, 5);

  const loaded = useAsyncValue(
    () => api.getWorkflowSubagents(projectSlug, sessionId, workflow.workflowId),
    [api, projectSlug, sessionId, workflow.workflowId],
    [] as SubagentListItem[],
  );
  const agents = loaded.value;
  const [selected, setSelected] = useState(0);
  const [scroll, setScroll] = useState(0);
  const viewportItems = Math.max(1, Math.floor(viewportHeight / 3));
  const maxScroll = Math.max(0, agents.length - viewportItems);

  useInput(
    (_input, key) => {
      if (key.upArrow) {
        setSelected((prev) => {
          const next = Math.max(0, prev - 1);
          if (next < scroll) setScroll(next);
          return next;
        });
      } else if (key.downArrow) {
        setSelected((prev) => {
          const next = Math.min(agents.length - 1, prev + 1);
          if (next >= scroll + viewportItems) setScroll(Math.min(maxScroll, scroll + 1));
          return next;
        });
      } else if (key.return) {
        const agent = agents[selected];
        if (agent) {
          nav.push({
            type: 'detail',
            component: () => <SubagentTranscriptPlaceholder agentId={agent.agentId} agentType={agent.agentType} />,
            breadcrumb: `${agent.agentType} ${agent.agentId.slice(0, 8)}`,
            hints: 'Esc back',
          });
        }
      } else if (key.escape) {
        nav.pop();
      }
    },
    { isActive: !nav.searchMode },
  );

  if (loaded.loading) return <Text dimColor> Loading canonical workflow…</Text>;
  if (loaded.error) return <Text color="red"> {loaded.error.message}</Text>;

  return (
    <Box flexDirection="column">
      <Text>
        {' '}
        <Text bold>{workflow.name}</Text>
        <Text dimColor>
          {' · '}
          {workflow.status}
          {' · '}
          {agents.length} subagent{agents.length === 1 ? '' : 's'}
        </Text>
      </Text>
      <Text> </Text>
      {agents.length === 0 ? (
        <Text dimColor>{'  '}No subagent transcripts recorded for this run.</Text>
      ) : (
        agents
          .slice(scroll, scroll + viewportItems)
          .map((agent, i) => (
            <SubagentsPanelCard key={agent.agentId} agent={agent} selected={scroll + i === selected} />
          ))
      )}
    </Box>
  );
}

// ─── SessionTabView ───────────────────────────────────────────────────

export interface SessionTabViewProps {
  project: ProjectListItem;
  session: SessionListItem;
  sessionIndex: number;
}

export function SessionTabView({ project, session, sessionIndex }: SessionTabViewProps): React.ReactElement {
  const nav = useViewNav();
  const api = useApi();
  const { stdout } = useStdout();
  const termRows = stdout?.rows ?? 24;

  const [activeTab, setActiveTab] = useState(0);

  // Scroll state for tabs 1-4 (Todos, Plan, Subagents, Team)
  const [todosScroll, setTodosScroll] = useState(0);
  const [planScroll, setPlanScroll] = useState(0);
  const [subagentsScroll, setSubagentsScroll] = useState(0);
  const [subagentsSelected, setSubagentsSelected] = useState(0);
  const [teamScroll, setTeamScroll] = useState(0);
  const [teamSelected, setTeamSelected] = useState(0);
  const [workflowScroll, setWorkflowScroll] = useState(0);
  const [workflowSelected, setWorkflowSelected] = useState(0);

  // Compute max scroll values for tabs 1-5
  const viewportHeight = Math.max(termRows - 8, 5);

  const loaded = useAsyncValue(
    async () => {
      const [rawTodos, rawPlan, subagents, teams, workflows] = await Promise.all([
        api.getSessionTodos(session.projectSlug, session.sessionId),
        api.getSessionPlan(session.projectSlug, session.sessionId),
        api.getSessionSubagents(session.projectSlug, session.sessionId),
        api.getTeams(),
        api.getSessionWorkflows(session.projectSlug, session.sessionId),
      ]);
      return { rawTodos, rawPlan, subagents, teams, workflows };
    },
    [api, session.projectSlug, session.sessionId],
    {
      rawTodos: [] as unknown[],
      rawPlan: null as unknown | null,
      subagents: [] as SubagentListItem[],
      teams: [] as TeamDirectory[],
      workflows: [] as WorkflowListItem[],
    },
  );
  const { rawTodos, rawPlan, subagents, teams, workflows } = loaded.value;
  const todosCount = rawTodos.length;
  const todosMaxScroll = Math.max(0, todosCount - viewportHeight);

  const planContent = useMemo(() => extractPlanContent(rawPlan), [rawPlan]);
  const planLineCount = useMemo(() => (planContent ? planContent.split('\n').length : 0), [planContent]);
  const planMaxScroll = Math.max(0, planLineCount - viewportHeight);

  const subagentsCount = subagents.length;
  const subagentsViewportItems = Math.max(1, Math.floor(viewportHeight / 3));
  const subagentsMaxScroll = Math.max(0, subagentsCount - subagentsViewportItems);

  const sessionTeams = useMemo(() => matchTeamsForSession(teams, session.sessionId), [teams, session.sessionId]);
  const teamRows = useMemo(() => buildTeamRows(sessionTeams), [sessionTeams]);
  const teamRowCount = teamRows.length;
  const teamViewportItems = Math.max(1, Math.floor((viewportHeight - 2) / 3));
  const teamMaxScroll = Math.max(0, teamRowCount - teamViewportItems);

  const workflowCount = workflows.length;
  const workflowViewportItems = Math.max(1, Math.floor(viewportHeight / 3));
  const workflowMaxScroll = Math.max(0, workflowCount - workflowViewportItems);

  // Tab switching — always active (works on all tabs including Messages)
  useInput(
    (_input, key) => {
      if (key.leftArrow) {
        setActiveTab((prev) => Math.max(0, prev - 1));
      } else if (key.rightArrow) {
        setActiveTab((prev) => Math.min(TABS.length - 1, prev + 1));
      }
    },
    { isActive: !nav.searchMode },
  );

  // Key handling for non-Messages tabs (Todos, Plan, Subagents)
  // When Messages tab is active, MessagesView handles its own keys.
  useInput(
    (_input, key) => {
      if (activeTab === 1) {
        // Todos tab — scroll only
        if (key.upArrow) {
          setTodosScroll((prev) => Math.max(0, prev - 1));
        } else if (key.downArrow) {
          setTodosScroll((prev) => Math.min(todosMaxScroll, prev + 1));
        } else if (key.escape) {
          nav.pop();
        }
      } else if (activeTab === 2) {
        // Plan tab — scroll only
        if (key.upArrow) {
          setPlanScroll((prev) => Math.max(0, prev - 1));
        } else if (key.downArrow) {
          setPlanScroll((prev) => Math.min(planMaxScroll, prev + 1));
        } else if (key.escape) {
          nav.pop();
        }
      } else if (activeTab === 3) {
        // Subagents tab — list navigation + Enter for detail
        if (key.upArrow) {
          setSubagentsSelected((prev) => {
            const next = Math.max(0, prev - 1);
            // Adjust scroll if selection goes above viewport
            if (next < subagentsScroll) {
              setSubagentsScroll(next);
            }
            return next;
          });
        } else if (key.downArrow) {
          setSubagentsSelected((prev) => {
            const next = Math.min(subagentsCount - 1, prev + 1);
            // Adjust scroll if selection goes below viewport
            if (next >= subagentsScroll + subagentsViewportItems) {
              setSubagentsScroll(Math.min(subagentsMaxScroll, subagentsScroll + 1));
            }
            return next;
          });
        } else if (key.return) {
          if (subagentsCount === 0) return;
          const agent = subagents[subagentsSelected];
          if (!agent) return;

          const entry: ViewEntry = {
            type: 'detail',
            component: () => <SubagentTranscriptPlaceholder agentId={agent.agentId} agentType={agent.agentType} />,
            breadcrumb: `${agent.agentType} ${agent.agentId.slice(0, 8)}`,
            hints: 'Esc back',
          };
          nav.push(entry);
        } else if (key.escape) {
          nav.pop();
        }
      } else if (activeTab === 4) {
        // Team tab — member list navigation + Enter for inbox
        if (key.upArrow) {
          setTeamSelected((prev) => {
            const next = Math.max(0, prev - 1);
            if (next < teamScroll) {
              setTeamScroll(next);
            }
            return next;
          });
        } else if (key.downArrow) {
          setTeamSelected((prev) => {
            const next = Math.min(teamRowCount - 1, prev + 1);
            if (next >= teamScroll + teamViewportItems) {
              setTeamScroll(Math.min(teamMaxScroll, teamScroll + 1));
            }
            return next;
          });
        } else if (key.return) {
          if (teamRowCount === 0) return;
          const row = teamRows[teamSelected];
          if (!row) return;

          const entry: ViewEntry = {
            type: 'detail',
            component: () => <TeamInboxView row={row} />,
            breadcrumb: `${row.memberName} inbox`,
            hints: '↑↓ scroll · Esc back',
          };
          nav.push(entry);
        } else if (key.escape) {
          nav.pop();
        }
      } else if (activeTab === 5) {
        // Workflow tab — run list navigation + Enter for the run's subagents
        if (key.upArrow) {
          setWorkflowSelected((prev) => {
            const next = Math.max(0, prev - 1);
            if (next < workflowScroll) setWorkflowScroll(next);
            return next;
          });
        } else if (key.downArrow) {
          setWorkflowSelected((prev) => {
            const next = Math.min(workflowCount - 1, prev + 1);
            if (next >= workflowScroll + workflowViewportItems) {
              setWorkflowScroll(Math.min(workflowMaxScroll, workflowScroll + 1));
            }
            return next;
          });
        } else if (key.return) {
          if (workflowCount === 0) return;
          const wf = workflows[workflowSelected];
          if (!wf) return;

          const entry: ViewEntry = {
            type: 'detail',
            component: () => (
              <WorkflowDetailView projectSlug={session.projectSlug} sessionId={session.sessionId} workflow={wf} />
            ),
            breadcrumb: `${wf.name}`,
            hints: '↑↓ select · ⏎ transcript · Esc back',
          };
          nav.push(entry);
        } else if (key.escape) {
          nav.pop();
        }
      }
    },
    { isActive: activeTab !== 0 && !nav.searchMode },
  );

  // Build breadcrumb for the tab bar
  const tabBreadcrumb = `${project.folderName} \u203A #${sessionIndex + 1}`;

  return (
    <Box flexDirection="column">
      <TabBar tabs={[...TABS]} activeIndex={activeTab} onTabChange={setActiveTab} breadcrumb={tabBreadcrumb} />
      {activeTab === 0 && <MessagesView project={project} session={session} sessionIndex={sessionIndex} />}
      {activeTab !== 0 && <HRule />}
      {activeTab !== 0 && loaded.loading && <Text dimColor> Loading canonical session details…</Text>}
      {activeTab !== 0 && loaded.error && <Text color="red"> {loaded.error.message}</Text>}
      {activeTab === 1 && !loaded.loading && !loaded.error && (
        <TodosPanel rawTodos={rawTodos} scrollOffset={todosScroll} viewportHeight={viewportHeight} />
      )}
      {activeTab === 2 && !loaded.loading && !loaded.error && (
        <PlanPanel rawPlan={rawPlan} scrollOffset={planScroll} viewportHeight={viewportHeight} />
      )}
      {activeTab === 3 && !loaded.loading && !loaded.error && (
        <SubagentsPanel
          subagents={subagents}
          scrollOffset={subagentsScroll}
          selectedIndex={subagentsSelected}
          viewportHeight={viewportHeight}
        />
      )}
      {activeTab === 4 && !loaded.loading && !loaded.error && (
        <TeamPanel
          teams={sessionTeams}
          rows={teamRows}
          scrollOffset={teamScroll}
          selectedIndex={teamSelected}
          viewportHeight={viewportHeight}
        />
      )}
      {activeTab === 5 && !loaded.loading && !loaded.error && (
        <WorkflowPanel
          workflows={workflows}
          scrollOffset={workflowScroll}
          selectedIndex={workflowSelected}
          viewportHeight={viewportHeight}
        />
      )}
    </Box>
  );
}

// ─── Placeholder for subagent transcript ──────────────────────────────

function SubagentTranscriptPlaceholder({
  agentId,
  agentType,
}: {
  agentId: string;
  agentType: string;
}): React.ReactElement {
  const nav = useViewNav();

  useInput(
    (_input, key) => {
      if (key.escape) {
        nav.pop();
      }
    },
    { isActive: !nav.searchMode },
  );

  return (
    <Box flexDirection="column" paddingLeft={2} marginTop={1}>
      <Text>
        Subagent: <Text bold>{agentType}</Text> <Text dimColor>{agentId}</Text>
      </Text>
      <Text> </Text>
      <Text dimColor>Subagent transcript view coming soon.</Text>
    </Box>
  );
}

/**
 * Chat Theme Configuration
 *
 * All colors, sizes, and style constants for the chat components.
 * Edit this file to customize the appearance of the chat UI.
 */

// =============================================================================
// TIMELINE SETTINGS
// =============================================================================

export const timeline = {
  /** Size of the timeline dots (px) */
  dotSize: 24,

  /** Large dot size for assistant messages (px) */
  dotSizeLarge: 32,

  /** Inner glow opacity (0-100) */
  glowInnerOpacity: 30,

  /** Outer glow opacity (0-100) */
  glowOuterOpacity: 0,

  /** Inner glow gradient stop (%) */
  glowInnerStop: 0,

  /** Outer glow gradient stop (%) */
  glowOuterStop: 100,

  /** Glow blur amount (px) */
  glowBlur: 8,

  /** Glow scale multiplier */
  glowSpread: 1.5,

  // === SVG Timeline Layout ===
  /** X position for main thread line (px from left) */
  mainX: 24,

  /** X position for agent/sub-thread line (px from left) */
  indentX: 64,

  /** Y offset from top of row to icon center (px) */
  iconOffsetY: 28,

  /** Width of the rail column (px) */
  railWidth: 48,

  /** Connector line stroke width */
  lineWidth: 2,

  /** Agent thread color */
  agentColor: '#0EA5E9',

  /** Main thread color */
  mainColor: 'var(--border)',

  /** Dash pattern for curved connectors */
  dashPattern: '4 4',

  /** Animation duration for path transitions (ms) */
  animationDuration: 500,

  /** Connector glow opacity */
  connectorGlowOpacity: 0.2,

  /** Connector stroke opacity */
  connectorStrokeOpacity: 0.8,
} as const;

// =============================================================================
// COLORS - MESSAGE TYPES
// =============================================================================

export const messageColors = {
  /** User message bubble background */
  userBg: '#D97757',

  /** Assistant message dot/accent */
  assistantBg: '#D97757',

  /** Compact summary message */
  summary: '#d946ef',
} as const;

// =============================================================================
// COLORS - TOOL TYPES
// =============================================================================

export const toolColors = {
  /** Bash/Terminal commands */
  bash: '#8b5cf6',

  /** File read operations */
  read: '#06b6d4',

  /** File write/edit operations */
  write: '#f97316',

  /** Search operations (Grep, Glob) */
  search: '#f43f5e',

  /** Successful tool result */
  success: '#10b981',

  /** Error tool result */
  error: '#ef4444',

  /** Todo/task list */
  todo: '#8b5cf6',

  /** Agent task spawn */
  task: '#0ea5e9',

  /** Web operations (WebFetch, WebSearch) */
  web: '#06b6d4',

  /** Question/prompt operations */
  question: '#f59e0b',

  /** Plan mode operations */
  plan: '#a855f7',

  /** Skill operations */
  skill: '#ec4899',

  /** MCP operations */
  mcp: '#14b8a6',

  /** Notebook operations */
  notebook: '#f97316',

  /** Kill/terminate operations */
  kill: '#ef4444',
} as const;

// =============================================================================
// COLORS - SYNTAX HIGHLIGHTING
// =============================================================================

export const syntaxColors = {
  /** Command/function names */
  command: '#8b5cf6',

  /** Flags and options */
  flag: '#f59e0b',

  /** String literals */
  string: '#22c55e',

  /** Pipes, redirects, operators */
  operator: '#8b5cf6',

  /** File paths */
  path: '#06b6d4',

  /** Added lines in diff */
  added: '#22c55e',

  /** Removed lines in diff */
  removed: '#ef4444',

  /** Wildcard characters in glob */
  wildcard: '#f59e0b',
} as const;

// =============================================================================
// COLORS - FILE TYPE ICONS
// =============================================================================

export const fileTypeColors: Record<string, string> = {
  // TypeScript
  ts: '#3178c6',
  tsx: '#3178c6',

  // JavaScript
  js: '#f7df1e',
  jsx: '#f7df1e',
  mjs: '#f7df1e',
  cjs: '#f7df1e',

  // Python
  py: '#3776ab',

  // Rust
  rs: '#dea584',

  // Go
  go: '#00add8',

  // JSON/Config
  json: '#cbcb41',
  yaml: '#cb171e',
  yml: '#cb171e',
  toml: '#9c4121',

  // Markdown/Docs
  md: '#083fa1',
  mdx: '#083fa1',

  // Styles
  css: '#264de4',
  scss: '#cc6699',
  sass: '#cc6699',
  less: '#1d365d',

  // HTML
  html: '#e34c26',
  htm: '#e34c26',

  // Shell
  sh: '#89e051',
  bash: '#89e051',
  zsh: '#89e051',

  // Default fallback
  default: 'var(--muted-foreground)',
} as const;

// =============================================================================
// COLORS - STATUS INDICATORS
// =============================================================================

export const statusColors = {
  completed: {
    bg: 'rgba(16, 185, 129, 0.1)',
    border: 'rgba(16, 185, 129, 0.3)',
    text: '#10b981',
  },
  in_progress: {
    bg: 'rgba(245, 158, 11, 0.1)',
    border: 'rgba(245, 158, 11, 0.3)',
    text: '#f59e0b',
  },
  pending: {
    bg: 'transparent',
    border: 'var(--border)',
    text: 'var(--muted-foreground)',
  },
} as const;

// =============================================================================
// TYPOGRAPHY
// =============================================================================

export const typography = {
  /** Monospace font stack */
  mono: 'ui-monospace, "SF Mono", "Monaco", "Inconsolata", "Fira Mono", "Droid Sans Mono", "Source Code Pro", monospace',

  /** Font sizes */
  sizes: {
    xs: '9px',
    sm: '10px',
    base: '11px',
    md: '12px',
    lg: '13px',
  },
} as const;

// =============================================================================
// TOOL CONFIGURATION
// =============================================================================

export interface ToolConfig {
  color: string;
  label: string;
}

export const toolConfig: Record<string, ToolConfig> = {
  // File operations
  Bash: { color: toolColors.bash, label: 'Exec' },
  Read: { color: toolColors.read, label: 'Read' },
  Write: { color: toolColors.write, label: 'Write' },
  Edit: { color: toolColors.write, label: 'Edit' },
  NotebookEdit: { color: toolColors.notebook, label: 'Notebook' },
  Grep: { color: toolColors.search, label: 'Search' },
  Glob: { color: toolColors.search, label: 'Glob' },

  // Execution
  Task: { color: toolColors.task, label: 'Task' },
  TaskOutput: { color: toolColors.task, label: 'Output' },
  KillShell: { color: toolColors.kill, label: 'Kill' },

  // Web
  WebFetch: { color: toolColors.web, label: 'Fetch' },
  WebSearch: { color: toolColors.web, label: 'Search' },

  // Session
  TodoWrite: { color: toolColors.todo, label: 'Todo' },
  AskUserQuestion: { color: toolColors.question, label: 'Ask' },
  EnterPlanMode: { color: toolColors.plan, label: 'Plan' },
  ExitPlanMode: { color: toolColors.plan, label: 'Plan' },

  // Skills
  Skill: { color: toolColors.skill, label: 'Skill' },

  // Default
  default: { color: toolColors.bash, label: 'Tool' },
} as const;

// =============================================================================
// HELPER FUNCTIONS
// =============================================================================

/**
 * Convert hex color to rgba
 */
export function hexToRgba(hex: string, opacity: number): string {
  const r = parseInt(hex.slice(1, 3), 16);
  const g = parseInt(hex.slice(3, 5), 16);
  const b = parseInt(hex.slice(5, 7), 16);
  return `rgba(${r}, ${g}, ${b}, ${opacity / 100})`;
}

/**
 * Get color for a file type based on extension
 */
export function getFileTypeColor(filename: string): string {
  const ext = filename.split('.').pop()?.toLowerCase() || '';
  return fileTypeColors[ext] || fileTypeColors.default;
}

/**
 * Get tool configuration by name
 */
export function getToolConfig(name: string): ToolConfig {
  return toolConfig[name] || toolConfig.default;
}

/**
 * Get status colors by status string
 */
export function getStatusColors(status: string): { bg: string; border: string; text: string } {
  return statusColors[status as keyof typeof statusColors] || statusColors.pending;
}

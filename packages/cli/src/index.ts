/**
 * Program setup — exported for testing
 */

import { Command } from 'commander';
import { initService, shutdownService } from './lib/init.js';
import { dashboardCommand } from './commands/dashboard.js';
import { projectsCommand } from './commands/projects.js';
import type { ProjectsOptions } from './commands/projects.js';
import { sessionsCommand } from './commands/sessions.js';
import type { SessionsOptions } from './commands/sessions.js';
import { messagesCommand } from './commands/messages.js';
import type { MessagesOptions } from './commands/messages.js';
import { searchCommand } from './commands/search.js';
import type { SearchOptions } from './commands/search.js';
import { statsCommand } from './commands/stats.js';
import type { StatsOptions } from './commands/stats.js';
import { memoryCommand } from './commands/memory.js';
import type { MemoryOptions } from './commands/memory.js';
import { todosCommand } from './commands/todos.js';
import type { TodosOptions } from './commands/todos.js';
import { subagentsCommand } from './commands/subagents.js';
import type { SubagentsOptions } from './commands/subagents.js';
import { planCommand } from './commands/plan.js';
import type { PlanOptions } from './commands/plan.js';
import { exportCommand } from './commands/export.js';
import type { ExportOptions } from './commands/export.js';
import { doctorCommand } from './commands/doctor.js';
import { engineCommand } from './commands/engine.js';
import { theme } from './lib/color.js';
import { uninstallCommand } from './commands/uninstall.js';
import { updateCommand } from './lib/updater.js';
import { createRequire } from 'node:module';

const _require = createRequire(import.meta.url);
const VERSION = (_require('../package.json') as { version: string }).version;

/**
 * Turn an initialization failure into guidance that matches its actual cause.
 *
 * This used to append "Install a supported agent" to every failure, which is
 * the wrong advice for the most common one: npm 12 blocks install scripts by
 * default, so `better-sqlite3` never fetches its prebuilt binding and every
 * DB-touching command dies. Telling that user to install an agent they already
 * have sends them somewhere there is nothing to find.
 */
export function initFailureHint(msg: string): string {
  if (/bindings file|better_sqlite3\.node|node-gyp|prebuild-install/i.test(msg)) {
    // Every command below was run against a published build before being
    // printed here. `0.6.2` shipped two that were not, and both failed in ways
    // that look like success: `--allow-scripts` on a project-scoped install is
    // a hard `EALLOWSCRIPTS` error, and `install-scripts approve` records the
    // permission in package.json without ever running the script — so it
    // reports "Approved" and changes nothing. Approve only grants; `rebuild`
    // is what executes.
    return [
      "better-sqlite3's native binding is missing — its install script did not run.",
      'npm 12 blocks install scripts by default.',
      '',
      'Fix an existing install (either kind):',
      '',
      '  npm install-scripts approve better-sqlite3',
      '  npm rebuild better-sqlite3',
      '',
      'Or reinstall globally, allowing that one script:',
      '',
      '  npm install -g --allow-scripts=better-sqlite3 @vibecook/spaghetti',
      '',
      // The flag is rejected outright for project-scoped installs, so those
      // have to declare it in the manifest instead.
      'Installing as a project dependency? The flag is refused there — add to package.json:',
      '',
      '  "allowScripts": { "better-sqlite3": true }',
      '',
      'pnpm users: add better-sqlite3 to onlyBuiltDependencies in pnpm-workspace.yaml.',
    ].join('\n');
  }
  if (/root dir not found|not a directory|ENOENT/i.test(msg)) {
    return 'Install a supported agent and re-run. Data roots: ~/.claude, ~/.codex, ~/.grok';
  }
  return 'Re-run with --verbose for the full stack.';
}

/** Helper to initialize the service with standard error handling */
async function withService<T>(fn: (api: Awaited<ReturnType<typeof initService>>) => Promise<T>): Promise<T> {
  let api;
  try {
    api = await initService();
  } catch (err) {
    const msg = err instanceof Error ? err.message : String(err);
    process.stderr.write(
      theme.error('\nFailed to initialize: ') + msg + '\n' + theme.muted(initFailureHint(msg) + '\n\n'),
    );
    if (process.argv.includes('--verbose') && err instanceof Error && err.stack) {
      process.stderr.write(theme.muted(err.stack + '\n'));
    }
    process.exit(1);
  }

  try {
    return await fn(api);
  } finally {
    shutdownService();
  }
}

export function createProgram(): Command {
  const program = new Command();

  program.name('spaghetti').version(VERSION, '-v, --version').description('Local-first agent history explorer');

  // Known commands with their aliases for unknown-command detection
  const knownCommands: Array<{ name: string; alias: string; description: string }> = [
    { name: 'projects', alias: 'p', description: 'List all projects' },
    { name: 'sessions', alias: 's', description: 'Sessions for a project' },
    { name: 'messages', alias: 'm', description: 'Read messages' },
    { name: 'search', alias: '', description: 'Full-text search' },
    { name: 'stats', alias: 'st', description: 'Usage statistics' },
    { name: 'memory', alias: 'mem', description: 'View project MEMORY.md' },
    { name: 'todos', alias: 't', description: 'View session todos' },
    { name: 'subagents', alias: 'sub', description: 'View subagents' },
    { name: 'plan', alias: 'pl', description: 'View session plan' },
    { name: 'export', alias: 'x', description: 'Export project data' },
    { name: 'doctor', alias: '', description: 'Health check for spaghetti and its data paths' },
    { name: 'engine', alias: '', description: 'Show or switch the active ingest engine (ts | rs)' },
    { name: 'uninstall', alias: '', description: 'Show uninstall instructions' },
    { name: 'update', alias: '', description: 'Check for updates and install the latest version' },
  ];

  // Default action: catch unknown commands.
  // Bare `spag` (no args) is handled in bin.ts before commander runs.
  program.argument('[args...]', '').action(async (args: string[]) => {
    // If no arguments, show the static dashboard (fallback for --json or piped usage via commander)
    if (!args || args.length === 0) {
      await withService((api) => dashboardCommand(api, VERSION));
      return;
    }

    // If the first arg looks like a command (not starting with '-'), treat it as unknown
    const attempted = args[0];
    if (!attempted.startsWith('-')) {
      // Check for a close match (simple prefix/substring matching)
      const allNames = knownCommands.flatMap((c) => (c.alias ? [c.name, c.alias] : [c.name]));
      const suggestion = allNames.find((name) => name.startsWith(attempted) || attempted.startsWith(name));

      const lines: string[] = [];
      lines.push('');
      lines.push(theme.error(`Unknown command: "${attempted}"`));

      if (suggestion) {
        const cmd = knownCommands.find((c) => c.name === suggestion || c.alias === suggestion);
        lines.push(theme.warning(`Did you mean "${cmd ? cmd.name : suggestion}"?`));
      }

      lines.push('');
      lines.push(theme.heading('Available commands:'));

      for (const cmd of knownCommands) {
        const aliasStr = cmd.alias ? ` (${cmd.alias})` : '';
        const nameCol = `  ${cmd.name}${aliasStr}`;
        lines.push(`${theme.accent(nameCol.padEnd(20))}${theme.muted(cmd.description)}`);
      }

      lines.push('');
      lines.push(theme.muted("Run 'spaghetti --help' for more info."));
      lines.push('');

      process.stderr.write(lines.join('\n'));
      process.exit(1);
    }

    // Otherwise (e.g. unknown flag), show the dashboard
    await withService((api) => dashboardCommand(api, VERSION));
  });

  // Projects command
  const projectsCmd = new Command('projects')
    .alias('p')
    .description('List all projects')
    .option('-s, --sort <key>', 'Sort by: active, sessions, messages, tokens, name', 'active')
    .option('-l, --limit <n>', 'Limit results', parseInt)
    .option('--json', 'Output as JSON')
    .action(async (cmdOpts: ProjectsOptions) => {
      await withService((api) =>
        projectsCommand(api, {
          sort: cmdOpts.sort,
          limit: cmdOpts.limit,
          json: cmdOpts.json,
        }),
      );
    });

  program.addCommand(projectsCmd);

  // Sessions command
  const sessionsCmd = new Command('sessions')
    .alias('s')
    .description('List sessions for a project')
    .argument('[project]', 'Project name, index, or "." for cwd')
    .option('-s, --sort <field>', 'Sort by: recent, tokens, messages, duration', 'recent')
    .option('-l, --limit <n>', 'Limit results (default: 20)', parseInt)
    .option('-a, --all', 'Show all sessions (no limit)')
    .option('--since <time>', 'Filter by time (today, yesterday, "3 days ago", ISO date)')
    .option('--json', 'Output as JSON')
    .action(async (projectArg: string | undefined, cmdOpts: SessionsOptions) => {
      await withService((api) =>
        sessionsCommand(api, projectArg, {
          sort: cmdOpts.sort,
          limit: cmdOpts.limit,
          all: cmdOpts.all,
          since: cmdOpts.since,
          json: cmdOpts.json,
        }),
      );
    });

  program.addCommand(sessionsCmd);

  // Messages command
  const messagesCmd = new Command('messages')
    .alias('m')
    .description('Read messages from a session')
    .argument('[project]', 'Project name, index, or "." for cwd')
    .argument('[session]', 'Session index (1=latest), "latest", or partial UUID')
    .option('-n, --limit <n>', 'Limit messages (default: 50)', parseInt)
    .option('--offset <n>', 'Start from message N', parseInt)
    .option('--last <n>', 'Show last N messages', parseInt)
    .option('--compact', 'One-line-per-message view')
    .option('--no-tools', 'Hide tool use details')
    .option('--no-thinking', 'Hide thinking blocks')
    .option('--raw', 'Show raw JSON per message')
    .option('--json', 'Full JSON output')
    .action(async (projectArg: string | undefined, sessionArg: string | undefined, cmdOpts: MessagesOptions) => {
      await withService((api) =>
        messagesCommand(api, projectArg, sessionArg, {
          limit: cmdOpts.limit,
          offset: cmdOpts.offset,
          last: cmdOpts.last,
          compact: cmdOpts.compact,
          noTools: cmdOpts.noTools,
          noThinking: cmdOpts.noThinking,
          raw: cmdOpts.raw,
          json: cmdOpts.json,
        }),
      );
    });

  program.addCommand(messagesCmd);

  // Search command
  const searchCmd = new Command('search')
    .description('Full-text search across all data')
    .argument('<query>', 'Search text')
    .option('-p, --project <name>', 'Scope to a specific project (fuzzy resolved)')
    .option('-l, --limit <n>', 'Limit results (default: 20)', parseInt)
    .option('--offset <n>', 'Pagination offset', parseInt)
    .option('--json', 'Output as JSON')
    .action(async (query: string, cmdOpts: SearchOptions) => {
      await withService((api) =>
        searchCommand(api, query, {
          project: cmdOpts.project,
          limit: cmdOpts.limit,
          offset: cmdOpts.offset,
          json: cmdOpts.json,
        }),
      );
    });

  program.addCommand(searchCmd);

  // Stats command
  const statsCmd = new Command('stats')
    .alias('st')
    .description('Usage statistics')
    .option('--json', 'Output as JSON')
    .action(async (cmdOpts: StatsOptions) => {
      await withService((api) =>
        statsCommand(api, {
          json: cmdOpts.json,
        }),
      );
    });

  program.addCommand(statsCmd);

  // Memory command
  const memoryCmd = new Command('memory')
    .alias('mem')
    .description('View project MEMORY.md')
    .argument('[project]', 'Project name, index, or "." for cwd')
    .option('--json', 'Output as JSON')
    .action(async (projectArg: string | undefined, cmdOpts: MemoryOptions) => {
      await withService((api) =>
        memoryCommand(api, projectArg, {
          json: cmdOpts.json,
        }),
      );
    });

  program.addCommand(memoryCmd);

  // Todos command
  const todosCmd = new Command('todos')
    .alias('t')
    .description('View session todos')
    .argument('[project]', 'Project name, index, or "." for cwd')
    .argument('[session]', 'Session index (1=latest), "latest", or partial UUID')
    .option('--json', 'Output as JSON')
    .action(async (projectArg: string | undefined, sessionArg: string | undefined, cmdOpts: TodosOptions) => {
      await withService((api) =>
        todosCommand(api, projectArg, sessionArg, {
          json: cmdOpts.json,
        }),
      );
    });

  program.addCommand(todosCmd);

  // Subagents command
  const subagentsCmd = new Command('subagents')
    .alias('sub')
    .description('View session subagents')
    .argument('[project]', 'Project name, index, or "." for cwd')
    .argument('[session]', 'Session index (1=latest), "latest", or partial UUID')
    .argument('[agent]', 'Agent index to view messages')
    .option('--json', 'Output as JSON')
    .action(
      async (
        projectArg: string | undefined,
        sessionArg: string | undefined,
        agentArg: string | undefined,
        cmdOpts: SubagentsOptions,
      ) => {
        await withService((api) =>
          subagentsCommand(api, projectArg, sessionArg, agentArg, {
            json: cmdOpts.json,
          }),
        );
      },
    );

  program.addCommand(subagentsCmd);

  // Plan command
  const planCmd = new Command('plan')
    .alias('pl')
    .description('View session plan')
    .argument('[project]', 'Project name, index, or "." for cwd')
    .argument('[session]', 'Session index (1=latest), "latest", or partial UUID')
    .option('--json', 'Output as JSON')
    .action(async (projectArg: string | undefined, sessionArg: string | undefined, cmdOpts: PlanOptions) => {
      await withService((api) =>
        planCommand(api, projectArg, sessionArg, {
          json: cmdOpts.json,
        }),
      );
    });

  program.addCommand(planCmd);

  // Export command
  const exportCmd = new Command('export')
    .alias('x')
    .description('Export project data')
    .argument('[project]', 'Project name, index, or "." for cwd')
    .option('-s, --session <id>', 'Export specific session only')
    .option('-f, --format <fmt>', 'Output format: json, markdown', 'json')
    .option('-o, --output <path>', 'Output file path (default: stdout)')
    .option('--include-tools', 'Include full tool result content')
    .option('--json', 'Output as JSON (same as --format json)')
    .action(async (projectArg: string | undefined, cmdOpts: ExportOptions) => {
      await withService((api) =>
        exportCommand(api, projectArg, {
          session: cmdOpts.session,
          format: cmdOpts.format,
          output: cmdOpts.output,
          includeTools: cmdOpts.includeTools,
          json: cmdOpts.json,
        }),
      );
    });

  program.addCommand(exportCmd);

  // Doctor command (does not need SpaghettiAPI)
  const doctorCmd = new Command('doctor')
    .description('Health check for spaghetti and its related data paths')
    .action(async () => {
      await doctorCommand(VERSION);
    });

  program.addCommand(doctorCmd);

  // Engine command (does not need SpaghettiAPI — reads/writes settings only)
  program
    .command('engine')
    .description('Show or switch the active ingest engine (ts | rs)')
    .argument('[target]', 'Engine to switch to: `ts` (TypeScript) or `rs` (Rust native)')
    .option('--json', 'Output as JSON')
    .action(async (target: string | undefined, opts: { json?: boolean }) => {
      await engineCommand(target, opts);
    });

  // Uninstall command
  program
    .command('uninstall')
    .description('Show uninstall instructions')
    .action(async () => {
      await uninstallCommand();
    });

  program
    .command('update')
    .description('Check for updates and install the latest version')
    .action(async () => {
      await updateCommand();
    });

  return program;
}

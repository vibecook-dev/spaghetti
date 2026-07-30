/**
 * `git worktree list --porcelain` enumeration for the open project.
 *
 * Deliberately NOT routed through the SDK UtilityProcess like the rest of
 * `ipc-handlers.ts`. Everything the SDK serves is derived from Claude Code's
 * session files on disk, and its database is a pure function of those files —
 * a live `git` subprocess answering "what does this repo look like right now"
 * is a different kind of question and does not belong in that data plane.
 * `mille-host.ts` sets the precedent for the main process owning workspace
 * concerns directly.
 *
 * Degradation follows the same rule as mille's git companion: this is
 * decoration, never a lifecycle anchor. A directory that isn't a repository,
 * a missing `git` binary, and malformed output all resolve to an empty list
 * rather than rejecting, because a project pane that fails to open is a far
 * worse outcome than one showing no worktrees.
 */

import { spawn as nodeSpawn } from 'node:child_process';
import { realpathSync } from 'node:fs';

// `WorktreeInfo` is part of the renderer-facing IPC contract, so it is defined
// in `src/shared`. The web tsconfig includes only `src/renderer` + `src/shared`,
// so a renderer importing the type from this module would not compile.
import type { WorktreeInfo } from '../shared/ipc.js';

export type { WorktreeInfo };

export interface ListWorktreesOptions {
  /** Override the git binary. Electron renderers don't always inherit PATH. */
  readonly gitPath?: string;
  /** Inject a fake spawn for tests so nothing shells out. */
  readonly spawn?: typeof nodeSpawn;
  /** Override the warn sink. Tests silence it. */
  readonly warn?: (msg: string) => void;
  /** Give up after this long. Default 5s. */
  readonly timeoutMs?: number;
}

/**
 * Parse the porcelain stream into records.
 *
 * Format is blank-line-separated stanzas, each opening with `worktree <path>`
 * and followed by either `label value` or bare-label attribute lines:
 *
 *     worktree /repo
 *     HEAD deadbeef...
 *     branch refs/heads/main
 *
 *     worktree /repo/../wt
 *     HEAD cafe...
 *     detached
 *     prunable gitdir file points to non-existent location
 *
 * Known limitation: the non-`-z` format is line-oriented, so a worktree path
 * containing a literal newline would split into garbage. `-z` fixes that but
 * needs git >= 2.36; a newline in a checkout path is pathological enough that
 * the version floor is the worse trade. Such an entry is dropped by the
 * `worktree ` prefix check rather than corrupting its neighbours.
 */
export function parseWorktreePorcelain(stdout: string): WorktreeInfo[] {
  const out: WorktreeInfo[] = [];
  let current: WorktreeInfo | null = null;

  const push = (): void => {
    if (current !== null) out.push(current);
    current = null;
  };

  for (const rawLine of stdout.split('\n')) {
    const line = rawLine.replace(/\r$/, '');
    if (line === '') {
      push();
      continue;
    }

    if (line.startsWith('worktree ')) {
      // A `worktree` line without an intervening blank ends the previous
      // stanza too — don't rely on the trailing newline being well-formed.
      push();
      current = {
        path: line.slice('worktree '.length),
        realPath: null,
        head: null,
        branch: null,
        branchRef: null,
        isMain: out.length === 0,
        detached: false,
        bare: false,
        locked: false,
        lockReason: null,
        prunable: false,
        prunableReason: null,
      };
      continue;
    }

    if (current === null) continue; // attribute before any `worktree` line

    const space = line.indexOf(' ');
    const label = space === -1 ? line : line.slice(0, space);
    const value = space === -1 ? '' : line.slice(space + 1);

    switch (label) {
      case 'HEAD':
        current.head = value;
        break;
      case 'branch':
        current.branchRef = value;
        current.branch = value.startsWith('refs/heads/') ? value.slice('refs/heads/'.length) : value;
        break;
      case 'detached':
        current.detached = true;
        break;
      case 'bare':
        current.bare = true;
        break;
      case 'locked':
        current.locked = true;
        current.lockReason = value === '' ? null : value;
        break;
      case 'prunable':
        current.prunable = true;
        current.prunableReason = value === '' ? null : value;
        break;
      default:
        // Unknown attribute — git may add more. Ignore rather than fail;
        // an unrecognized key must not cost us the whole stanza.
        break;
    }
  }

  push();
  return out;
}

/**
 * Fill in `realPath` for each entry.
 *
 * Kept out of `parseWorktreePorcelain` so that stays a pure string->data
 * function testable without a filesystem. A path that cannot be resolved keeps
 * `realPath: null` — normal for a prunable worktree, whose directory is gone.
 */
function resolveRealPaths(worktrees: WorktreeInfo[]): WorktreeInfo[] {
  return worktrees.map((w) => {
    try {
      return { ...w, realPath: realpathSync.native(w.path) };
    } catch {
      return w;
    }
  });
}

/**
 * Enumerate the worktrees of the repository containing `projectPath`.
 *
 * Resolves to `[]` for a non-repository, a missing git binary, a timeout, or
 * unparseable output. Never rejects.
 */
export async function listWorktrees(projectPath: string, options: ListWorktreesOptions = {}): Promise<WorktreeInfo[]> {
  const {
    gitPath = 'git',
    spawn = nodeSpawn,
    warn = (msg: string): void => {
      console.warn(msg);
    },
    timeoutMs = 5000,
  } = options;

  return new Promise<WorktreeInfo[]>((resolve) => {
    let settled = false;
    const finish = (result: WorktreeInfo[]): void => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      resolve(result);
    };

    let child: ReturnType<typeof nodeSpawn>;
    try {
      child = spawn(gitPath, ['worktree', 'list', '--porcelain'], { cwd: projectPath });
    } catch (err) {
      warn(`[worktrees] could not spawn ${gitPath}: ${String(err)}`);
      return resolve([]);
    }

    const timer = setTimeout(() => {
      try {
        child.kill();
      } catch {
        /* already gone */
      }
      warn(`[worktrees] \`git worktree list\` timed out after ${timeoutMs}ms in ${projectPath}`);
      finish([]);
    }, timeoutMs);

    let stdout = '';
    child.stdout?.setEncoding('utf8');
    child.stdout?.on('data', (chunk: string) => {
      stdout += chunk;
    });
    // Drain stderr so a chatty git can't fill the pipe buffer and wedge.
    child.stderr?.resume();

    child.on('error', (err) => {
      // ENOENT here means no git on PATH. One warn, empty list, move on.
      warn(`[worktrees] ${gitPath} failed in ${projectPath}: ${err.message}`);
      finish([]);
    });

    child.on('close', (code) => {
      if (code !== 0) {
        // 128 is git's "not a repository", by far the common case — a project
        // simply isn't version controlled. Not worth a warning.
        finish([]);
        return;
      }
      try {
        finish(resolveRealPaths(parseWorktreePorcelain(stdout)));
      } catch (err) {
        warn(`[worktrees] could not parse porcelain output: ${String(err)}`);
        finish([]);
      }
    });
  });
}

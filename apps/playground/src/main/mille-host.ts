/**
 * Spawns / respawns the mille UtilityProcess and transfers a MessagePort
 * into the active BrowserWindow for the renderer FileTree.
 */

import { BrowserWindow, MessageChannelMain, utilityProcess, type UtilityProcess } from 'electron';
import { existsSync, statSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const __dirname = dirname(fileURLToPath(import.meta.url));

let fxProcess: UtilityProcess | null = null;
let activeRoot: string | null = null;

function resolveFxHostPath(): string {
  // electron-vite emits alongside main entry (index.js / index.mjs).
  const candidates = [join(__dirname, 'fx-host.mjs'), join(__dirname, 'fx-host.js')];
  for (const p of candidates) {
    if (existsSync(p)) return p;
  }
  return candidates[0]!;
}

function killFxProcess(): void {
  if (fxProcess === null) return;
  try {
    fxProcess.kill();
  } catch {
    /* already dead */
  }
  fxProcess = null;
  activeRoot = null;
}

function forkFxProcess(root: string): UtilityProcess {
  const script = resolveFxHostPath();
  const proc = utilityProcess.fork(script, [], {
    serviceName: 'mille-file-explorer',
    stdio: 'pipe',
    env: {
      ...process.env,
      WORKSPACE_ROOT: root,
    },
  });
  proc.stdout?.on('data', (d) => process.stdout.write(`[fx-host] ${d}`));
  proc.stderr?.on('data', (d) => process.stderr.write(`[fx-host] ${d}`));
  proc.on('exit', (code) => {
    if (code !== 0 && code !== null) {
      console.error(`[mille] fx utility exited with code ${code}`);
    }
    if (fxProcess === proc) {
      fxProcess = null;
      activeRoot = null;
    }
  });
  return proc;
}

/**
 * Start (or restart) the fx utility against `root` and transfer a
 * MessagePort to the renderer. No-ops if the same root is already live.
 */
export function openMilleWorkspace(win: BrowserWindow, root: string): void {
  if (!win || win.isDestroyed()) return;

  try {
    const st = statSync(root);
    if (!st.isDirectory()) {
      throw new Error(`not a directory: ${root}`);
    }
  } catch (e) {
    throw new Error(`Cannot open folder "${root}": ${e instanceof Error ? e.message : String(e)}`, { cause: e });
  }

  if (fxProcess !== null && activeRoot === root) {
    // Already browsing this root — re-attach a port in case the renderer remounted.
    const proc = fxProcess;
    const { port1, port2 } = new MessageChannelMain();
    proc.postMessage({ type: 'attach' }, [port1]);
    win.webContents.postMessage('fx-port', { workspaceRoot: root }, [port2]);
    return;
  }

  killFxProcess();

  const proc = forkFxProcess(root);
  fxProcess = proc;
  activeRoot = root;
  console.log(`[mille] forked fx-host for ${root}`);

  const onMessage = (msg: unknown): void => {
    const m = msg as { type?: string } | undefined;
    if (m?.type !== 'ready') return;
    proc.off('message', onMessage);
    if (proc !== fxProcess) return; // superseded
    const { port1, port2 } = new MessageChannelMain();
    proc.postMessage({ type: 'attach' }, [port1]);
    if (!win.isDestroyed()) {
      win.webContents.postMessage('fx-port', { workspaceRoot: root }, [port2]);
    }
    console.log(`[mille] attach+port transferred for ${root}`);
  };
  proc.on('message', onMessage);
}

/** Tear down the utility process (panel closed / app quit). */
export function closeMilleWorkspace(): void {
  killFxProcess();
}

export function getMilleActiveRoot(): string | null {
  return activeRoot;
}

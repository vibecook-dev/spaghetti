/**
 * Electron main entrypoint.
 *
 * - Single-instance lock so two playgrounds cannot share the same cache.
 * - Creates a BrowserWindow with context isolation + preload.
 * - Starts one Rust observation owner in a UtilityProcess.
 * - Mille file explorer UtilityProcess (right panel) via MessagePort.
 * - Awaitable dispose on quit (prefer over kill -9 mid-ingest).
 */

import { app, BrowserWindow, ipcMain, shell } from 'electron';
import { execFile } from 'node:child_process';
import { existsSync } from 'node:fs';
import { realpath } from 'node:fs/promises';
import { basename, dirname, extname, isAbsolute, join, relative } from 'node:path';
import { fileURLToPath } from 'node:url';
import type { SdkHostEvent } from '../shared/sdk-protocol.js';
import { EVENT_CHANNELS } from '../shared/ipc.js';
import { registerIpcHandlers } from './ipc-handlers.js';
import { SdkHostClient } from './sdk-host-client.js';
import { closeMilleWorkspace, getMilleActiveRoot, openMilleWorkspace } from './mille-host.js';

const __dirname = dirname(fileURLToPath(import.meta.url));
let sdkHostClient: SdkHostClient | null = null;

function broadcast(channel: string, payload: unknown): void {
  for (const win of BrowserWindow.getAllWindows()) {
    if (!win.isDestroyed()) win.webContents.send(channel, payload);
  }
}

function broadcastSdkEvent(event: SdkHostEvent): void {
  switch (event.event) {
    case 'progress':
      broadcast(EVENT_CHANNELS.progress, event.payload);
      return;
    case 'ready':
      broadcast(EVENT_CHANNELS.ready, event.payload);
      return;
    case 'change':
      broadcast(EVENT_CHANNELS.change, event.payload);
      return;
    case 'active-session-change':
      broadcast(EVENT_CHANNELS.activeSessionChange, event.payload);
      return;
    case 'init-error':
      broadcast(EVENT_CHANNELS.initError, event.payload);
  }
}

// ── Single instance ─────────────────────────────────────────────────────────
// Prevent two playground processes from exclusive-ingesting the same cache
// (journal_mode flip races / SQLITE_CORRUPT risk).
const gotTheLock = app.requestSingleInstanceLock();
if (!gotTheLock) {
  app.quit();
} else {
  app.on('second-instance', () => {
    const win = BrowserWindow.getAllWindows()[0];
    if (win) {
      if (win.isMinimized()) win.restore();
      win.focus();
    }
  });
}

/**
 * Resolve the SQLite index path inside Electron's per-app `userData` folder.
 *
 * The stable `-rs` name also keeps existing RFC 011 databases reusable.
 */
function resolvePlaygroundDbPath(): string {
  return join(app.getPath('userData'), 'cache', 'spaghetti-rs.db');
}

/**
 * electron-vite emits ESM preloads as `index.mjs` (and historically as
 * `index.js`). Pick whichever exists so contextBridge always loads.
 */
function resolvePreloadPath(): string {
  const mjs = join(__dirname, '../preload/index.mjs');
  const js = join(__dirname, '../preload/index.js');
  if (existsSync(mjs)) return mjs;
  if (existsSync(js)) return js;
  return mjs;
}

function createWindow(): BrowserWindow {
  const isMac = process.platform === 'darwin';
  const win = new BrowserWindow({
    width: 1400,
    height: 900,
    minWidth: 900,
    minHeight: 600,
    // Match the light paper frame before React paints.
    backgroundColor: '#e9e5da',
    // Show immediately so the loading screen is visible during cold ingest.
    show: true,
    title: 'Spaghetti Playground',
    autoHideMenuBar: true,
    // Custom chrome: no native title bar. We draw macOS-style traffic lights
    // in the renderer (top-left) so they always match the archive UI.
    ...(isMac
      ? {
          titleBarStyle: 'hidden' as const,
          roundedCorners: false,
        }
      : {}),
    webPreferences: {
      preload: resolvePreloadPath(),
      nodeIntegration: false,
      contextIsolation: true,
      sandbox: false,
    },
  });

  // Prefer our in-app lights — hide native buttons when available.
  if (isMac && typeof win.setWindowButtonVisibility === 'function') {
    win.setWindowButtonVisibility(false);
  }

  win.once('ready-to-show', () => {
    if (!win.isVisible()) win.show();
    win.focus();
    if (isMac && typeof win.setWindowButtonVisibility === 'function') {
      win.setWindowButtonVisibility(false);
    }
  });

  win.webContents.setWindowOpenHandler(({ url }) => {
    void shell.openExternal(url);
    return { action: 'deny' };
  });

  const devUrl = process.env['ELECTRON_RENDERER_URL'];
  if (devUrl) {
    void win.loadURL(devUrl);
    win.webContents.openDevTools({ mode: 'detach' });
  } else {
    void win.loadFile(join(__dirname, '../renderer/index.html'));
  }

  return win;
}

void app.whenReady().then(async () => {
  if (!gotTheLock) return;

  const dbPath = resolvePlaygroundDbPath();
  const sdkHost = new SdkHostClient({ dbPath, onEvent: broadcastSdkEvent });
  sdkHostClient = sdkHost;
  registerIpcHandlers(sdkHost);

  // Mille file explorer — open/close workspace from the Files panel.
  // open-workspace forks UtilityProcess + transfers MessagePort to renderer.
  ipcMain.handle('mille:open-workspace', (evt, raw: unknown) => {
    if (typeof raw !== 'string' || raw.length === 0) {
      throw new Error('mille:open-workspace: path must be a non-empty string');
    }
    const win = BrowserWindow.fromWebContents(evt.sender);
    if (!win) throw new Error('mille:open-workspace: no window');
    openMilleWorkspace(win, raw);
    return { ok: true as const, root: raw };
  });
  ipcMain.handle('mille:close-workspace', () => {
    closeMilleWorkspace();
    return { ok: true as const };
  });

  ipcMain.handle('file-viewer:open-html', async (_evt, rawPath: unknown, rawBrowser: unknown) => {
    if (typeof rawPath !== 'string' || rawPath.length === 0) {
      throw new Error('file-viewer: path must be a non-empty string');
    }
    if (!isBrowserTarget(rawBrowser)) {
      throw new Error('file-viewer: unsupported browser target');
    }

    const activeRoot = getMilleActiveRoot();
    if (!activeRoot) throw new Error('The project workspace is no longer open.');

    const [rootPath, targetPath] = await Promise.all([
      realpath(activeRoot),
      resolveViewerTargetPath(activeRoot, rawPath),
    ]);
    const projectRelativePath = relative(rootPath, targetPath);
    if (
      projectRelativePath === '' ||
      projectRelativePath === '..' ||
      projectRelativePath.startsWith(`..${process.platform === 'win32' ? '\\' : '/'}`) ||
      isAbsolute(projectRelativePath)
    ) {
      throw new Error('The file must be inside the active project.');
    }
    if (!['.html', '.htm'].includes(extname(targetPath).toLowerCase())) {
      throw new Error('Only HTML files can be opened from the file viewer.');
    }

    if (rawBrowser === 'default') {
      if (process.platform === 'darwin') {
        await executeFile('/usr/bin/open', [targetPath]);
      } else {
        const message = await shell.openPath(targetPath);
        if (message) throw new Error(message);
      }
      return { ok: true as const };
    }
    if (process.platform !== 'darwin') {
      throw new Error('Named browser selection is currently available on macOS only.');
    }

    const appName = {
      safari: 'Safari',
      chrome: 'Google Chrome',
      firefox: 'Firefox',
    }[rawBrowser];
    await executeFile('/usr/bin/open', ['-a', appName, targetPath]);
    return { ok: true as const };
  });

  // Window chrome controls (fallback when traffic lights are missing / hard to hit).
  ipcMain.handle('window:minimize', (evt) => {
    BrowserWindow.fromWebContents(evt.sender)?.minimize();
  });
  ipcMain.handle('window:toggle-full-screen', (evt) => {
    const win = BrowserWindow.fromWebContents(evt.sender);
    if (!win) return;
    win.setFullScreen(!win.isFullScreen());
  });
  ipcMain.handle('window:close', (evt) => {
    BrowserWindow.fromWebContents(evt.sender)?.close();
  });

  createWindow();

  void sdkHost.start().catch((err: unknown) => {
    console.error('[main] SDK utility failed to start', err);
    broadcast(EVENT_CHANNELS.initError, String(err));
  });

  app.on('activate', () => {
    if (BrowserWindow.getAllWindows().length === 0) {
      createWindow();
    }
  });
});

app.on('window-all-closed', () => {
  if (process.platform !== 'darwin') {
    app.quit();
  }
});

// Graceful teardown: await live pipeline drain + SQLite close.
// Prefer this over kill -9 during cold ingest (native bulk can leave a
// half-written cache if terminated mid-write).
let isQuitting = false;
app.on('before-quit', (event) => {
  if (isQuitting) return;
  event.preventDefault();
  isQuitting = true;
  closeMilleWorkspace();
  void (sdkHostClient?.dispose() ?? Promise.resolve())
    .catch((err) => {
      console.error('[main] SDK utility dispose failed', err);
      sdkHostClient?.kill();
    })
    .finally(() => {
      app.exit(0);
    });
});

type BrowserTarget = 'default' | 'safari' | 'chrome' | 'firefox';

function isBrowserTarget(value: unknown): value is BrowserTarget {
  return value === 'default' || value === 'safari' || value === 'chrome' || value === 'firefox';
}

function executeFile(file: string, args: string[]): Promise<void> {
  return new Promise((resolve, reject) => {
    execFile(file, args, (error) => {
      if (error) reject(error);
      else resolve();
    });
  });
}

async function resolveViewerTargetPath(activeRoot: string, rawPath: string): Promise<string> {
  try {
    return await realpath(rawPath);
  } catch (initialError: unknown) {
    // Compatibility guard for previews opened before the renderer path fix:
    // `/project/project/file` becomes `/project/file`, but only after the
    // original path fails and only for the active root's repeated basename.
    const repeatedRoot = join(activeRoot, basename(activeRoot));
    const repeatedRelativePath = relative(repeatedRoot, rawPath);
    if (
      repeatedRelativePath === '' ||
      repeatedRelativePath === '..' ||
      repeatedRelativePath.startsWith(`..${process.platform === 'win32' ? '\\' : '/'}`) ||
      isAbsolute(repeatedRelativePath)
    ) {
      throw initialError;
    }
    return realpath(join(activeRoot, repeatedRelativePath));
  }
}

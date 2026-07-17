/**
 * Electron main entrypoint.
 *
 * - Single-instance lock so two playgrounds cannot share the same cache.
 * - Creates a BrowserWindow with context isolation + preload.
 * - Initializes SpaghettiService (multi-source ingest into userData SQLite).
 * - Awaitable dispose on quit (prefer over kill -9 mid-ingest).
 */

import { app, BrowserWindow, shell } from 'electron';
import { existsSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';
import type { IngestEngine } from '@vibecook/spaghetti-sdk';
import { registerIpcHandlers, wireEventForwarding } from './ipc-handlers.js';
import { resolveAppEngine } from './settings.js';
import { disposeSdk, shutdownSdk } from './sdk.js';

const __dirname = dirname(fileURLToPath(import.meta.url));

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
 * Filename includes the active ingest engine (rs|ts). Engine is read from the
 * app's own settings file — not `~/.spaghetti/config.json`.
 */
function resolvePlaygroundDbPath(engine: IngestEngine): string {
  return join(app.getPath('userData'), 'cache', `spaghetti-${engine}.db`);
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
  const win = new BrowserWindow({
    width: 1400,
    height: 900,
    minWidth: 900,
    minHeight: 600,
    backgroundColor: '#050505',
    // Show immediately so the loading screen is visible during cold ingest.
    show: true,
    title: 'Spaghetti Playground',
    autoHideMenuBar: true,
    webPreferences: {
      preload: resolvePreloadPath(),
      nodeIntegration: false,
      contextIsolation: true,
      sandbox: false,
    },
  });

  win.once('ready-to-show', () => {
    if (!win.isVisible()) win.show();
    win.focus();
  });

  win.webContents.setWindowOpenHandler(({ url }) => {
    void shell.openExternal(url);
    return { action: 'deny' };
  });

  const devUrl = process.env['ELECTRON_RENDERER_URL'];
  if (devUrl) {
    void win.loadURL(devUrl);
  } else {
    void win.loadFile(join(__dirname, '../renderer/index.html'));
  }

  return win;
}

void app.whenReady().then(async () => {
  if (!gotTheLock) return;

  const engine = resolveAppEngine();
  const dbPath = resolvePlaygroundDbPath(engine);
  registerIpcHandlers();

  createWindow();

  void wireEventForwarding({ dbPath, engine }).catch((err) => {
    console.error('[main] SDK initialization failed', err);
    for (const win of BrowserWindow.getAllWindows()) {
      if (!win.isDestroyed()) {
        win.webContents.send('spaghetti:event:init-error', String(err));
      }
    }
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
  void disposeSdk()
    .catch((err) => {
      console.error('[main] dispose failed', err);
      shutdownSdk();
    })
    .finally(() => {
      app.exit(0);
    });
});

/**
 * Ensure a complete Electron binary is available for electron-vite.
 *
 * Incomplete installs (only MacOS/Electron stub, no Frameworks/) happen when
 * `path.txt` is written without a full extract — install.js then thinks
 * Electron is installed and skips the download. This script detects that and
 * re-downloads / re-extracts.
 */
import { createRequire } from 'node:module';
import { existsSync, writeFileSync, rmSync, mkdirSync, readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';

const require = createRequire(import.meta.url);

function resolveElectronDir() {
  try {
    return dirname(require.resolve('electron/package.json'));
  } catch {
    return null;
  }
}

function platformPath() {
  switch (process.platform) {
    case 'darwin':
      return 'Electron.app/Contents/MacOS/Electron';
    case 'win32':
      return 'electron.exe';
    default:
      return 'electron';
  }
}

/** True when the full Electron app layout is present (not just a stub binary). */
function isCompleteInstall(electronDir) {
  const dist = join(electronDir, 'dist');
  const pathTxt = join(electronDir, 'path.txt');
  const versionFile = join(dist, 'version');
  const pkg = JSON.parse(readFileSync(join(electronDir, 'package.json'), 'utf8'));

  if (!existsSync(pathTxt) || !existsSync(versionFile)) return false;

  try {
    const installed = readFileSync(versionFile, 'utf8').replace(/^v/, '').trim();
    if (installed !== pkg.version) return false;
  } catch {
    return false;
  }

  const exe = join(dist, platformPath());
  if (!existsSync(exe)) return false;

  // macOS: Frameworks/Electron Framework is required — a lone MacOS/Electron
  // stub is NOT enough (dyld will fail at launch).
  if (process.platform === 'darwin') {
    const framework = join(
      dist,
      'Electron.app/Contents/Frameworks/Electron Framework.framework/Versions/A/Electron Framework',
    );
    if (!existsSync(framework)) return false;
  }

  return true;
}

function writePathTxt(electronDir) {
  writeFileSync(join(electronDir, 'path.txt'), platformPath());
}

function forceReinstall(electronDir) {
  const dist = join(electronDir, 'dist');
  const pathTxt = join(electronDir, 'path.txt');
  console.log('[ensure-electron] incomplete install — wiping dist and re-downloading…');
  try {
    rmSync(dist, { recursive: true, force: true });
  } catch {
    /* ignore */
  }
  try {
    rmSync(pathTxt, { force: true });
  } catch {
    /* ignore */
  }
  mkdirSync(dist, { recursive: true });

  // Prefer system unzip after @electron/get download (more reliable than
  // extract-zip when dist is partially populated).
  const installJs = join(electronDir, 'install.js');
  const r = spawnSync(process.execPath, [installJs], {
    cwd: electronDir,
    stdio: 'inherit',
    env: {
      ...process.env,
      // Force @electron/get to re-fetch if cache is corrupt
      force_no_cache: 'true',
      ELECTRON_SKIP_BINARY_DOWNLOAD: '',
    },
  });

  if (r.status !== 0 || !isCompleteInstall(electronDir)) {
    // Fallback: manual download + unzip
    console.log('[ensure-electron] install.js incomplete — manual download+unzip…');
    return manualDownloadAndUnzip(electronDir);
  }
  return true;
}

function manualDownloadAndUnzip(electronDir) {
  const { downloadArtifact } = require(
    require.resolve('@electron/get', { paths: [electronDir] }),
  );
  const pkg = JSON.parse(readFileSync(join(electronDir, 'package.json'), 'utf8'));
  const dist = join(electronDir, 'dist');

  // downloadArtifact is async — use child to keep script simple
  const script = `
    const { downloadArtifact } = require('@electron/get');
    const { version } = require(${JSON.stringify(join(electronDir, 'package.json'))});
    downloadArtifact({
      version,
      artifactName: 'electron',
      force: true,
      platform: process.platform,
      arch: process.arch,
    }).then((zip) => {
      console.log('[ensure-electron] downloaded', zip);
      process.stdout.write(zip);
    }).catch((e) => {
      console.error(e);
      process.exit(1);
    });
  `;

  const dl = spawnSync(process.execPath, ['-e', script], {
    cwd: electronDir,
    encoding: 'utf8',
    env: process.env,
    maxBuffer: 10 * 1024 * 1024,
  });
  if (dl.status !== 0) {
    console.error(dl.stderr || dl.stdout);
    return false;
  }
  const lines = (dl.stdout || '').trim().split('\n');
  const zipPath = lines[lines.length - 1]?.trim();
  if (!zipPath || !existsSync(zipPath)) {
    console.error('[ensure-electron] download path missing:', zipPath);
    return false;
  }

  rmSync(dist, { recursive: true, force: true });
  mkdirSync(dist, { recursive: true });

  const unzip = spawnSync('unzip', ['-q', zipPath, '-d', dist], { stdio: 'inherit' });
  if (unzip.status !== 0) {
    console.error('[ensure-electron] unzip failed');
    return false;
  }
  writePathTxt(electronDir);
  return isCompleteInstall(electronDir);
}

const electronDir = resolveElectronDir();
if (!electronDir) {
  console.error('[ensure-electron] electron package not found — run pnpm install from repo root');
  process.exit(1);
}

if (!isCompleteInstall(electronDir)) {
  const ok = forceReinstall(electronDir);
  if (!ok || !isCompleteInstall(electronDir)) {
    console.error('[ensure-electron] failed to install a complete Electron binary');
    process.exit(1);
  }
} else {
  // Ensure path.txt is correct even when install is complete
  writePathTxt(electronDir);
}

try {
  // Clear require cache for electron so path is re-read
  const electronEntry = require.resolve('electron', { paths: [process.cwd()] });
  delete require.cache[electronEntry];
  const p = require('electron');
  if (typeof p !== 'string' || !existsSync(p)) {
    throw new Error(`resolved path not usable: ${p}`);
  }
  // macOS framework check via resolved binary parent
  if (process.platform === 'darwin') {
    const framework = join(
      dirname(p),
      '../Frameworks/Electron Framework.framework/Versions/A/Electron Framework',
    );
    if (!existsSync(framework)) {
      throw new Error(`Electron Framework missing at ${framework}`);
    }
  }
  console.log('[ensure-electron] ok:', p);
} catch (err) {
  console.error('[ensure-electron] require(electron) failed:', err.message);
  process.exit(1);
}

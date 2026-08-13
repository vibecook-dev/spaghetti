#!/usr/bin/env node
/**
 * Run one repository-only TypeScript oracle against an isolated native addon
 * built with the retired bulk/live writer feature.
 *
 * The generated JS, declarations, and `.node` binary live in a temporary
 * directory. Production package artifacts are never overwritten, and the
 * native-library override is scoped to the child oracle process.
 */

import { spawn } from 'node:child_process';
import { mkdtempSync, readdirSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const argv = process.argv.slice(2);
const release = argv[0] === '--release';
if (release) argv.shift();
const target = argv.shift();

if (!target) {
  console.error('usage: run-legacy-oracle.mjs [--release] <script.ts> [...args]');
  process.exitCode = 2;
} else {
  const outputDir = mkdtempSync(path.join(tmpdir(), 'spaghetti-legacy-oracle-'));
  try {
    const pnpmArgs = [
      '--dir',
      path.join(repoRoot, 'crates/spaghetti-napi'),
      'exec',
      'napi',
      'build',
      '--platform',
      '--features',
      'legacy-oracle',
      '--output-dir',
      outputDir,
    ];
    if (release) pnpmArgs.push('--release');

    const npmExecPath = process.env.npm_execpath;
    const build = npmExecPath
      ? await run(process.execPath, [npmExecPath, ...pnpmArgs])
      : await run(process.platform === 'win32' ? 'pnpm.cmd' : 'pnpm', pnpmArgs);
    if (build !== 0) {
      process.exitCode = build;
    } else {
      const bindings = readdirSync(outputDir).filter((entry) => entry.endsWith('.node'));
      if (bindings.length !== 1) {
        throw new Error(`expected one legacy native binding, found ${bindings.length}`);
      }
      const status = await run(
        process.execPath,
        ['--import', 'tsx', path.resolve(repoRoot, target), ...argv],
        {
          ...process.env,
          NAPI_RS_NATIVE_LIBRARY_PATH: path.join(outputDir, bindings[0]),
        },
      );
      process.exitCode = status;
    }
  } finally {
    rmSync(outputDir, { recursive: true, force: true, maxRetries: 10, retryDelay: 50 });
  }
}

/** @returns {Promise<number>} */
function run(command, args, env = process.env) {
  return new Promise((resolve, reject) => {
    const child = spawn(command, args, { cwd: repoRoot, env, stdio: 'inherit' });
    child.once('error', reject);
    child.once('exit', (code, signal) => {
      if (signal) reject(new Error(`${command} terminated by ${signal}`));
      else resolve(code ?? 1);
    });
  });
}

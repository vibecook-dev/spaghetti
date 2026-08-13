#!/usr/bin/env -S node --import tsx
/**
 * Vendor-neutral RFC 011 adapter conformance report.
 *
 * This command intentionally composes named core, driver, capability,
 * lifecycle, SDK-manifest, and differential packs. A broad workspace test is
 * useful too, but is not a substitute for this per-adapter result.
 */

import { spawnSync } from 'node:child_process';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { parseArgs } from 'node:util';

type Adapter = 'claude-code' | 'codex' | 'grok';
type PackStatus = 'pass' | 'fail';

interface PackReport {
  name: string;
  scope: 'common' | Adapter;
  status: PackStatus;
  durationMs: number;
  command: string;
  detail?: string;
}

const adapters: Adapter[] = ['claude-code', 'codex', 'grok'];
const adapterTestModules: Record<Adapter, string> = {
  'claude-code': 'claude::adapter::tests::',
  codex: 'codex::adapter::tests::',
  grok: 'grok::adapter::tests::',
};

const { values } = parseArgs({
  options: {
    adapter: { type: 'string' },
    all: { type: 'boolean' },
    fixture: { type: 'string' },
    'skip-build': { type: 'boolean' },
  },
});

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const selected = selectAdapters(values.adapter, values.all);
if (values.fixture && selected.length !== 1) {
  throw new Error('--fixture requires exactly one --adapter');
}

const reports: PackReport[] = [];

if (!values['skip-build']) {
  reports.push(
    runPack('native-addon-build', 'common', 'pnpm', ['--dir', 'crates/spaghetti-napi', 'build:debug']),
  );
}

for (const [name, filter] of [
  ['source-driver-pack', 'source::'],
  ['transaction-delivery-pack', 'engine::commit::tests::'],
  ['coordinator-core-pack', 'engine::coordinator::tests::'],
  ['watch-recovery-pack', 'engine::supervisor::tests::'],
  ['capability-projection-pack', 'engine::projection::tests::'],
] as const) {
  reports.push(runPack(name, 'common', 'cargo', ['test', '-p', 'spaghetti-napi', filter]));
}

for (const adapter of selected) {
  reports.push(
    runPack('native-decoder-golden-pack', adapter, 'cargo', [
      'test',
      '-p',
      'spaghetti-napi',
      adapterTestModules[adapter],
    ]),
  );
  const differentialArgs = [
    '--import',
    'tsx',
    'scripts/observation-diff.ts',
    '--adapter',
    adapter,
  ];
  if (values.fixture) differentialArgs.push('--fixture', values.fixture);
  reports.push(runPack('cold-live-reconcile-restart-differential', adapter, process.execPath, differentialArgs));
}

reports.push(
  runPack('sdk-manifest-and-multi-adapter-host-pack', 'common', process.execPath, [
    '--import',
    'tsx',
    '--test',
    '--test-force-exit',
    'packages/sdk/src/__tests__/observation-host.test.ts',
  ]),
);

const failed = reports.filter((report) => report.status === 'fail');
const output = {
  contractVersion: 1,
  command: 'adapter-check',
  adapters: selected,
  exact: failed.length === 0,
  passedPacks: reports.length - failed.length,
  failedPacks: failed.length,
  reports,
};
console.log(JSON.stringify(output, null, 2));
if (failed.length > 0) process.exitCode = 1;

function selectAdapters(raw: string | undefined, all: boolean | undefined): Adapter[] {
  if (all || raw === undefined || raw === 'all') return adapters;
  if (adapters.includes(raw as Adapter)) return [raw as Adapter];
  throw new Error('--adapter must be one of: claude-code, codex, grok, all');
}

function runPack(name: string, scope: PackReport['scope'], command: string, args: string[]): PackReport {
  const startedAt = Date.now();
  const result = spawnSync(command, args, {
    cwd: repoRoot,
    encoding: 'utf8',
    env: { ...process.env, NO_COLOR: '1' },
    maxBuffer: 32 * 1024 * 1024,
  });
  const status: PackStatus = result.status === 0 ? 'pass' : 'fail';
  const detail = status === 'fail' ? boundedFailureDetail(result.error, result.stdout, result.stderr) : undefined;
  return {
    name,
    scope,
    status,
    durationMs: Date.now() - startedAt,
    command: [command, ...args].map(shellLabel).join(' '),
    ...(detail ? { detail } : {}),
  };
}

function boundedFailureDetail(error: Error | undefined, stdout: string, stderr: string): string {
  const text = [error?.message, stdout, stderr].filter(Boolean).join('\n').trim();
  return text.length <= 8_192 ? text : text.slice(text.length - 8_192);
}

function shellLabel(value: string): string {
  return /^[A-Za-z0-9_./:@=-]+$/.test(value) ? value : JSON.stringify(value);
}

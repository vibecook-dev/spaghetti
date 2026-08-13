/** Engine command — report the RFC 011 Rust owner and clean old settings. */

import {
  defaultDbPathForEngine,
  readSettings,
  resolveActiveEngine,
  settingsPath,
  writeSettings,
} from '@vibecook/spaghetti-sdk';
import { theme } from '../lib/color.js';

export interface EngineOptions {
  json?: boolean;
}

export async function engineCommand(target: string | undefined, opts: EngineOptions): Promise<void> {
  const active = resolveActiveEngine();
  const settings = readSettings();
  const dbPath = defaultDbPathForEngine('rs');

  if (!target) {
    if (opts.json) {
      process.stdout.write(
        JSON.stringify(
          {
            active: 'rs',
            policy: 'rust-only',
            ignoredLegacyPreference: settings.engine === 'ts' ? 'ts' : null,
            nativeAddonAvailable: active.nativeAvailable,
            nativeVersion: active.nativeVersion,
            dbPath,
            configPath: settingsPath(),
          },
          null,
          2,
        ) + '\n',
      );
      return;
    }

    const lines = [
      '',
      `  ${theme.heading('Observation engine')}`,
      '',
      `    engine:     ${theme.project('rs')} (Rust observation owner)`,
      `    native:     ${
        active.nativeAvailable
          ? theme.project('available') + theme.muted(` (v${active.nativeVersion})`)
          : theme.muted('not installed — startup is blocked')
      }`,
    ];
    if (settings.engine === 'ts') {
      lines.push(`    ${theme.warning('legacy config requested "ts"; RFC 011 ignores it')}`);
    }
    lines.push(
      '',
      `  ${theme.muted('Production DB')}`,
      `    ${dbPath}`,
      '',
      `  ${theme.muted('The TypeScript engine is a repository-only differential oracle.')}`,
      `  ${theme.muted('Config: ' + settingsPath())}`,
      '',
    );
    process.stdout.write(lines.join('\n') + '\n');
    return;
  }

  const lower = target.toLowerCase();
  if (lower !== 'rs') {
    const reason =
      lower === 'ts' ? 'The TypeScript production engine was retired by RFC 011.' : `Unknown engine: "${target}".`;
    process.stderr.write(theme.error(`\n  ${reason} Use "rs".\n\n`));
    process.exitCode = 1;
    return;
  }

  if (!active.nativeAvailable) {
    process.stderr.write(
      theme.error(
        '\n  Native addon (@vibecook/spaghetti-sdk-native) not installed.\n' +
          '  RFC 011 requires this addon; reinstall the package before starting Spaghetti.\n\n',
      ),
    );
    process.exitCode = 1;
    return;
  }

  writeSettings({ ...settings, engine: 'rs' });
  if (opts.json) {
    process.stdout.write(
      JSON.stringify({ active: 'rs', policy: 'rust-only', dbPath, configPath: settingsPath() }, null, 2) + '\n',
    );
    return;
  }
  process.stdout.write(
    '\n  ' + theme.project('Rust observation engine selected') + '\n' + theme.muted(`  DB: ${dbPath}`) + '\n\n',
  );
}

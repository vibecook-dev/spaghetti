/**
 * Doctor command — text health check for spaghetti and its data paths.
 *
 * The data is gathered by `collectDoctorReport()` in lib/doctor-report.ts
 * so the same snapshot can be rendered by the TUI's DoctorView.
 */

import type { ObservationService, SpaghettiReadiness } from '@vibecook/spaghetti-sdk/observation';
import { theme } from '../lib/color.js';
import { readinessField, readinessFields } from '../lib/catalog.js';
import {
  collectDoctorReport,
  formatBytes,
  leftoverLines,
  leftoverManualCommands,
  tildify,
  type DoctorReport,
  type LeftoverKind,
} from '../lib/doctor-report.js';

const OK = theme.success('✓');
const WARN = theme.warning('!');
const BAD = theme.error('✗');
const DOT = theme.muted('·');
const LABEL_WIDTH = 18;
const INDENT = '  ';

function row(icon: string, label: string, value: string): string {
  return `${INDENT}${icon} ${theme.muted(label.padEnd(LABEL_WIDTH))}  ${value}`;
}

function sub(value: string): string {
  return `${INDENT}  ${' '.repeat(LABEL_WIDTH)}  ${theme.muted(value)}`;
}

function heading(title: string): string {
  return `${INDENT}${theme.heading(title)}`;
}

function colorForLeftover(kind: LeftoverKind, detail: string): string {
  switch (kind) {
    case 'clean':
      return theme.success(detail);
    case 'leftover':
    case 'source-mismatch':
      return theme.warning(detail);
    case 'unknown':
      return theme.error(detail);
  }
}

function leftoverIcon(kind: LeftoverKind): string {
  switch (kind) {
    case 'clean':
      return OK;
    case 'leftover':
    case 'source-mismatch':
      return WARN;
    case 'unknown':
      return BAD;
  }
}

function renderReadiness(readiness: SpaghettiReadiness | undefined): string[] {
  const lines = [heading('Readiness')];
  if (!readiness) {
    lines.push(sub('engine unavailable — open spag once to build the index'));
    lines.push('');
    return lines;
  }
  lines.push(sub(`derived from committed rows at commit ${readiness.atCommitSeq}`));
  for (const [name, field] of readinessFields(readiness)) {
    const icon =
      field.state === 'ready' ? OK : field.state === 'degraded' || field.state === 'unavailable' ? BAD : WARN;
    lines.push(`${INDENT}${icon} ${readinessField(name.padEnd(LABEL_WIDTH), field)}`);
  }
  lines.push('');
  return lines;
}

function renderReport(report: DoctorReport, readiness: SpaghettiReadiness | undefined): string {
  const lines: string[] = [];
  lines.push('');
  lines.push(`${INDENT}${theme.heading('Spaghetti Doctor')}  ${theme.muted(`v${report.version}`)}`);
  lines.push('');

  // ─── Environment ────────────────────────────────────────────────────
  const env = report.environment;
  lines.push(heading('Environment'));
  lines.push(row(OK, 'Node', `${env.node} (${env.platform} ${env.arch})`));
  for (const root of env.agentRoots ?? []) {
    lines.push(row(root.exists ? OK : WARN, root.label, tildify(root.path)));
    const binName = root.id === 'claude-code' ? 'claude' : root.id;
    if (root.bin) {
      lines.push(sub(`${binName} CLI: ${root.bin}`));
    } else {
      lines.push(sub(`${binName} CLI: not in PATH`));
    }
  }
  lines.push(row(env.settings.exists ? OK : WARN, 'settings.json', tildify(env.settings.path)));
  lines.push(row(env.pluginsDir.exists ? OK : WARN, 'plugins dir', tildify(env.pluginsDir.path)));
  lines.push('');

  // ─── Index & live (Plane 1 / 2) ─────────────────────────────────────
  const ix = report.indexLive;
  lines.push(heading('Index & live'));
  const engineLabel =
    ix.preferredEngine === ix.effectiveEngine ? ix.effectiveEngine : `${ix.preferredEngine} → ${ix.effectiveEngine}`;
  lines.push(row(OK, 'engine', engineLabel));
  if (ix.nativeAvailable) {
    lines.push(sub(`native ${ix.nativeVersion ?? 'loaded'}`));
  } else {
    lines.push(sub(theme.muted('native addon unavailable — Rust observation cannot start')));
  }
  lines.push(row(ix.dbExists ? OK : WARN, 'index db', tildify(ix.dbPath)));
  if (ix.dbExists && ix.dbSizeBytes != null) {
    lines.push(sub(formatBytes(ix.dbSizeBytes)));
  } else {
    lines.push(sub('not created yet — run spag once to build the index'));
  }
  lines.push(row(OK, 'live (TUI)', ix.liveDefaultLongLived ? theme.success('on by default') : theme.muted('off')));
  lines.push(row(DOT, 'live (CLI)', ix.liveDefaultOneShot ? 'on' : theme.muted('off for one-shots')));
  lines.push(
    row(
      ix.activeSessionsAlive > 0 ? OK : DOT,
      'active sessions',
      `${ix.activeSessionsAlive} live / ${ix.activeSessionsOnDisk} on disk`,
    ),
  );
  lines.push(sub(tildify(ix.activeSessionsDir)));
  lines.push('');

  lines.push(...renderReadiness(readiness));

  // ─── Retired Claude Code plugins (RFC 007) ──────────────────────────
  lines.push(heading('Claude Code plugins'));
  lines.push(sub('read-only — these plugins were removed from spaghetti'));
  for (const line of leftoverLines(report.leftovers)) {
    lines.push(row(leftoverIcon(line.kind), line.label, colorForLeftover(line.kind, line.detail)));
  }
  const manual = leftoverManualCommands(report.leftovers);
  if (manual.length > 0) {
    lines.push(sub('remove with:'));
    for (const command of manual) lines.push(sub(theme.accent(command)));
  }
  lines.push('');

  return lines.join('\n') + '\n';
}

export async function doctorCommand(version: string, api?: ObservationService): Promise<void> {
  const report = collectDoctorReport(version);
  // A missing or unopenable engine is itself diagnostic information, so the
  // rest of the report still prints.
  const readiness = await api?.getReadiness().catch(() => undefined);
  process.stdout.write(renderReport(report, readiness));
}

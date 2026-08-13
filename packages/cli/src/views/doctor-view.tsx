/**
 * DoctorView — TUI health check screen
 *
 * Mirrors `spag doctor` CLI output but rendered with Ink components.
 * Pulls state via `collectDoctorReport()` so it stays structurally in sync
 * with the CLI view.
 *
 * Keys:
 *   r   refresh
 *   Esc pop back to previous view
 */

import React, { useState, useCallback } from 'react';
import { Box, Text, useInput } from 'ink';
import { useViewNav } from './context.js';
import { VERSION } from './shell.js';
import {
  collectDoctorReport,
  formatBytes,
  formatRelative,
  leftoverLines,
  leftoverManualCommands,
  tildify,
  type DoctorReport,
  type LeftoverKind,
} from '../lib/doctor-report.js';

// ─── Status icons ──────────────────────────────────────────────────────

function StatusIcon({ kind }: { kind: LeftoverKind | 'ok' | 'warn' | 'bad' | 'dot' }): React.ReactElement {
  if (kind === 'ok') return <Text color="green">✓</Text>;
  if (kind === 'warn') return <Text color="yellow">!</Text>;
  if (kind === 'bad') return <Text color="red">✗</Text>;
  if (kind === 'dot') return <Text dimColor>·</Text>;
  // LeftoverKind
  if (kind === 'unknown') return <Text color="red">✗</Text>;
  if (kind === 'leftover' || kind === 'source-mismatch') return <Text color="yellow">!</Text>;
  return <Text color="green">✓</Text>;
}

const LABEL_WIDTH = 18;

function Row({
  icon,
  label,
  children,
}: {
  icon: React.ReactElement;
  label: string;
  children: React.ReactNode;
}): React.ReactElement {
  return (
    <Box>
      <Text>{'  '}</Text>
      {icon}
      <Text> </Text>
      <Text dimColor>{label.padEnd(LABEL_WIDTH)}</Text>
      <Text>{'  '}</Text>
      <Box>
        <Text>{children}</Text>
      </Box>
    </Box>
  );
}

function Sub({ children }: { children: React.ReactNode }): React.ReactElement {
  // Indent: 2 (base) + 2 (icon+space) + LABEL_WIDTH + 2 (separator)
  return (
    <Box>
      <Text>{' '.repeat(2 + 2 + LABEL_WIDTH + 2)}</Text>
      <Text dimColor>{children}</Text>
    </Box>
  );
}

function SectionHeading({ title }: { title: string }): React.ReactElement {
  return (
    <Box>
      <Text>{'  '}</Text>
      <Text bold>{title}</Text>
    </Box>
  );
}

function Spacer(): React.ReactElement {
  return <Text> </Text>;
}

// ─── Retired-plugin leftover rendering (RFC 007) ───────────────────────

function leftoverColor(kind: LeftoverKind): string {
  if (kind === 'clean') return 'green';
  if (kind === 'unknown') return 'red';
  return 'yellow';
}

/**
 * Read-only leftover section. There is deliberately no install or enable call
 * to action — the plugins are retiring, so the only direction offered is
 * removal, and only for state proven to belong to Spaghetti.
 */
function LeftoverSection({ report }: { report: DoctorReport }): React.ReactElement {
  const lines = leftoverLines(report.leftovers);
  const manual = leftoverManualCommands(report.leftovers);

  return (
    <>
      <SectionHeading title="Claude Code plugins" />
      <Sub>read-only — these plugins were removed from spaghetti</Sub>
      {lines.map((line, i) => (
        <Row key={`${line.label}-${i}`} icon={<StatusIcon kind={line.kind} />} label={line.label}>
          <Text color={leftoverColor(line.kind)}>{line.detail}</Text>
        </Row>
      ))}
      {manual.length > 0 && <Sub>remove with:</Sub>}
      {manual.map((command) => (
        <Sub key={command}>
          <Text color="cyan">{command}</Text>
        </Sub>
      ))}
    </>
  );
}

// ─── DoctorView ────────────────────────────────────────────────────────

export function DoctorView(): React.ReactElement {
  const nav = useViewNav();
  const [report, setReport] = useState<DoctorReport>(() => collectDoctorReport(VERSION));
  const [lastRefreshed, setLastRefreshed] = useState(() => Date.now());

  const refresh = useCallback(() => {
    setReport(collectDoctorReport(VERSION));
    setLastRefreshed(Date.now());
  }, []);

  useInput(
    (input, key) => {
      if (key.escape) {
        nav.pop();
        return;
      }
      if (input === 'r' || input === 'R') {
        refresh();
      }
    },
    { isActive: !nav.searchMode },
  );

  const env = report.environment;
  const ix = report.indexLive;
  const engineLabel =
    ix.preferredEngine === ix.effectiveEngine ? ix.effectiveEngine : `${ix.preferredEngine} → ${ix.effectiveEngine}`;

  return (
    <Box flexDirection="column">
      <Spacer />
      <Box>
        <Text>{'  '}</Text>
        <Text bold>Spaghetti Doctor</Text>
        <Text>{'  '}</Text>
        <Text dimColor>v{report.version}</Text>
        <Text>{'  '}</Text>
        <Text dimColor>· refreshed {formatRelative(lastRefreshed)}</Text>
      </Box>
      <Spacer />

      {/* Environment */}
      <SectionHeading title="Environment" />
      <Row icon={<StatusIcon kind="ok" />} label="Node">
        {env.node} ({env.platform} {env.arch})
      </Row>
      {(env.agentRoots ?? []).map((root) => (
        <Box key={root.id} flexDirection="column">
          <Row icon={<StatusIcon kind={root.exists ? 'ok' : 'warn'} />} label={root.label}>
            {tildify(root.path)}
          </Row>
          {root.bin ? (
            <Sub>
              {root.id === 'claude-code' ? 'claude' : root.id} CLI: {root.bin}
            </Sub>
          ) : (
            <Sub>{root.id === 'claude-code' ? 'claude' : root.id} CLI: not in PATH</Sub>
          )}
        </Box>
      ))}
      <Row icon={<StatusIcon kind={env.settings.exists ? 'ok' : 'warn'} />} label="settings.json">
        {tildify(env.settings.path)}
      </Row>
      <Row icon={<StatusIcon kind={env.pluginsDir.exists ? 'ok' : 'warn'} />} label="plugins dir">
        {tildify(env.pluginsDir.path)}
      </Row>
      <Spacer />

      {/* Index & live */}
      <SectionHeading title="Index & live" />
      <Row icon={<StatusIcon kind="ok" />} label="engine">
        {engineLabel}
      </Row>
      <Sub>
        {ix.nativeAvailable
          ? `native ${ix.nativeVersion ?? 'loaded'}`
          : 'native addon unavailable — Rust startup blocked'}
      </Sub>
      <Row icon={<StatusIcon kind={ix.dbExists ? 'ok' : 'warn'} />} label="index db">
        {tildify(ix.dbPath)}
      </Row>
      <Sub>
        {ix.dbExists && ix.dbSizeBytes != null
          ? formatBytes(ix.dbSizeBytes)
          : 'not created yet — run spag once to build the index'}
      </Sub>
      <Row icon={<StatusIcon kind="ok" />} label="live (TUI)">
        <Text color="green">{ix.liveDefaultLongLived ? 'on by default' : 'off'}</Text>
      </Row>
      <Row icon={<StatusIcon kind="dot" />} label="live (CLI)">
        <Text dimColor>{ix.liveDefaultOneShot ? 'on' : 'off for one-shots'}</Text>
      </Row>
      <Row icon={<StatusIcon kind={ix.activeSessionsAlive > 0 ? 'ok' : 'dot'} />} label="active sessions">
        {ix.activeSessionsAlive} live / {ix.activeSessionsOnDisk} on disk
      </Row>
      <Sub>{tildify(ix.activeSessionsDir)}</Sub>
      <Spacer />

      {/* Retired Claude Code plugins (RFC 007) */}
      <LeftoverSection report={report} />
      <Spacer />
    </Box>
  );
}

/**
 * Uninstall command — prints instructions. It never mutates anything itself,
 * including Claude plugin state.
 *
 * RFC 007 changed two things about this output. First, plugin cleanup comes
 * *before* the npm removal, because uninstalling the CLI does not stop plugins
 * that Claude Code has already installed — they keep running with no way left
 * to remove them through Spaghetti. Second, the old blanket
 * `rm -rf ~/.spaghetti` suggestion is gone: it was presented as cache cleanup
 * but would also have deleted hook and channel history. Cache paths are now
 * named individually, and a full purge is a separate, explicit warning.
 */

import pc from 'picocolors';
import { defaultClaudeHome, probePluginLeftovers, type PluginLeftoverReport } from '../lib/plugin-leftovers.js';
import { leftoverManualCommands } from '../lib/doctor-report.js';

export interface UninstallOptions {
  /** Test seam — defaults to the real Claude home. */
  claudeHome?: string;
}

export async function uninstallCommand(options: UninstallOptions = {}): Promise<void> {
  const report = probePluginLeftovers({ claudeHome: options.claudeHome ?? defaultClaudeHome() });
  process.stdout.write(renderUninstall(report) + '\n');
}

export function renderUninstall(report: PluginLeftoverReport): string {
  const lines: string[] = ['', `  ${pc.bold('Uninstall Spaghetti')}`, ''];

  let step = 1;
  const next = (): string => `  ${pc.dim(`${step++}.`)}`;

  // ─── Step 1 (conditional): Claude plugin cleanup, before npm removal ──
  const ownedState = hasOwnedUserLeftovers(report);
  if (ownedState) {
    lines.push(`${next()} Remove the Claude Code plugins ${pc.dim('(do this first)')}:`);
    for (const command of leftoverManualCommands(report)) {
      lines.push(`     ${pc.cyan(command)}`);
    }
    lines.push(
      '',
      `     ${pc.dim('Uninstalling the CLI does not stop plugins Claude Code has already')}`,
      `     ${pc.dim('installed. Plugin data directories are preserved (--keep-data).')}`,
      '',
    );
  }

  // Diagnostic only — never a destructive suggestion for state we cannot claim.
  const diagnostics = diagnosticNotes(report);
  if (diagnostics.length > 0) {
    lines.push(`     ${pc.yellow('Needs manual review:')}`);
    for (const note of diagnostics) lines.push(`     ${pc.dim(`• ${note}`)}`);
    lines.push(`     ${pc.dim('Run')} ${pc.cyan('spag doctor')} ${pc.dim('for the full state.')}`, '');
  }

  if (!ownedState && diagnostics.length === 0) {
    lines.push(`  ${pc.dim('·')} ${pc.dim('No Claude Code plugin leftovers detected.')}`, '');
  }

  // ─── Step 2: the CLI itself ──────────────────────────────────────────
  lines.push(`${next()} Remove the CLI:`, `     ${pc.cyan('npm uninstall -g @vibecook/spaghetti')}`, '');

  // ─── Step 3: cache only, named explicitly ────────────────────────────
  lines.push(
    `${next()} Remove cached index data ${pc.dim('(optional)')}:`,
    `     ${pc.cyan('rm -rf ~/.spaghetti/cache')}`,
    `     ${pc.cyan('rm -f  ~/.spaghetti/update-check.json')}`,
    '',
    `  ${pc.dim('That removes only the rebuildable index and the update stamp.')}`,
    `  ${pc.dim('Your Claude Code data (~/.claude) is NOT affected.')}`,
    '',
  );

  // ─── Separate, explicit purge warning ────────────────────────────────
  lines.push(
    `  ${pc.yellow('Full data purge')} ${pc.dim('— separate from the steps above:')}`,
    `     ${pc.cyan('rm -rf ~/.spaghetti')}`,
    `  ${pc.dim('This also deletes hook-event history and channel message history,')}`,
    `  ${pc.dim('which are not rebuildable. Only run it if you want that data gone.')}`,
    '',
  );

  return lines.join('\n');
}

/** True when user-scope state we can prove is ours is present. */
function hasOwnedUserLeftovers(report: PluginLeftoverReport): boolean {
  for (const plugin of report.plugins) {
    if (plugin.userInstall.status === 'present') return true;
    if (plugin.userEnabled.status === 'present' && plugin.userEnabled.value.enabled) return true;
  }
  return report.userMarketplace.status === 'present' && report.userMarketplace.value.ownership === 'owned';
}

/** Non-actionable findings: reported, never turned into a destructive command. */
function diagnosticNotes(report: PluginLeftoverReport): string[] {
  const notes: string[] = [];

  for (const plugin of report.plugins) {
    if (plugin.nonUserInstalls.status === 'present') {
      for (const record of plugin.nonUserInstalls.value) {
        notes.push(`${plugin.id} is installed in ${record.scope} scope — remove it from that project yourself`);
      }
    }
  }

  if (report.userMarketplace.status === 'present' && report.userMarketplace.value.ownership === 'source-mismatch') {
    notes.push(
      `a marketplace named "spaghetti" points at ${report.userMarketplace.value.sourceDescription}, which is not this repository`,
    );
  }

  for (const unknown of report.unknowns) {
    notes.push(`could not read ${unknown.input}: ${unknown.reason}`);
  }

  return notes;
}

import * as Dialog from '@radix-ui/react-dialog';
import { Database, RotateCcw, X } from 'lucide-react';
import type { StoreStats } from '@vibecook/spaghetti-sdk';
import type { SpaghettiClientResponseMap } from '@vibecook/spaghetti-sdk/client';
import type { ObservationOwnerStatus } from '@shared/ipc';
import { formatBytes, formatNumber } from '../lib/format.js';

export interface SettingsDialogProps {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  onRebuild: () => void;
  rebuilding: boolean;
  engine: 'rs' | null;
  stats: StoreStats | null;
  observationStatus: ObservationOwnerStatus | null;
  canonicalStats: SpaghettiClientResponseMap['getStats'] | null;
  canonicalStatsLoading: boolean;
  canonicalStatsError: string | null;
}

/** Archive-styled application settings and index maintenance popup. */
export function SettingsDialog({
  open,
  onOpenChange,
  onRebuild,
  rebuilding,
  engine,
  stats,
  observationStatus,
  canonicalStats,
  canonicalStatsLoading,
  canonicalStatsError,
}: SettingsDialogProps) {
  const canonicalState = observationStatus?.enabled
    ? observationStatus.state
    : observationStatus
      ? 'disabled'
      : canonicalStatsLoading
        ? 'loading'
        : 'unknown';
  const canonicalError = canonicalStatsError ?? observationStatus?.error;
  return (
    <Dialog.Root open={open} onOpenChange={onOpenChange}>
      <Dialog.Portal>
        <Dialog.Overlay className="fixed inset-0 z-50 bg-[#11100f]/72 backdrop-blur-[3px]" />
        <Dialog.Content className="fixed left-1/2 top-1/2 z-50 flex max-h-[calc(100vh-2rem)] w-[min(32rem,calc(100vw-2rem))] -translate-x-1/2 -translate-y-1/2 flex-col overflow-hidden border border-[#2b2623]/60 bg-[#f8f6f0] text-[#2b2623] shadow-2xl outline-none dark:border-[#d4cbbd]/45 dark:bg-[#171615] dark:text-[#d4cbbd]">
          <header className="flex shrink-0 items-center justify-between border-b border-[#2b2623]/20 px-5 py-3 dark:border-[#d4cbbd]/20">
            <span className="font-mono text-[9px] tracking-[0.18em] opacity-55">SPAGHETTI ARCHIVE · SETTINGS</span>
            <Dialog.Close asChild>
              <button
                type="button"
                className="flex cursor-pointer items-center gap-2 border-0 bg-transparent font-mono text-[9px] tracking-[0.14em] text-inherit opacity-60 transition-opacity hover:opacity-100"
                aria-label="Close settings"
              >
                <span>CLOSE</span>
                <X size={14} />
              </button>
            </Dialog.Close>
          </header>

          <div className="overflow-y-auto px-6 py-6">
            <div className="mb-7 flex items-start gap-4">
              <Database size={17} strokeWidth={1.4} className="mt-1 shrink-0 opacity-55" aria-hidden />
              <div>
                <Dialog.Title className="font-serif text-[25px] font-medium leading-none tracking-[-0.01em]">
                  Archive settings
                </Dialog.Title>
                <Dialog.Description className="mt-2 max-w-md font-serif text-[13px] leading-relaxed opacity-65">
                  Inspect the local index and manage its on-disk catalogue.
                </Dialog.Description>
              </div>
            </div>

            <section aria-labelledby="index-settings-heading">
              <h2
                id="index-settings-heading"
                className="mb-2 font-mono text-[9px] uppercase tracking-[0.16em] opacity-45"
              >
                Index
              </h2>
              <dl className="border-y border-[#2b2623]/15 font-mono text-[9px] uppercase tracking-[0.1em] dark:border-[#d4cbbd]/15">
                <SettingRow label="Engine" value={engine === 'rs' ? 'Rust observation' : 'Resolving'} />
                <SettingRow label="Status" value="Ready" />
                <SettingRow label="Segments" value={stats ? formatNumber(stats.totalSegments) : '—'} />
                <SettingRow label="Search entries" value={stats ? formatNumber(stats.searchIndexed) : '—'} />
                <SettingRow label="Database size" value={stats ? formatBytes(stats.dbSizeBytes) : '—'} last />
              </dl>
            </section>

            <section className="mt-7" aria-labelledby="canonical-settings-heading">
              <h2
                id="canonical-settings-heading"
                className="mb-2 font-mono text-[9px] uppercase tracking-[0.16em] opacity-45"
              >
                Canonical observation
              </h2>
              <dl className="border-y border-[#2b2623]/15 font-mono text-[9px] uppercase tracking-[0.1em] dark:border-[#d4cbbd]/15">
                <SettingRow label="Owner" value={canonicalState} />
                <SettingRow label="Commit" value={canonicalStats ? formatNumber(canonicalStats.atCommitSeq) : '—'} />
                <SettingRow
                  label="Searchable messages"
                  value={canonicalStats ? formatNumber(canonicalStats.searchableMessages) : '—'}
                />
                <SettingRow
                  label="Source objects"
                  value={canonicalStats ? formatNumber(canonicalStats.activeSourceObjects) : '—'}
                />
                <SettingRow
                  label="Database allocation"
                  value={canonicalStats ? formatBytes(canonicalStats.allocatedDatabaseBytes) : '—'}
                  last
                />
              </dl>
              <p className="mt-2 font-serif text-[11px] leading-relaxed opacity-55">
                {canonicalError
                  ? `Canonical read unavailable: ${canonicalError}`
                  : observationStatus && !observationStatus.enabled
                    ? 'The RFC 011 observation owner is not enabled for this run.'
                    : canonicalStatsLoading
                      ? 'Negotiating the framed utility-process client…'
                      : canonicalStats
                        ? 'Read from the Rust-owned catalog through the versioned client boundary.'
                        : 'Canonical statistics are not available.'}
              </p>
            </section>

            <section className="mt-7" aria-labelledby="maintenance-heading">
              <h2 id="maintenance-heading" className="mb-2 font-mono text-[9px] uppercase tracking-[0.16em] opacity-45">
                Maintenance
              </h2>
              <div className="border border-[#2b2623]/15 p-4 dark:border-[#d4cbbd]/15">
                <div className="flex items-start gap-5">
                  <div className="min-w-0 flex-1">
                    <p className="font-serif text-[14px] leading-none">Rebuild local index</p>
                    <p className="mt-2 font-serif text-[12px] leading-relaxed opacity-60">
                      Discard the current catalogue and ingest agent history again from disk.
                    </p>
                  </div>
                  <button
                    type="button"
                    onClick={onRebuild}
                    disabled={rebuilding}
                    className="inline-flex shrink-0 cursor-pointer items-center gap-2 border border-sanguine/40 bg-transparent px-3 py-1.5 font-mono text-[9px] uppercase tracking-[0.12em] text-sanguine transition-colors hover:bg-sanguine hover:text-[#f8f6f0] disabled:cursor-not-allowed disabled:opacity-30 dark:hover:text-[#11100f]"
                  >
                    <RotateCcw size={11} aria-hidden />
                    {rebuilding ? 'Rebuilding' : 'Rebuild'}
                  </button>
                </div>
              </div>
            </section>
          </div>
        </Dialog.Content>
      </Dialog.Portal>
    </Dialog.Root>
  );
}

function SettingRow({ label, value, last = false }: { label: string; value: string; last?: boolean }) {
  return (
    <div
      className={`flex items-center justify-between gap-6 px-3 py-2.5 ${
        last ? '' : 'border-b border-[#2b2623]/10 dark:border-[#d4cbbd]/10'
      }`}
    >
      <dt className="opacity-45">{label}</dt>
      <dd className="text-right tracking-[0.08em] opacity-80">{value}</dd>
    </div>
  );
}

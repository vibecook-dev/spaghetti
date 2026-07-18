import { formatElapsed, phaseLabel, type ProgressSnapshot, type SourceProgressState } from '../lib/source-progress.js';
import { paperStyle } from '../lib/archive-theme.js';
import { TrafficLights } from './TrafficLights.js';

export interface LoadingScreenProps {
  sources: SourceProgressState[];
  progress: ProgressSnapshot | null;
  elapsedMs: number;
  engine: 'rs' | 'ts' | null;
  error: string | null;
  headline?: string;
  onRetry?: () => void;
  retrying?: boolean;
  isDark?: boolean;
}

/**
 * Full-viewport cold-start / rebuild — archive paper frame.
 */
export function LoadingScreen({
  sources,
  progress,
  elapsedMs,
  engine,
  error,
  headline = 'Indexing agent history',
  onRetry,
  retrying = false,
  isDark = false,
}: LoadingScreenProps) {
  const active = sources.find((s) => s.stage === 'active');
  const hasDeterminate = progress?.total != null && progress.total > 0 && progress.current != null;
  const busy = !error || retrying;

  // Match the reference composition even during cold ingest: matte outside,
  // constrained paper frame inside, then the quiet centered archive ledger.
  return (
    <div
      className={`h-full w-full flex flex-col overflow-hidden border border-[color:var(--archive-ink-line-outer)] transition-colors duration-500 ${
        isDark ? 'dark text-[#d4cbbd]' : 'text-[#2b2623]'
      }`}
      style={paperStyle(isDark)}
      role="status"
      aria-live="polite"
      aria-busy={busy}
    >
      {/* Drag strip + macOS-style lights (same inset rhythm as the main shell). */}
      <div className="titlebar-drag h-12 shrink-0 flex items-center px-6">
        <TrafficLights />
      </div>
      <div className="flex-1 flex items-center justify-center min-h-0">
        <div className="w-full max-w-md p-8">
          <header className="flex items-center justify-between mb-8">
            <div className="font-serif text-[11px] tracking-[0.2em] flex items-center gap-2">
              <span className="w-1.5 h-1.5 bg-current inline-block" />
              spaghetti
            </div>
            <div className="flex items-center gap-3 font-mono text-[9px] tracking-widest uppercase opacity-60">
              {engine && <span>{engine === 'rs' ? 'native' : 'typescript'}</span>}
              <span>{formatElapsed(elapsedMs)}</span>
            </div>
          </header>

          <p className="font-mono text-[9px] tracking-[0.2em] uppercase opacity-50 mb-2">Archive</p>
          <h1 className="font-serif text-2xl tracking-tight mb-2">
            {error && !retrying ? 'Something went wrong' : headline}
          </h1>
          <p className="font-serif text-[13px] opacity-60 mb-6 leading-relaxed">
            {error && !retrying
              ? 'The index could not be built. You can delete the local cache and rebuild from disk.'
              : 'Reading on-disk sessions into a local SQLite index.'}
          </p>

          {/* Progress bar */}
          <div className="mb-6">
            <div className="h-px bg-ink/15 relative overflow-hidden">
              {(!error || retrying) && (
                <div
                  className="absolute inset-y-0 left-0 bg-ink/60"
                  style={
                    hasDeterminate && progress
                      ? { width: `${Math.min(100, ((progress.current ?? 0) / (progress.total ?? 1)) * 100)}%` }
                      : { width: '40%', animation: 'ls-sweep 1.4s ease-in-out infinite' }
                  }
                />
              )}
            </div>
            <div className="flex justify-between mt-2 font-mono text-[9px] tracking-widest uppercase opacity-50">
              <span>{error && !retrying ? 'Failed' : progress ? phaseLabel(progress.phase) : 'Starting'}</span>
              <span>
                {hasDeterminate && progress ? `${progress.current} / ${progress.total}` : active ? active.label : '—'}
              </span>
            </div>
          </div>

          <ul className="space-y-2 mb-6">
            {sources.map((s) => (
              <li key={s.id} className="flex items-start gap-2 font-mono text-[10px]">
                <span
                  className={`mt-1 w-1.5 h-1.5 shrink-0 ${
                    s.stage === 'done' ? 'bg-verdigris' : s.stage === 'active' ? 'bg-ink animate-pulse' : 'bg-ink/25'
                  }`}
                />
                <div className="min-w-0 flex-1">
                  <div className="flex justify-between gap-2">
                    <span className="tracking-wide uppercase opacity-80">{s.label}</span>
                    <span className="opacity-50 shrink-0">
                      {s.stage === 'done' && 'Done'}
                      {s.stage === 'active' && (s.fraction != null ? `${Math.round(s.fraction * 100)}%` : 'Working')}
                      {s.stage === 'pending' && 'Waiting'}
                    </span>
                  </div>
                  {s.stage === 'active' && s.detail ? (
                    <p className="mt-0.5 opacity-40 truncate normal-case tracking-normal">{s.detail}</p>
                  ) : null}
                </div>
              </li>
            ))}
          </ul>

          {error && !retrying && (
            <div className="border border-sanguine/25 p-3 mb-4">
              <p className="font-mono text-[9px] tracking-widest uppercase text-sanguine mb-2">Error</p>
              <pre className="font-mono text-[10px] whitespace-pre-wrap break-all opacity-80 max-h-28 overflow-auto">
                {error}
              </pre>
              {onRetry && (
                <button
                  type="button"
                  className="mt-3 font-mono text-[9px] tracking-widest uppercase border border-[color:var(--archive-ink-line)] px-3 py-1.5 bg-transparent text-ink cursor-pointer hover:bg-ink hover:text-paper transition-colors"
                  onClick={() => onRetry()}
                  disabled={retrying}
                >
                  Delete cache &amp; retry
                </button>
              )}
            </div>
          )}

          <footer className="flex items-center gap-2 font-mono text-[8px] tracking-widest uppercase opacity-40 pt-2 border-t border-[color:var(--archive-ink-line-soft)]">
            <span>Local-first</span>
            <span>·</span>
            <span>~/.claude · ~/.codex · ~/.grok</span>
          </footer>
        </div>
      </div>

      <style>{`
        @keyframes ls-sweep {
          0% { transform: translateX(-100%); }
          100% { transform: translateX(350%); }
        }
      `}</style>
    </div>
  );
}

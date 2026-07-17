import { formatElapsed, phaseLabel, type ProgressSnapshot, type SourceProgressState } from '../lib/source-progress.js';

export interface LoadingScreenProps {
  sources: SourceProgressState[];
  progress: ProgressSnapshot | null;
  /** Wall-clock ms since load started. */
  elapsedMs: number;
  engine: 'rs' | 'ts' | null;
  error: string | null;
  /** e.g. "Indexing agent history" / "Rebuilding index" */
  headline?: string;
  /** Wipe cache + re-ingest (shown when init failed). */
  onRetry?: () => void;
  retrying?: boolean;
}

/**
 * Full-viewport cold-start / rebuild screen.
 * Monochrome, quiet typography — premium desktop feel.
 * Master bar is an indeterminate left→right sweep (multi-source totals
 * are not a single reliable 0–100 scale).
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
}: LoadingScreenProps) {
  const active = sources.find((s) => s.stage === 'active');
  const hasDeterminate = progress?.total != null && progress.total > 0 && progress.current != null;
  const busy = !error || retrying;

  return (
    <div className="ls-root" role="status" aria-live="polite" aria-busy={busy}>
      <div className="ls-noise" aria-hidden />
      <div className="ls-frame">
        {/* Brand rail */}
        <header className="ls-header">
          <div className="ls-mark">
            <span className="ls-mark-dot" aria-hidden />
            <span className="ls-brand">spaghetti</span>
          </div>
          <div className="ls-meta">
            {engine && (
              <span className="ls-chip" title={engine === 'rs' ? 'Native Rust engine' : 'TypeScript engine'}>
                {engine === 'rs' ? 'native' : 'typescript'}
              </span>
            )}
            <span className="ls-elapsed">{formatElapsed(elapsedMs)}</span>
          </div>
        </header>

        <main className="ls-main">
          <div className="ls-title-block">
            <p className="ls-kicker">Playground</p>
            <h1 className="ls-title">{error && !retrying ? 'Something went wrong' : headline}</h1>
            <p className="ls-subtitle">
              {error && !retrying
                ? 'The index could not be built. You can delete the local cache and rebuild from disk.'
                : 'Reading on-disk sessions into a local SQLite index.'}
            </p>
          </div>

          {/* Indeterminate sweep — not a global % (multi-source phases don't sum cleanly) */}
          <div className="ls-bar-wrap">
            <div className="ls-bar-track" aria-hidden={!!error && !retrying}>
              {(!error || retrying) && <div className="ls-bar-fill ls-bar-fill--indeterminate" />}
            </div>
            <div className="ls-bar-caption">
              <span className="ls-bar-phase">
                {error && !retrying ? 'Failed' : progress ? phaseLabel(progress.phase) : 'Starting'}
              </span>
              <span className="ls-bar-count">
                {hasDeterminate && progress ? `${progress.current} / ${progress.total}` : active ? active.label : '—'}
              </span>
            </div>
          </div>

          {/* Per-source rows */}
          <ul className="ls-sources">
            {sources.map((s) => (
              <li key={s.id} className={`ls-source ls-source--${s.stage}`}>
                <span className="ls-source-rail" aria-hidden />
                <div className="ls-source-body">
                  <div className="ls-source-row">
                    <span className="ls-source-name">{s.label}</span>
                    <span className="ls-source-status">
                      {s.stage === 'done' && 'Done'}
                      {s.stage === 'active' && (s.fraction != null ? `${Math.round(s.fraction * 100)}%` : 'Working')}
                      {s.stage === 'pending' && 'Waiting'}
                    </span>
                  </div>
                  {s.stage === 'active' && s.detail && <p className="ls-source-detail">{s.detail}</p>}
                  {s.stage === 'active' && (
                    <div className="ls-source-mini">
                      <div
                        className={`ls-source-mini-fill${s.fraction == null ? ' ls-source-mini-fill--pulse' : ''}`}
                        style={s.fraction != null ? { width: `${Math.round(s.fraction * 1000) / 10}%` } : undefined}
                      />
                    </div>
                  )}
                </div>
              </li>
            ))}
          </ul>

          {error && !retrying && (
            <div className="ls-error">
              <p className="ls-error-label">Error</p>
              <pre className="ls-error-body">{error}</pre>
              {onRetry && (
                <button type="button" className="ls-retry" onClick={() => onRetry()} disabled={retrying}>
                  Delete cache &amp; retry
                </button>
              )}
            </div>
          )}

          {(!error || retrying) && active?.detail && <p className="ls-footer-detail">{active.detail}</p>}
        </main>

        <footer className="ls-footer">
          <span>Local-first · no network</span>
          <span className="ls-footer-sep" aria-hidden />
          <span>~/.claude · ~/.codex · ~/.grok</span>
        </footer>
      </div>

      <style>{loadingCss}</style>
    </div>
  );
}

/** Scoped CSS — no external assets; fine monochrome detail. */
const loadingCss = `
.ls-root {
  position: relative;
  height: 100%;
  width: 100%;
  display: flex;
  align-items: center;
  justify-content: center;
  background: #050505;
  color: #f2f2f2;
  overflow: hidden;
  font-family:
    "SF Pro Text", "Segoe UI", system-ui, -apple-system, sans-serif;
  -webkit-font-smoothing: antialiased;
}

.ls-noise {
  pointer-events: none;
  position: absolute;
  inset: 0;
  opacity: 0.035;
  background-image: url("data:image/svg+xml,%3Csvg viewBox='0 0 256 256' xmlns='http://www.w3.org/2000/svg'%3E%3Cfilter id='n'%3E%3CfeTurbulence type='fractalNoise' baseFrequency='0.85' numOctaves='4' stitchTiles='stitch'/%3E%3C/filter%3E%3Crect width='100%25' height='100%25' filter='url(%23n)'/%3E%3C/svg%3E");
  background-size: 180px 180px;
}

.ls-frame {
  position: relative;
  z-index: 1;
  width: min(440px, calc(100% - 64px));
  display: flex;
  flex-direction: column;
  gap: 0;
  min-height: 420px;
}

.ls-header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  margin-bottom: 48px;
}

.ls-mark {
  display: inline-flex;
  align-items: center;
  gap: 10px;
}

.ls-mark-dot {
  width: 6px;
  height: 6px;
  border-radius: 50%;
  background: #f2f2f2;
  box-shadow: 0 0 0 1px rgba(255,255,255,0.15), 0 0 12px rgba(255,255,255,0.25);
  animation: ls-breathe 2.8s ease-in-out infinite;
}

.ls-brand {
  font-size: 11px;
  font-weight: 500;
  letter-spacing: 0.22em;
  text-transform: lowercase;
  color: rgba(242,242,242,0.72);
}

.ls-meta {
  display: flex;
  align-items: center;
  gap: 10px;
  font-variant-numeric: tabular-nums;
}

.ls-chip {
  font-size: 9px;
  letter-spacing: 0.14em;
  text-transform: uppercase;
  padding: 3px 7px;
  border: 1px solid rgba(255,255,255,0.12);
  color: rgba(242,242,242,0.55);
  border-radius: 2px;
}

.ls-elapsed {
  font-family: "SF Mono", ui-monospace, Menlo, Consolas, monospace;
  font-size: 11px;
  color: rgba(242,242,242,0.4);
  letter-spacing: 0.04em;
}

.ls-main {
  flex: 1;
  display: flex;
  flex-direction: column;
}

.ls-title-block {
  margin-bottom: 36px;
}

.ls-kicker {
  margin: 0 0 10px;
  font-size: 10px;
  letter-spacing: 0.2em;
  text-transform: uppercase;
  color: rgba(242,242,242,0.35);
}

.ls-title {
  margin: 0 0 10px;
  font-size: 26px;
  font-weight: 450;
  letter-spacing: -0.03em;
  line-height: 1.15;
  color: #fafafa;
}

.ls-subtitle {
  margin: 0;
  font-size: 13px;
  line-height: 1.5;
  color: rgba(242,242,242,0.42);
  max-width: 34em;
}

.ls-bar-wrap {
  margin-bottom: 32px;
}

.ls-bar-track {
  height: 1px;
  background: rgba(255,255,255,0.08);
  position: relative;
  overflow: hidden;
}

.ls-bar-fill {
  height: 100%;
  background: #f2f2f2;
  box-shadow: 0 0 14px rgba(255,255,255,0.4);
  will-change: transform;
}

/* Continuous left → right sweep (not a progress percentage). */
.ls-bar-fill--indeterminate {
  position: absolute;
  top: 0;
  left: 0;
  width: 28%;
  animation: ls-sweep 1.35s cubic-bezier(0.4, 0, 0.2, 1) infinite;
}

.ls-bar-caption {
  display: flex;
  justify-content: space-between;
  margin-top: 10px;
  font-size: 10px;
  letter-spacing: 0.12em;
  text-transform: uppercase;
  color: rgba(242,242,242,0.38);
  font-variant-numeric: tabular-nums;
}

.ls-bar-count {
  font-family: "SF Mono", ui-monospace, Menlo, Consolas, monospace;
  letter-spacing: 0.06em;
}

.ls-sources {
  list-style: none;
  margin: 0;
  padding: 0;
  border-top: 1px solid rgba(255,255,255,0.06);
}

.ls-source {
  display: flex;
  gap: 14px;
  padding: 14px 0;
  border-bottom: 1px solid rgba(255,255,255,0.06);
  transition: opacity 200ms ease;
}

.ls-source--pending {
  opacity: 0.38;
}

.ls-source--done {
  opacity: 0.55;
}

.ls-source--active {
  opacity: 1;
}

.ls-source-rail {
  width: 1px;
  flex-shrink: 0;
  margin-top: 4px;
  margin-bottom: 4px;
  background: rgba(255,255,255,0.1);
  position: relative;
}

.ls-source--active .ls-source-rail {
  background: #f2f2f2;
  box-shadow: 0 0 8px rgba(255,255,255,0.4);
}

.ls-source--done .ls-source-rail {
  background: rgba(255,255,255,0.28);
}

.ls-source-body {
  flex: 1;
  min-width: 0;
}

.ls-source-row {
  display: flex;
  align-items: baseline;
  justify-content: space-between;
  gap: 16px;
}

.ls-source-name {
  font-size: 13px;
  font-weight: 450;
  letter-spacing: -0.01em;
  color: #f2f2f2;
}

.ls-source--pending .ls-source-name {
  color: rgba(242,242,242,0.7);
}

.ls-source-status {
  font-family: "SF Mono", ui-monospace, Menlo, Consolas, monospace;
  font-size: 10px;
  letter-spacing: 0.08em;
  text-transform: uppercase;
  color: rgba(242,242,242,0.4);
  flex-shrink: 0;
}

.ls-source--active .ls-source-status {
  color: rgba(242,242,242,0.75);
}

.ls-source-detail {
  margin: 6px 0 0;
  font-size: 11px;
  line-height: 1.4;
  color: rgba(242,242,242,0.38);
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}

.ls-source-mini {
  margin-top: 10px;
  height: 1px;
  background: rgba(255,255,255,0.06);
  overflow: hidden;
}

.ls-source-mini-fill {
  height: 100%;
  background: rgba(255,255,255,0.55);
  transition: width 200ms ease;
}

.ls-source-mini-fill--pulse {
  width: 28% !important;
  animation: ls-slide 1.2s ease-in-out infinite;
}

.ls-error {
  margin-top: 28px;
  padding: 14px 16px;
  border: 1px solid rgba(255,255,255,0.12);
  background: rgba(255,255,255,0.02);
}

.ls-error-label {
  margin: 0 0 8px;
  font-size: 10px;
  letter-spacing: 0.16em;
  text-transform: uppercase;
  color: rgba(242,242,242,0.45);
}

.ls-error-body {
  margin: 0;
  font-family: "SF Mono", ui-monospace, Menlo, Consolas, monospace;
  font-size: 11px;
  line-height: 1.5;
  color: rgba(242,242,242,0.7);
  white-space: pre-wrap;
  word-break: break-word;
}

.ls-retry {
  margin-top: 14px;
  font-size: 11px;
  letter-spacing: 0.06em;
  padding: 8px 14px;
  border-radius: 2px;
  border: 1px solid rgba(255,255,255,0.22);
  background: rgba(255,255,255,0.06);
  color: #f2f2f2;
  cursor: pointer;
  font-family: inherit;
}

.ls-retry:hover {
  background: rgba(255,255,255,0.1);
  border-color: rgba(255,255,255,0.35);
}

.ls-retry:disabled {
  opacity: 0.45;
  cursor: default;
}

.ls-footer-detail {
  margin: 24px 0 0;
  font-size: 11px;
  color: rgba(242,242,242,0.28);
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}

.ls-footer {
  margin-top: 48px;
  display: flex;
  align-items: center;
  gap: 10px;
  font-size: 10px;
  letter-spacing: 0.08em;
  text-transform: uppercase;
  color: rgba(242,242,242,0.22);
}

.ls-footer-sep {
  width: 3px;
  height: 3px;
  border-radius: 50%;
  background: rgba(255,255,255,0.2);
}

@keyframes ls-slide {
  0% { transform: translateX(-120%); }
  100% { transform: translateX(400%); }
}

@keyframes ls-sweep {
  0% { transform: translateX(-100%); opacity: 0; }
  15% { opacity: 1; }
  85% { opacity: 1; }
  100% { transform: translateX(360%); opacity: 0; }
}

@keyframes ls-breathe {
  0%, 100% { opacity: 0.55; transform: scale(1); }
  50% { opacity: 1; transform: scale(1.15); }
}

@media (prefers-reduced-motion: reduce) {
  .ls-bar-fill--indeterminate,
  .ls-source-mini-fill--pulse,
  .ls-mark-dot {
    animation: none;
  }
  .ls-bar-fill--indeterminate {
    width: 100%;
    opacity: 0.35;
    transform: none;
  }
}
`;

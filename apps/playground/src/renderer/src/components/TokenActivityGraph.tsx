import { useEffect, useMemo, useState, type ReactNode } from 'react';
import type { ProjectLocator, TokenActivityDay, TokenActivityResult } from '@vibecook/spaghetti-sdk';
import { formatTokens } from '../lib/format.js';
import { sourceLabel } from '../lib/source-progress.js';

const WEEK_COUNT = 53;
const DAY_MS = 86_400_000;
const LEVEL_OPACITY = [0.06, 0.24, 0.43, 0.67, 0.95] as const;

function dateKey(date: Date): string {
  return date.toISOString().slice(0, 10);
}

function addUtcDays(date: Date, days: number): Date {
  return new Date(date.getTime() + days * DAY_MS);
}

function buildWeeks(): { weeks: Date[][]; from: string; to: string; today: string } {
  const now = new Date();
  const todayDate = new Date(Date.UTC(now.getUTCFullYear(), now.getUTCMonth(), now.getUTCDate()));
  const end = addUtcDays(todayDate, 6 - todayDate.getUTCDay());
  const start = addUtcDays(end, -(WEEK_COUNT * 7 - 1));
  // The visual grid includes partial boundary weeks, but RFC 011 usage
  // activity permits at most 366 inclusive days. Query exactly the trailing
  // year and leave the few leading grid cells empty.
  const queryStart = addUtcDays(todayDate, -365);
  const weeks = Array.from({ length: WEEK_COUNT }, (_, week) =>
    Array.from({ length: 7 }, (_, day) => addUtcDays(start, week * 7 + day)),
  );
  return { weeks, from: dateKey(queryStart), to: dateKey(todayDate), today: dateKey(todayDate) };
}

function percentile95(values: number[]): number {
  if (values.length === 0) return 0;
  const sorted = [...values].sort((a, b) => a - b);
  return sorted[Math.min(sorted.length - 1, Math.floor(sorted.length * 0.95))]!;
}

function activityLevel(total: number, ceiling: number): number {
  if (total <= 0 || ceiling <= 0) return 0;
  const ratio = Math.min(1, Math.log1p(total) / Math.log1p(ceiling));
  return Math.max(1, Math.min(4, Math.ceil(ratio * 4)));
}

function dayTitle(day: TokenActivityDay | undefined, date: Date): string {
  const label = date.toLocaleDateString(undefined, {
    timeZone: 'UTC',
    year: 'numeric',
    month: 'long',
    day: 'numeric',
  });
  if (!day) return `${label}\nNo recorded token activity`;
  const quality = day.quality === 'exact' ? 'exact' : `${day.quality} attribution`;
  const agents = day.sourceIds.map(sourceLabel).join(', ');
  return [
    label,
    `${formatTokens(day.tokenUsage.totalTokens)} normalized tokens · ${quality}`,
    `${formatTokens(day.tokenUsage.inputTokens)} input · ${formatTokens(day.tokenUsage.outputTokens)} output`,
    `${formatTokens(day.tokenUsage.cacheCreationTokens)} cache write · ${formatTokens(day.tokenUsage.cacheReadTokens)} cache read`,
    `${day.messageCount.toLocaleString()} messages · ${day.sessionCount.toLocaleString()} sessions · ${agents}`,
  ].join('\n');
}

export function TokenActivityGraph({
  project,
  sourceId,
  activityRevision,
  header,
  controls,
}: {
  project: ProjectLocator;
  sourceId?: string | null;
  activityRevision: number;
  header: ReactNode;
  controls?: ReactNode;
}) {
  const calendar = useMemo(buildWeeks, []);
  const [result, setResult] = useState<TokenActivityResult | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    // Coalesce bursty live-message notifications into one derived-day read.
    const timer = window.setTimeout(() => {
      void window.spaghetti
        .getProjectTokenActivity(project, {
          from: calendar.from,
          to: calendar.to,
          ...(sourceId ? { sourceId } : {}),
        })
        .then((next) => {
          if (cancelled) return;
          setResult(next);
          setError(null);
        })
        .catch((reason: unknown) => {
          if (!cancelled) setError(reason instanceof Error ? reason.message : String(reason));
        });
    }, 250);
    return () => {
      cancelled = true;
      window.clearTimeout(timer);
    };
  }, [activityRevision, calendar.from, calendar.to, project, sourceId]);

  const days = useMemo(() => new Map(result?.days.map((day) => [day.date, day]) ?? []), [result]);
  const ceiling = useMemo(
    () => percentile95((result?.days ?? []).map((day) => day.tokenUsage.totalTokens).filter((total) => total > 0)),
    [result],
  );
  const total = useMemo(() => (result?.days ?? []).reduce((sum, day) => sum + day.tokenUsage.totalTokens, 0), [result]);
  const hasEstimate = result?.days.some((day) => day.quality === 'estimated' || day.quality === 'mixed') ?? false;

  return (
    <section className="shrink-0 border-b border-[color:var(--archive-ink-line)] px-6 py-2">
      <div className="flex min-h-6 items-center justify-between gap-4">
        <div className="flex min-w-0 items-center gap-3">
          {header}
          <span className="h-3 w-px shrink-0 bg-ink/15" aria-hidden="true" />
          <span className="shrink-0 font-mono text-[8px] tabular-nums opacity-45">
            {result ? `${hasEstimate ? '~' : ''}${formatTokens(total)} tokens · past year` : 'loading tokens…'}
          </span>
          {error ? <span className="truncate font-mono text-[8px] text-sanguine">{error}</span> : null}
        </div>
        {controls}
      </div>

      <div className="mt-1.5 w-max max-w-full overflow-x-auto scrollbar-hide">
        <div className="mb-1 flex gap-[2px] pl-[18px]">
          {calendar.weeks.map((week, index) => {
            const firstOfMonth = week.find((date) => date.getUTCDate() === 1);
            const showMonth = firstOfMonth && firstOfMonth.getUTCMonth() % 2 === 0;
            return (
              <span key={index} className="w-[5px] shrink-0 font-mono text-[6px] opacity-35">
                {showMonth ? firstOfMonth.toLocaleDateString(undefined, { timeZone: 'UTC', month: 'short' }) : ''}
              </span>
            );
          })}
        </div>

        <div className="flex gap-[2px]">
          <div className="grid w-4 shrink-0 grid-rows-7 gap-[2px] font-mono text-[6px] leading-[5px] opacity-35">
            <span />
            <span>Mon</span>
            <span />
            <span>Wed</span>
            <span />
            <span>Fri</span>
            <span />
          </div>
          <div className="flex gap-[2px]">
            {calendar.weeks.map((week, weekIndex) => (
              <div key={weekIndex} className="grid w-[5px] shrink-0 grid-rows-7 gap-[2px]">
                {week.map((date) => {
                  const key = dateKey(date);
                  const day = days.get(key);
                  const future = key > calendar.today;
                  const level = future ? 0 : activityLevel(day?.tokenUsage.totalTokens ?? 0, ceiling);
                  return (
                    <span
                      key={key}
                      className={`h-[5px] w-[5px] border transition-colors ${
                        future ? 'border-transparent bg-transparent' : 'border-[color:var(--archive-ink-line-soft)]'
                      }`}
                      style={
                        future
                          ? undefined
                          : { backgroundColor: `rgb(var(--archive-live-rgb) / ${LEVEL_OPACITY[level]})` }
                      }
                      title={future ? undefined : dayTitle(day, date)}
                      aria-label={future ? undefined : dayTitle(day, date).replaceAll('\n', ', ')}
                    />
                  );
                })}
              </div>
            ))}
          </div>
        </div>

        <div className="mt-1 flex items-center justify-end gap-1.5">
          <span className="font-mono text-[6px] uppercase tracking-wider opacity-35">less</span>
          {LEVEL_OPACITY.map((opacity, level) => (
            <span
              key={opacity}
              className="h-1.5 w-1.5 border border-[color:var(--archive-ink-line-soft)]"
              style={{ backgroundColor: `rgb(var(--archive-live-rgb) / ${level === 0 ? 0.06 : opacity})` }}
            />
          ))}
          <span className="font-mono text-[6px] uppercase tracking-wider opacity-35">more</span>
        </div>
      </div>
    </section>
  );
}

/**
 * Turning a native ingest error report into something a person can act on.
 *
 * The addon returns up to 100 errors plus an uncapped `errorCount`, so the
 * summary has to distinguish "3 files failed" from "3 shown, 4,000 failed" —
 * printing the first hundred as if they were all of them is exactly the
 * silent under-reporting the cap exists to avoid.
 *
 * Introduced by RFC 008 Phase 2.
 */

import type { ErrorSink } from '../io/error-sink.js';
import type { NativeIngestError, NativeIngestErrorReport } from '../legacy-native.js';

/** How many paths to name before collapsing the rest into a count. */
const NAMED_PATHS = 5;

/**
 * A one-line summary naming the affected files.
 *
 * Returns `null` when the report is clean, so callers can use it as the
 * "should I warn at all" test rather than repeating the emptiness check.
 */
export function summarizeIngestErrors(sourceId: string, report: NativeIngestErrorReport): string | null {
  if (report.errorCount === 0) return null;

  const bySeverity = countBySeverity(report.errors);
  const parts: string[] = [];
  if (bySeverity['project-fatal']) parts.push(`${bySeverity['project-fatal']} project(s) rolled back`);
  if (bySeverity.source) parts.push(`${bySeverity.source} unreadable location(s)`);
  if (bySeverity['record-skip']) parts.push(`${bySeverity['record-skip']} record(s) skipped`);

  const named = report.errors.slice(0, NAMED_PATHS).map((e) => e.path);
  const unnamed = report.errorCount - named.length;
  const files = named.length > 0 ? ` — ${named.join(', ')}${unnamed > 0 ? ` (+${unnamed} more)` : ''}` : '';

  // `errorCount` rather than `errors.length`: the list is capped, the count is not.
  const detail = parts.length > 0 ? ` (${parts.join(', ')})` : '';
  return `${sourceId}: ${report.errorCount} ingest issue(s)${detail}${files}. Affected inputs will be retried on the next run.`;
}

function countBySeverity(errors: NativeIngestError[]): Record<string, number> {
  const out: Record<string, number> = {};
  for (const e of errors) out[e.severity] = (out[e.severity] ?? 0) + 1;
  return out;
}

/**
 * Route a native ingest report to the error sink.
 *
 * A no-op on a clean run. Errors here are non-fatal by construction — the
 * ingest completed and the affected inputs kept their retry — so this reports
 * rather than throws.
 */
export function reportIngestErrors(sourceId: string, report: NativeIngestErrorReport, errorSink?: ErrorSink): void {
  const summary = summarizeIngestErrors(sourceId, report);
  if (!summary || !errorSink) return;

  errorSink.error(new Error(summary), {
    component: 'NativeIngest',
    sourceId,
    errorCount: report.errorCount,
    errorsTruncated: report.errorsTruncated,
    // The full capped list, so a structured sink can render more than the
    // one-line summary without re-deriving it.
    errors: report.errors,
  });
}

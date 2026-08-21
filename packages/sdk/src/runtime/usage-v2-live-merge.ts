/**
 * Typed RFC 012C durable + scoped usage merge consumer.
 *
 * Merged contributions carry SemanticRevisionRef. Overlay replacement is
 * grouped by fact identity; occurrence-scoped event_id deduplicates delivery.
 * Overlay retirement is coverage-gated. This module never parses native JSON
 * payloads.
 */

import { compareCoverage, type SemanticRevisionRef, type SourceCoverageSet } from '../contracts/rfc012a.js';

export type ScopedUsageOperation = 'upsert' | 'retract';

export interface DurableUsageContribution {
  factId: string;
  semanticRevisionRef: SemanticRevisionRef;
}

export interface ScopedUsageObserverEvent {
  eventId: string;
  factId: string;
  semanticRevisionRef: SemanticRevisionRef;
  operation: ScopedUsageOperation;
}

export type OverlayDisposition = 'retired' | { retained: { stale: true } };

export interface MergedUsageContribution {
  factId: string;
  semanticRevisionRef: SemanticRevisionRef;
  origin: 'durable' | 'overlay';
}

export interface DurableLiveUsageMerge {
  contributions: MergedUsageContribution[];
  overlay: OverlayDisposition;
  deliveredObserverOccurrences: Array<{
    eventId: string;
    factId: string;
    semanticRevisionRef: SemanticRevisionRef;
  }>;
}

function overlayDisposition(
  observerCoverage: SourceCoverageSet,
  durableCoverage: SourceCoverageSet,
): OverlayDisposition {
  const comparison = compareCoverage(observerCoverage, durableCoverage);
  if (
    observerCoverage.completeness === 'complete' &&
    (comparison === 'equal' || comparison === 'dominates' || comparison === 'behind')
  ) {
    return 'retired';
  }
  return { retained: { stale: true } };
}

/**
 * Merge durable usage-v2 contributions with typed scoped observer events.
 * Duplicate event_id delivery is ignored; A→B→A with distinct event_ids is kept.
 */
export function mergeDurableAndScopedUsage(
  durable: readonly DurableUsageContribution[],
  durableCoverage: SourceCoverageSet,
  observerEvents: readonly ScopedUsageObserverEvent[],
  observerCoverage: SourceCoverageSet,
): DurableLiveUsageMerge {
  const disposition = overlayDisposition(observerCoverage, durableCoverage);
  const delivered: DurableLiveUsageMerge['deliveredObserverOccurrences'] = [];
  const seenEventIds = new Set<string>();
  const overlay = new Map<string, ScopedUsageObserverEvent>();
  const retracted = new Set<string>();
  for (const event of observerEvents) {
    if (seenEventIds.has(event.eventId)) {
      continue;
    }
    seenEventIds.add(event.eventId);
    delivered.push({
      eventId: event.eventId,
      factId: event.factId,
      semanticRevisionRef: event.semanticRevisionRef,
    });
    if (event.operation === 'retract') {
      overlay.delete(event.factId);
      retracted.add(event.factId);
    } else {
      retracted.delete(event.factId);
      overlay.set(event.factId, event);
    }
  }

  if (disposition === 'retired') {
    return {
      contributions: durable.map((item) => ({
        factId: item.factId,
        semanticRevisionRef: item.semanticRevisionRef,
        origin: 'durable',
      })),
      overlay: disposition,
      deliveredObserverOccurrences: delivered,
    };
  }

  const contributions: MergedUsageContribution[] = [];
  const seenFacts = new Set<string>();
  for (const item of durable) {
    const live = overlay.get(item.factId);
    if (live) {
      contributions.push({
        factId: live.factId,
        semanticRevisionRef: live.semanticRevisionRef,
        origin: 'overlay',
      });
    } else if (retracted.has(item.factId)) {
      continue;
    } else {
      contributions.push({
        factId: item.factId,
        semanticRevisionRef: item.semanticRevisionRef,
        origin: 'durable',
      });
    }
    seenFacts.add(item.factId);
  }
  for (const live of overlay.values()) {
    if (seenFacts.has(live.factId)) {
      continue;
    }
    contributions.push({
      factId: live.factId,
      semanticRevisionRef: live.semanticRevisionRef,
      origin: 'overlay',
    });
  }
  return {
    contributions,
    overlay: disposition,
    deliveredObserverOccurrences: delivered,
  };
}

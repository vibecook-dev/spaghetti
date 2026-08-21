"""X1: compare complete-only FTS strategies on a frozen timeline."""

from __future__ import annotations

from dataclasses import dataclass


@dataclass(frozen=True)
class IngestMilestone:
    label: str
    history_complete: bool
    catalog_complete: bool
    fts_complete: bool
    t_ms: int


@dataclass(frozen=True)
class StrategyResult:
    name: str
    search_available_at_ms: int | None
    search_visible_before_complete: bool
    quiescence_windows: int


def deferred_one_shot_after_history(milestones: list[IngestMilestone]) -> StrategyResult:
    """Search stays unavailable until history is complete, then one FTS pass."""
    search_at = None
    for item in milestones:
        if item.history_complete and item.fts_complete:
            search_at = item.t_ms
            break
    return StrategyResult(
        name="deferred-one-shot-after-history",
        search_available_at_ms=search_at,
        search_visible_before_complete=False,
        quiescence_windows=1 if search_at is not None else 0,
    )


def incremental_after_catalog(milestones: list[IngestMilestone]) -> StrategyResult:
    """FTS maintenance may start after catalog, but queries stay complete-only."""
    catalog_at = next((item.t_ms for item in milestones if item.catalog_complete), None)
    search_at = next((item.t_ms for item in milestones if item.fts_complete), None)
    if catalog_at is None or search_at is None or search_at < catalog_at:
        search_at = next(
            (
                item.t_ms
                for item in milestones
                if item.catalog_complete and item.fts_complete
            ),
            None,
        )
    return StrategyResult(
        name="incremental-after-catalog",
        search_available_at_ms=search_at,
        search_visible_before_complete=False,
        quiescence_windows=2 if search_at is not None else 0,
    )


def bounded_chunked_finalization(milestones: list[IngestMilestone]) -> StrategyResult:
    """Chunked FTS with reader quiescence; still complete-only."""
    complete = [item for item in milestones if item.fts_complete]
    return StrategyResult(
        name="bounded-chunked-finalization",
        search_available_at_ms=complete[-1].t_ms if complete else None,
        search_visible_before_complete=False,
        quiescence_windows=max(len(complete), 1) if complete else 0,
    )


FROZEN_TIMELINE = [
    IngestMilestone("catalog-ready", False, True, False, 1_200),
    IngestMilestone("history-ready", True, True, False, 8_400),
    IngestMilestone("fts-chunk-1", True, True, False, 9_100),
    IngestMilestone("fts-complete", True, True, True, 11_000),
]


def compare_strategies(
    milestones: list[IngestMilestone] | None = None,
) -> list[StrategyResult]:
    items = milestones or FROZEN_TIMELINE
    return [
        deferred_one_shot_after_history(items),
        incremental_after_catalog(items),
        bounded_chunked_finalization(items),
    ]

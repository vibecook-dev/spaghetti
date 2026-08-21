"""X1: compare complete-only FTS strategies on a frozen ingest trace."""

from __future__ import annotations

import json
from dataclasses import dataclass
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
TRACE_PATH = Path(__file__).resolve().parent / "fixtures" / "fts-bootstrap-trace.json"


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


def load_frozen_trace(path: Path | None = None) -> list[IngestMilestone]:
    document = json.loads((path or TRACE_PATH).read_text())
    if document.get("complete_only_gate") != "schema_meta.query_bootstrap_state":
        raise ValueError("frozen FTS trace must name the query bootstrap completeness gate")
    milestones = [
        IngestMilestone(
            label=str(item["label"]),
            history_complete=bool(item["history_complete"]),
            catalog_complete=bool(item["catalog_complete"]),
            fts_complete=bool(item["fts_complete"]),
            t_ms=int(item["t_ms"]),
        )
        for item in document["milestones"]
    ]
    if not milestones:
        raise ValueError("frozen FTS trace has no milestones")
    return milestones


def deferred_one_shot_after_history(milestones: list[IngestMilestone]) -> StrategyResult:
    search_at = None
    visible_before = False
    for item in milestones:
        if item.fts_complete and not (item.history_complete and item.catalog_complete):
            visible_before = True
        if item.history_complete and item.catalog_complete and item.fts_complete:
            search_at = item.t_ms
            break
    return StrategyResult(
        name="deferred-one-shot-after-history",
        search_available_at_ms=search_at,
        search_visible_before_complete=visible_before,
        quiescence_windows=1 if search_at is not None else 0,
    )


def incremental_after_catalog(milestones: list[IngestMilestone]) -> StrategyResult:
    catalog_at = next((item.t_ms for item in milestones if item.catalog_complete), None)
    search_at = None
    visible_before = False
    for item in milestones:
        if item.fts_complete and (catalog_at is None or item.t_ms < catalog_at or not item.catalog_complete):
            visible_before = True
        if item.catalog_complete and item.fts_complete:
            search_at = item.t_ms
            break
    return StrategyResult(
        name="incremental-after-catalog",
        search_available_at_ms=search_at,
        search_visible_before_complete=visible_before,
        quiescence_windows=2 if search_at is not None else 0,
    )


def bounded_chunked_finalization(milestones: list[IngestMilestone]) -> StrategyResult:
    complete = [item for item in milestones if item.fts_complete]
    visible_before = any(
        item.fts_complete and not item.catalog_complete for item in milestones
    )
    return StrategyResult(
        name="bounded-chunked-finalization",
        search_available_at_ms=complete[-1].t_ms if complete else None,
        search_visible_before_complete=visible_before,
        quiescence_windows=max(len(complete), 1) if complete else 0,
    )


def compare_strategies(
    milestones: list[IngestMilestone] | None = None,
) -> list[StrategyResult]:
    items = milestones or load_frozen_trace()
    return [
        deferred_one_shot_after_history(items),
        incremental_after_catalog(items),
        bounded_chunked_finalization(items),
    ]

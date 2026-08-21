"""X2: aggregate repeated diagnostics by source/reason/family with provenance."""

from __future__ import annotations

from collections import defaultdict
from dataclasses import dataclass
from datetime import datetime, timezone
from typing import Iterable


@dataclass(frozen=True)
class DiagnosticRow:
    adapter_id: str
    stream: str
    family: str
    reason: str
    source_object: str
    generation: int
    commit_seq: int
    sample_identity: str


@dataclass(frozen=True)
class AggregatedDiagnostic:
    adapter_id: str
    stream: str
    family: str
    reason: str
    count: int
    first_commit_seq: int
    last_commit_seq: int
    sample_identity: str
    provenance_objects: tuple[str, ...]


_DISPOSITION = {
    "record_permanent": "RecordPermanent",
    "transient": "RetryTransient",
    "stream_fatal": "StreamFatal",
    "adapter_fatal": "AdapterFatal",
    "invalid_contract": "InvalidContract",
    "malformed_usage": "RecordPermanent",
}


def _family_for(stream: str, error_class: str) -> str:
    key = stream.lower()
    if "transcript" in key or "session" in key:
        return "runtime.usage-v2"
    if "artifact" in key or "file-history" in key or "backup" in key:
        return "runtime.artifacts"
    if error_class == "malformed_usage":
        return "runtime.usage-v2"
    return "history.message"


def rows_from_sqlite_census(records: Iterable[tuple]) -> list[DiagnosticRow]:
    """Map census-shaped source_record_errors joins into aggregator rows."""
    rows: list[DiagnosticRow] = []
    for adapter_id, stream, error_class, source_object, generation, commit_seq, payload_hex in records:
        rows.append(
            DiagnosticRow(
                adapter_id=str(adapter_id),
                stream=str(stream),
                family=_family_for(str(stream), str(error_class)),
                reason=_DISPOSITION.get(str(error_class), str(error_class)),
                source_object=f"obj-{source_object}",
                generation=int(generation),
                commit_seq=int(commit_seq),
                sample_identity=f"diag:{payload_hex[:12]}",
            )
        )
    return rows


def aggregate_diagnostics(
    rows: Iterable[DiagnosticRow], *, max_examples: int = 8
) -> list[AggregatedDiagnostic]:
    if max_examples < 1 or max_examples > 64:
        raise ValueError("max_examples must be in 1..=64")
    grouped: dict[tuple[str, str, str, str], list[DiagnosticRow]] = defaultdict(list)
    for row in rows:
        if not row.reason or not row.adapter_id:
            raise ValueError("diagnostic rows require adapter and reason")
        grouped[(row.adapter_id, row.stream, row.family, row.reason)].append(row)

    aggregates: list[AggregatedDiagnostic] = []
    for (adapter_id, stream, family, reason), members in grouped.items():
        ordered = sorted(members, key=lambda item: (item.commit_seq, item.source_object))
        objects: list[str] = []
        seen: set[str] = set()
        for member in ordered:
            if member.source_object in seen:
                continue
            seen.add(member.source_object)
            if len(objects) < max_examples:
                objects.append(member.source_object)
        aggregates.append(
            AggregatedDiagnostic(
                adapter_id=adapter_id,
                stream=stream,
                family=family,
                reason=reason,
                count=len(ordered),
                first_commit_seq=ordered[0].commit_seq,
                last_commit_seq=ordered[-1].commit_seq,
                sample_identity=ordered[0].sample_identity,
                provenance_objects=tuple(objects),
            )
        )
    aggregates.sort(key=lambda item: (item.adapter_id, item.stream, item.family, item.reason))
    return aggregates


def row_reduction(raw_count: int, aggregated: list[AggregatedDiagnostic]) -> float:
    if raw_count < 0:
        raise ValueError("raw_count must be >= 0")
    if raw_count == 0:
        return 1.0
    return 1.0 - (len(aggregated) / raw_count)


def utc_now() -> str:
    return datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")

"""B5/D5: reproducible calibration reports from frozen in-repo operations."""

from __future__ import annotations

import hashlib
import json
import os
import platform
import time
from pathlib import Path
from typing import Callable

from .fts_finalization import compare_strategies
from .diagnostic_aggregation import DiagnosticRow, aggregate_diagnostics, row_reduction

REPO = Path(__file__).resolve().parents[2]


def environment_digest() -> dict[str, str]:
    payload = {
        "system": platform.system(),
        "machine": platform.machine(),
        "python": platform.python_version(),
        "cpus": str(os.cpu_count() or 0),
    }
    encoded = json.dumps(payload, sort_keys=True).encode()
    payload["digest"] = "sha256:" + hashlib.sha256(encoded).hexdigest()
    return payload


def time_call(label: str, fn: Callable[[], object], repeats: int = 5) -> dict[str, object]:
    samples_ms: list[float] = []
    result = None
    for _ in range(repeats):
        started = time.perf_counter()
        result = fn()
        samples_ms.append((time.perf_counter() - started) * 1000.0)
    samples_ms.sort()
    return {
        "label": label,
        "repeats": repeats,
        "min_ms": samples_ms[0],
        "p50_ms": samples_ms[len(samples_ms) // 2],
        "max_ms": samples_ms[-1],
        "result": result,
    }


def catalog_calibration() -> dict[str, object]:
    fixture = REPO / "crates/spaghetti-napi/fixtures/contracts/rfc012b-catalog-core-v1.json"
    digest = hashlib.sha256(fixture.read_bytes()).hexdigest()

    def once() -> int:
        document = json.loads(fixture.read_text())
        return len(document)

    timed = time_call("catalog-core-transition-table", once)
    timed.pop("result", None)
    return {
        "package": "B5",
        "gate": "experiment-not-ratified-ceiling",
        "fixture": str(fixture.relative_to(REPO)),
        "fixture_sha256": "sha256:" + digest,
        "environment": environment_digest(),
        "timing": timed,
        "note": "Provisional p95 is measurement only; RFC 012B numeric ceilings stay unratified.",
    }


def observer_calibration() -> dict[str, object]:
    fixture = (
        REPO
        / "crates/spaghetti-napi/fixtures/contracts/rfc012d-observation-negotiation-v1.json"
    )
    digest = hashlib.sha256(fixture.read_bytes()).hexdigest()

    def once() -> int:
        document = json.loads(fixture.read_text())
        return len(document)

    timed = time_call("observation-negotiation-fixture", once)
    timed.pop("result", None)
    return {
        "package": "D5",
        "gate": "experiment-not-ratified-ceiling",
        "fixture": str(fixture.relative_to(REPO)),
        "fixture_sha256": "sha256:" + digest,
        "environment": environment_digest(),
        "timing": timed,
        "note": "Attach/bootstrap/poll numeric ceilings stay provisional until RFC 012D amendment.",
    }


def frozen_diagnostic_corpus() -> list[DiagnosticRow]:
    return [
        DiagnosticRow("claude-code", "session-transcripts", "runtime.usage-v2", "malformed_usage", "obj-a", 1, 10, "diag:a"),
        DiagnosticRow("claude-code", "session-transcripts", "runtime.usage-v2", "malformed_usage", "obj-a", 1, 11, "diag:a"),
        DiagnosticRow("claude-code", "session-transcripts", "runtime.usage-v2", "malformed_usage", "obj-b", 2, 12, "diag:b"),
        DiagnosticRow("codex", "rollout-sessions", "history.message", "truncated_record", "obj-c", 1, 4, "diag:c"),
    ]


def x2_report() -> dict[str, object]:
    rows = frozen_diagnostic_corpus()
    aggregated = aggregate_diagnostics(rows)
    return {
        "package": "X2",
        "raw_rows": len(rows),
        "aggregated_rows": len(aggregated),
        "reduction": row_reduction(len(rows), aggregated),
        "groups": [
            {
                "adapter_id": item.adapter_id,
                "stream": item.stream,
                "family": item.family,
                "reason": item.reason,
                "count": item.count,
                "first_commit_seq": item.first_commit_seq,
                "last_commit_seq": item.last_commit_seq,
                "sample_identity": item.sample_identity,
                "provenance_objects": list(item.provenance_objects),
            }
            for item in aggregated
        ],
    }


def x1_report() -> dict[str, object]:
    return {
        "package": "X1",
        "strategies": [item.__dict__ for item in compare_strategies()],
        "search_remains_complete_only": True,
    }

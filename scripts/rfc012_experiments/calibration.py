"""B5/D5/X1/X2: reproducible reports from real in-repo operations."""

from __future__ import annotations

import hashlib
import json
import os
import platform
from pathlib import Path
from typing import Callable

from .cargo_ops import run_napi_lib_test
from .diagnostic_aggregation import aggregate_diagnostics, row_reduction, rows_from_sqlite_census
from .fts_finalization import compare_strategies, load_frozen_trace
from .sqlite_diagnostics import load_diagnostic_rows

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


def time_call(label: str, fn: Callable[[], object], repeats: int = 1) -> dict[str, object]:
    import time

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
    def once() -> str:
        completed = run_napi_lib_test("last_complete_catalog_pages_while_search_bootstrap_is_incomplete")
        output = completed.stdout
        if "test result: ok" not in output:
            raise RuntimeError("catalog retained-page test did not pass")
        return "catalog-retained-page"

    timed = time_call("catalog-retained-page", once)
    operation = timed.pop("result")
    return {
        "package": "B5",
        "gate": "experiment-not-ratified-ceiling",
        "operation": operation,
        "environment": environment_digest(),
        "timing": timed,
        "note": "Provisional p95 is measurement only; RFC 012B numeric ceilings stay unratified.",
    }


def observer_calibration() -> dict[str, object]:
    def overflow() -> str:
        completed = run_napi_lib_test("scoped_resync_required_invalidates_backlog_and_delivers_next")
        if "test result: ok" not in completed.stdout:
            raise RuntimeError("scoped resync/overflow test did not pass")
        return "scoped-resync-overflow"

    def fairness() -> str:
        completed = run_napi_lib_test("scoped_three_observers_isolate_slow_overflow_from_healthy_progress")
        if "test result: ok" not in completed.stdout:
            raise RuntimeError("multi-scope observer fairness test did not pass")
        return "multi-scope-slow-consumer"

    overflow_timed = time_call("scoped-resync-overflow", overflow)
    fairness_timed = time_call("multi-scope-slow-consumer", fairness)
    overflow_timed.pop("result")
    fairness_timed.pop("result")
    return {
        "package": "D5",
        "gate": "experiment-not-ratified-ceiling",
        "operation": "scoped-resync-overflow",
        "environment": environment_digest(),
        "timing": overflow_timed,
        "fairness_timing": fairness_timed,
        "note": "Measures overflow/resync and three-scope slow-consumer kernel paths. Numeric ceilings stay unratified.",
    }


def x2_report() -> dict[str, object]:
    fixture = REPO / "scripts/rfc012_experiments/fixtures/source-record-errors.sqlite"
    completed = run_napi_lib_test(
        "rfc012_x2_dump_engine_source_record_errors",
        extra_env={"RFC012_X2_DUMP": str(fixture)},
    )
    if "test result: ok" not in completed.stdout:
        raise RuntimeError("engine source_record_errors dump test did not pass")
    records = load_diagnostic_rows(fixture)
    if not records:
        raise RuntimeError("engine dump has no source_record_errors rows")
    rows = rows_from_sqlite_census(records)
    aggregated = aggregate_diagnostics(rows)
    return {
        "package": "X2",
        "fixture": str(fixture.relative_to(REPO)),
        "source": "rfc012_x2_dump_engine_source_record_errors",
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
    trace = REPO / "scripts/rfc012_experiments/fixtures/fts-bootstrap-trace.json"
    completed = run_napi_lib_test(
        "rfc012_x1_emit_complete_only_ingest_trace",
        extra_env={"RFC012_X1_TRACE": str(trace)},
    )
    if "test result: ok" not in completed.stdout:
        raise RuntimeError("complete-only ingest trace test did not pass")
    milestones = load_frozen_trace(trace)
    if not any(item.t_ms > 0 for item in milestones):
        raise RuntimeError("ingest trace timestamps were not emitted from a real run")
    strategies = compare_strategies(milestones)
    search_complete_only = all(not item.search_visible_before_complete for item in strategies)
    return {
        "package": "X1",
        "operation": "rfc012_x1_emit_complete_only_ingest_trace",
        "strategies": [item.__dict__ for item in strategies],
        "search_remains_complete_only": search_complete_only,
        "milestones": [item.__dict__ for item in milestones],
    }

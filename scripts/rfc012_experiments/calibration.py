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
from .sqlite_diagnostics import load_diagnostic_rows, write_diagnostic_fixture

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
    def once() -> str:
        completed = run_napi_lib_test("nested_directory_child_jsonl_decodes_through_replace_driver")
        if "test result: ok" not in completed.stdout:
            raise RuntimeError("observer directory-member test did not pass")
        return "observer-attach-poll"

    timed = time_call("observer-attach-poll", once)
    operation = timed.pop("result")
    return {
        "package": "D5",
        "gate": "experiment-not-ratified-ceiling",
        "operation": operation,
        "environment": environment_digest(),
        "timing": timed,
        "note": "Attach/bootstrap/poll numeric ceilings stay provisional until RFC 012D amendment.",
    }


def x2_report() -> dict[str, object]:
    fixture = REPO / "scripts/rfc012_experiments/fixtures/source-record-errors.sqlite"
    write_diagnostic_fixture(fixture)
    records = load_diagnostic_rows(fixture)
    rows = rows_from_sqlite_census(records)
    aggregated = aggregate_diagnostics(rows)
    return {
        "package": "X2",
        "fixture": str(fixture.relative_to(REPO)),
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
    completed = run_napi_lib_test("search_stays_unavailable_until_query_bootstrap_completes")
    if "test result: ok" not in completed.stdout:
        raise RuntimeError("complete-only search gate test did not pass")
    strategies = compare_strategies(load_frozen_trace())
    search_complete_only = all(not item.search_visible_before_complete for item in strategies)
    return {
        "package": "X1",
        "operation": "search-complete-only-gate",
        "strategies": [item.__dict__ for item in strategies],
        "search_remains_complete_only": search_complete_only,
    }

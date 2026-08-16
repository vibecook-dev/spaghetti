#!/usr/bin/env python3
"""Measure lossless aggregation opportunities in source-record diagnostics."""

from __future__ import annotations

import argparse
import hashlib
import json
import sqlite3
import time
import urllib.parse
from pathlib import Path
from typing import Any


def display_path(path: Path) -> str:
    try:
        return "~/" + str(path.resolve().relative_to(Path.home().resolve()))
    except ValueError:
        try:
            return str(path.resolve().relative_to(Path.cwd().resolve()))
        except ValueError:
            return str(path)


def open_read_only(path: Path) -> sqlite3.Connection:
    encoded = urllib.parse.quote(str(path.resolve()), safe="/")
    connection = sqlite3.connect(f"file:{encoded}?mode=ro", uri=True)
    connection.execute("PRAGMA query_only = ON")
    return connection


def scalar(connection: sqlite3.Connection, query: str) -> int:
    value = connection.execute(query).fetchone()[0]
    return int(value or 0)


def signature_hash(message: str) -> str:
    return hashlib.sha256(message.encode()).hexdigest()[:16]


def analyze_database(path: Path) -> dict[str, Any]:
    started = time.perf_counter()
    with open_read_only(path) as connection:
        total_rows, distinct_signatures, distinct_payloads, raw_payload_bytes = connection.execute(
            """
            SELECT COUNT(*),
                   COUNT(DISTINCT error_message),
                   COUNT(DISTINCT hex(payload_hash)),
                   COALESCE(SUM(length(raw_payload)), 0)
              FROM source_record_errors
            """
        ).fetchone()
        object_generation_groups = scalar(
            connection,
            """
            SELECT COUNT(*) FROM (
              SELECT source_object_id, generation, error_message
                FROM source_record_errors
               GROUP BY source_object_id, generation, error_message
            )
            """,
        )
        per_adapter = [
            {
                "adapterId": adapter,
                "occurrences": occurrences,
                "signatures": signatures,
                "objectGenerationGroups": groups,
            }
            for adapter, occurrences, signatures, groups in connection.execute(
                """
                SELECT si.adapter_id,
                       COUNT(*) AS occurrences,
                       COUNT(DISTINCT sre.error_message) AS signatures,
                       COUNT(DISTINCT
                         CAST(sre.source_object_id AS TEXT) || ':' ||
                         CAST(sre.generation AS TEXT) || ':' || sre.error_message
                       ) AS object_generation_groups
                  FROM source_record_errors sre
                  JOIN source_objects so
                    ON so.source_object_id = sre.source_object_id
                  JOIN source_streams ss
                    ON ss.source_stream_id = so.source_stream_id
                  JOIN source_instances si
                    ON si.source_instance_id = ss.source_instance_id
                 GROUP BY si.adapter_id
                 ORDER BY occurrences DESC
                """
            )
        ]
        signature_groups = [
            {
                "adapterId": adapter,
                "stream": stream,
                "errorClass": error_class,
                "signatureHash": signature_hash(message),
                "occurrences": occurrences,
            }
            for adapter, stream, error_class, message, occurrences in connection.execute(
                """
                SELECT si.adapter_id,
                       ss.stream_key,
                       sre.error_class,
                       sre.error_message,
                       COUNT(*) AS occurrences
                  FROM source_record_errors sre
                  JOIN source_objects so
                    ON so.source_object_id = sre.source_object_id
                  JOIN source_streams ss
                    ON ss.source_stream_id = so.source_stream_id
                  JOIN source_instances si
                    ON si.source_instance_id = ss.source_instance_id
                 GROUP BY si.adapter_id,
                          ss.stream_key,
                          sre.error_class,
                          sre.error_message
                 ORDER BY occurrences DESC
                 LIMIT 20
                """
            )
        ]
        allocation = {
            name: {
                "allocatedBytes": allocated,
                "payloadBytes": payload,
                "pages": pages,
            }
            for name, allocated, payload, pages in connection.execute(
                """
                SELECT name, SUM(pgsize), SUM(payload), COUNT(*)
                  FROM dbstat
                 WHERE name IN (
                   'source_record_errors',
                   'sqlite_autoindex_source_record_errors_1'
                 )
                 GROUP BY name
                """
            )
        }
        unknown_fact_rows = scalar(
            connection,
            "SELECT COUNT(*) FROM fact_records WHERE fact_kind = 'unknown_record'",
        )
        unknown_fact_codecs = [
            {
                "codec": codec,
                "rows": rows,
                "payloadBytes": payload_bytes,
            }
            for codec, rows, payload_bytes in connection.execute(
                """
                SELECT payload_codec,
                       COUNT(*),
                       COALESCE(SUM(length(payload_json)), 0)
                  FROM fact_records
                 WHERE fact_kind = 'unknown_record'
                 GROUP BY payload_codec
                """
            )
        ]

    total_rows = int(total_rows)
    global_reduction = (
        100.0 * (1.0 - int(distinct_signatures) / total_rows) if total_rows else 0.0
    )
    object_reduction = (
        100.0 * (1.0 - object_generation_groups / total_rows) if total_rows else 0.0
    )
    stat = path.stat()
    return {
        "schemaVersion": 1,
        "database": display_path(path),
        "databaseSizeBytes": stat.st_size,
        "databaseMtimeNs": stat.st_mtime_ns,
        "elapsedMs": round((time.perf_counter() - started) * 1_000, 3),
        "diagnostics": {
            "occurrences": total_rows,
            "distinctSignatures": int(distinct_signatures),
            "objectGenerationSignatureGroups": object_generation_groups,
            "distinctPayloadHashes": int(distinct_payloads),
            "rawPayloadBytes": int(raw_payload_bytes),
            "globalSignatureRowReductionPercent": round(global_reduction, 6),
            "objectGenerationRowReductionPercent": round(object_reduction, 6),
        },
        "perAdapter": per_adapter,
        "topSignatureGroups": signature_groups,
        "allocation": allocation,
        "unknownFacts": {
            "rows": unknown_fact_rows,
            "codecs": unknown_fact_codecs,
        },
        "privacy": (
            "Error messages and native payloads are not emitted. Diagnostic signatures are "
            "represented by truncated SHA-256 hashes."
        ),
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--database", required=True, type=Path)
    parser.add_argument(
        "--out",
        type=Path,
        default=Path("/private/tmp/spaghetti-diagnostic-census.json"),
    )
    args = parser.parse_args()
    database = args.database.expanduser().resolve()
    if not database.is_file():
        parser.error(f"database does not exist: {database}")
    report = analyze_database(database)
    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    diagnostics = report["diagnostics"]
    allocated = sum(item["allocatedBytes"] for item in report["allocation"].values())
    print("Diagnostic aggregation census")
    print(f"  occurrences:             {diagnostics['occurrences']:,}")
    print(f"  global signatures:       {diagnostics['distinctSignatures']:,}")
    print(
        f"  object-generation groups:{diagnostics['objectGenerationSignatureGroups']:>9,}"
    )
    print(
        f"  row reduction:           {diagnostics['objectGenerationRowReductionPercent']:.4f}% "
        "at object-generation granularity"
    )
    print(f"  diagnostic allocation:   {allocated:,} B")
    print(f"  unknown fact rows:        {report['unknownFacts']['rows']:,}")
    print(f"  elapsed:                  {report['elapsedMs']:.1f} ms")
    print(f"  wrote {args.out}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

"""Read engine-produced source_record_errors from a napi dump database."""

from __future__ import annotations

import sqlite3
from pathlib import Path

CENSUS_SQL = """
SELECT si.adapter_id,
       ss.stream_key,
       sre.error_class,
       sre.error_message,
       sre.source_object_id,
       sre.generation,
       sre.first_commit_seq,
       hex(sre.payload_hash)
  FROM source_record_errors sre
  JOIN source_objects so ON so.source_object_id = sre.source_object_id
  JOIN source_streams ss ON ss.source_stream_id = so.source_stream_id
  JOIN source_instances si ON si.source_instance_id = ss.source_instance_id
 ORDER BY si.adapter_id, ss.stream_key, sre.error_class, sre.first_commit_seq
"""


def load_diagnostic_rows(path: Path) -> list[tuple]:
    if not path.is_file():
        raise FileNotFoundError(f"engine diagnostic dump missing: {path}")
    connection = sqlite3.connect(f"file:{path}?mode=ro", uri=True)
    try:
        tables = {
            name
            for (name,) in connection.execute(
                "SELECT name FROM sqlite_master WHERE type = 'table'"
            )
        }
        if "source_record_errors" not in tables:
            raise ValueError(f"{path} is not an engine dump: missing source_record_errors")
        return list(connection.execute(CENSUS_SQL))
    finally:
        connection.close()

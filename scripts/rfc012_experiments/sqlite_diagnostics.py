"""Build and query a privacy-safe source_record_errors fixture matching engine schema."""

from __future__ import annotations

import sqlite3
from pathlib import Path


SCHEMA = """
CREATE TABLE source_instances (
  source_instance_id INTEGER PRIMARY KEY,
  adapter_id TEXT NOT NULL
);
CREATE TABLE source_streams (
  source_stream_id INTEGER PRIMARY KEY,
  source_instance_id INTEGER NOT NULL,
  stream_key TEXT NOT NULL
);
CREATE TABLE source_objects (
  source_object_id INTEGER PRIMARY KEY,
  source_stream_id INTEGER NOT NULL
);
CREATE TABLE source_record_errors (
  source_object_id INTEGER NOT NULL,
  generation INTEGER NOT NULL,
  cursor_start BLOB NOT NULL,
  cursor_end BLOB NOT NULL,
  payload_hash BLOB NOT NULL,
  media_type TEXT NOT NULL,
  raw_payload BLOB,
  error_class TEXT NOT NULL,
  error_message TEXT NOT NULL,
  adapter_version TEXT NOT NULL,
  contract_version INTEGER NOT NULL,
  first_commit_seq INTEGER NOT NULL,
  last_retry_at INTEGER,
  retry_count INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (source_object_id, generation, cursor_start, cursor_end)
);
"""

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


def write_diagnostic_fixture(path: Path) -> Path:
    path.parent.mkdir(parents=True, exist_ok=True)
    if path.exists():
        path.unlink()
    connection = sqlite3.connect(path)
    try:
        connection.executescript(SCHEMA)
        connection.execute(
            "INSERT INTO source_instances(source_instance_id, adapter_id) VALUES (1, ?), (2, ?)",
            ("claude-code", "codex"),
        )
        connection.execute(
            "INSERT INTO source_streams(source_stream_id, source_instance_id, stream_key) VALUES (1, 1, ?), (2, 2, ?)",
            ("session-transcripts", "rollout-sessions"),
        )
        connection.execute(
            "INSERT INTO source_objects(source_object_id, source_stream_id) VALUES (10, 1), (11, 1), (20, 2)"
        )
        rows = [
            (10, 1, b"\x01", b"\x02", b"\x11" * 32, "application/x-ndjson", None, "malformed_usage", "opaque:usage", "0.7.0", 1, 10, None, 0),
            (10, 1, b"\x03", b"\x04", b"\x11" * 32, "application/x-ndjson", None, "malformed_usage", "opaque:usage", "0.7.0", 1, 11, None, 1),
            (11, 2, b"\x05", b"\x06", b"\x22" * 32, "application/x-ndjson", None, "malformed_usage", "opaque:usage", "0.7.0", 1, 12, None, 0),
            (20, 1, b"\x07", b"\x08", b"\x33" * 32, "application/x-ndjson", None, "truncated_record", "opaque:trunc", "0.7.0", 1, 4, None, 0),
        ]
        connection.executemany(
            """INSERT INTO source_record_errors(
                 source_object_id, generation, cursor_start, cursor_end, payload_hash,
                 media_type, raw_payload, error_class, error_message, adapter_version,
                 contract_version, first_commit_seq, last_retry_at, retry_count
               ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)""",
            rows,
        )
        connection.commit()
    finally:
        connection.close()
    return path


def load_diagnostic_rows(path: Path) -> list[tuple]:
    connection = sqlite3.connect(path)
    try:
        return list(connection.execute(CENSUS_SQL))
    finally:
        connection.close()

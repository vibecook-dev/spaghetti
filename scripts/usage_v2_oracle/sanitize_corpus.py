#!/usr/bin/env python3
"""Build an oracle fixture from a Claude transcript corpus without native content.

The oracle in `oracle.py` accepts one bounded, self-describing fixture. This
sanitizer is the bridge that lets the same oracle run against a real corpus:
it walks `<root>/projects/**/*.jsonl`, keeps only the fields RFC 012C usage-v2
reduction reads, and replaces every native identifier with a synthetic one.

What crosses the boundary:

- `type` (only the literal `assistant` is retained; other records become a
  contentless placeholder so cursor ranges stay contiguous),
- `message.usage.{input,output,cache_creation_input,cache_read_input}_tokens`
  exactly as native evidence presents them (missing stays missing),
- `message.id` and `requestId` replaced by stable synthetic surrogates,
- `message.model` replaced by a synthetic surrogate,
- `timestamp` replaced by a synthetic monotone stamp.

No path, prompt, answer, tool payload, project name, or native identifier is
written to the output. Byte offsets are real because the framed cursor is the
response fallback identity under decoder contract 17/18.
"""

from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path
from typing import Any


SANITIZER_VERSION = 2
ORACLE_CONTRACT_VERSION = 1
BUCKETS = (
    "input_tokens",
    "output_tokens",
    "cache_creation_input_tokens",
    "cache_read_input_tokens",
)


class Surrogates:
    """Stable, collision-resistant synthetic identifiers keyed by namespace."""

    def __init__(self, salt: bytes) -> None:
        self._salt = salt
        self._assigned: dict[tuple[str, str], str] = {}
        self._counters: dict[str, int] = {}

    def of(self, namespace: str, native: str) -> str:
        key = (namespace, native)
        existing = self._assigned.get(key)
        if existing is not None:
            return existing
        index = self._counters.get(namespace, 0) + 1
        self._counters[namespace] = index
        digest = hashlib.sha256(
            self._salt + namespace.encode("utf-8") + b"\0" + native.encode("utf-8")
        ).hexdigest()[:12]
        surrogate = f"{namespace}-{index:06d}-{digest}"
        self._assigned[key] = surrogate
        return surrogate


def _optional_native_string(value: Any) -> str | None:
    return value if isinstance(value, str) and value else None


def _sanitized_usage(native_usage: Any) -> Any:
    """Keep bucket presence and value; drop every other native usage field."""

    if not isinstance(native_usage, dict):
        # Preserve malformed shape so the oracle still classifies it malformed.
        return native_usage if native_usage is None or isinstance(native_usage, (int, str, bool, list)) else {}
    kept: dict[str, Any] = {}
    for bucket in BUCKETS:
        if bucket in native_usage:
            kept[bucket] = native_usage[bucket]
    return kept


def _sanitized_record(
    record: Any, surrogates: Surrogates, record_ordinal: int
) -> dict[str, Any]:
    if not isinstance(record, dict) or record.get("type") != "assistant":
        return {"type": "other"}
    message = record.get("message")
    if not isinstance(message, dict) or "usage" not in message:
        return {"type": "other"}

    sanitized_message: dict[str, Any] = {"usage": _sanitized_usage(message.get("usage"))}
    native_message_id = _optional_native_string(message.get("id"))
    if native_message_id is not None:
        sanitized_message["id"] = surrogates.of("msg", native_message_id)
    elif "id" in message:
        # Preserve the "present but not a usable string" case verbatim in kind
        # only: the oracle treats it as absent, and so must the engine.
        sanitized_message["id"] = ""
    native_model = _optional_native_string(message.get("model"))
    if native_model is not None:
        sanitized_message["model"] = surrogates.of("model", native_model)

    sanitized: dict[str, Any] = {"type": "assistant", "message": sanitized_message}
    native_request_id = _optional_native_string(record.get("requestId"))
    if native_request_id is not None:
        sanitized["requestId"] = surrogates.of("req", native_request_id)
    if _optional_native_string(record.get("timestamp")) is not None:
        # A synthetic monotone stamp keeps ordering evidence without wall-clock
        # correlation to the operator's real activity.
        sanitized["timestamp"] = f"2000-01-01T00:00:00.{record_ordinal % 1000:03d}Z"
    return sanitized


def _transcripts(root: Path) -> list[Path]:
    projects = root / "projects"
    base = projects if projects.is_dir() else root
    return sorted(path for path in base.rglob("*.jsonl") if path.is_file())


def build_fixture(root: Path, salt: bytes) -> dict[str, Any]:
    surrogates = Surrogates(salt)
    objects: list[dict[str, Any]] = []
    for path in _transcripts(root):
        try:
            raw = path.read_bytes()
        except OSError:
            continue
        records: list[dict[str, Any]] = []
        cursor = 0
        ordinal = 0
        native_session: str | None = None
        for line in raw.splitlines(keepends=True):
            start = cursor
            cursor += len(line)
            if not line.strip():
                # An empty frame still advances the cursor; the decoder frames
                # on the delimiter, so contiguity must be preserved exactly.
                records.append({"cursorStart": start, "cursorEnd": cursor, "record": {"type": "other"}})
                continue
            try:
                decoded = json.loads(line)
            except (UnicodeDecodeError, json.JSONDecodeError):
                records.append({"cursorStart": start, "cursorEnd": cursor, "record": {"type": "other"}})
                continue
            if native_session is None and isinstance(decoded, dict):
                native_session = _optional_native_string(decoded.get("sessionId"))
            ordinal += 1
            records.append(
                {
                    "cursorStart": start,
                    "cursorEnd": cursor,
                    "record": _sanitized_record(decoded, surrogates, ordinal),
                }
            )
        if not records:
            continue
        object_id = surrogates.of("obj", str(path.relative_to(root)))
        session_id = surrogates.of("ses", native_session or str(path.relative_to(root)))
        objects.append(
            {
                "objectId": object_id,
                "sessionId": session_id,
                # One transcript file is one actor run for aggregate purposes;
                # the oracle only groups by it, and the comparison this script
                # serves is the corpus aggregate.
                "actorId": session_id,
                "role": "root",
                "generations": [{"generation": 1, "records": records}],
            }
        )
    return {
        "_fixture": {
            "fixture_kind": "sanitized-source-record-sequence",
            "format": "json",
            "sanitization": "surrogate",
            "sanitizer_version": SANITIZER_VERSION,
        },
        "scenario": {"oracleContractVersion": ORACLE_CONTRACT_VERSION, "objects": objects},
    }


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("root", type=Path, help="corpus root containing projects/**/*.jsonl")
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument(
        "--salt",
        default="usage-v2-oracle",
        help="surrogate salt; change it to make two runs non-correlatable",
    )
    args = parser.parse_args(argv)
    if not args.root.is_dir():
        parser.error(f"{args.root} is not a directory")
    fixture = build_fixture(args.root, args.salt.encode("utf-8"))
    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(
        json.dumps(fixture, ensure_ascii=False, sort_keys=True, separators=(",", ":")),
        encoding="utf-8",
    )
    objects = fixture["scenario"]["objects"]
    print(
        f"objects={len(objects)} "
        f"records={sum(len(entry['generations'][0]['records']) for entry in objects)}"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

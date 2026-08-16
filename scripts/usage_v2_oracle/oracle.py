#!/usr/bin/env python3
"""Reduce a sanitized native-record fixture with RFC 012C usage-v2 semantics.

This oracle is intentionally independent of Spaghetti's Rust adapter, SDK, and
database schema. It accepts only a bounded, self-describing fixture of framed
Claude transcript records and emits a deterministic response/actor/session
report. Omitted buckets remain qualified unknown values; they are never filled
with zero.
"""

from __future__ import annotations

import argparse
import base64
import hashlib
import json
from collections import defaultdict
from pathlib import Path
from typing import Any, Iterable


ORACLE_CONTRACT_VERSION = 1
U64_MAX = (1 << 64) - 1
BUCKETS = (
    "input_tokens",
    "output_tokens",
    "cache_creation_input_tokens",
    "cache_read_input_tokens",
)


class OracleError(ValueError):
    """The frozen fixture violates the oracle input contract."""


def _object(value: Any, label: str) -> dict[str, Any]:
    if not isinstance(value, dict):
        raise OracleError(f"{label} must be an object")
    return value


def _array(value: Any, label: str) -> list[Any]:
    if not isinstance(value, list):
        raise OracleError(f"{label} must be an array")
    return value


def _nonempty_string(value: Any, label: str) -> str:
    if not isinstance(value, str) or not value:
        raise OracleError(f"{label} must be a non-empty string")
    return value


def _u64(value: Any, label: str) -> int:
    if isinstance(value, bool) or not isinstance(value, int) or not 0 <= value <= U64_MAX:
        raise OracleError(f"{label} must be an unsigned 64-bit integer")
    return value


def _optional_native_string(value: Any) -> str | None:
    # Match serde_json::Value::as_str followed by `!value.is_empty()`. Do not
    # trim: whitespace is native evidence and changing that rule is a decoder
    # contract change.
    return value if isinstance(value, str) and value else None


def _component(value: bytes) -> bytes:
    return len(value).to_bytes(8, "big") + value


def _append_cursor(offset: int) -> bytes:
    return bytes((1, 1)) + offset.to_bytes(8, "big")


def source_record_fallback_key(cursor_start: int, cursor_end: int) -> bytes:
    """Return Claude decoder-contract-17's framed response fallback key."""

    return (
        b"source-record-v1\0"
        + _component(_append_cursor(cursor_start))
        + _component(_append_cursor(cursor_end))
    )


def _base64url(value: bytes) -> str:
    return base64.urlsafe_b64encode(value).decode("ascii").rstrip("=")


def _qualified_exact(value: Any, native_field: str) -> dict[str, Any]:
    return {
        "value": value,
        "quality": "exact",
        "completeness": "complete",
        "unknownReason": None,
        "authority": "native_response",
        "nativeField": native_field,
        "normalizationContractVersion": 1,
    }


def _qualified_missing(native_field: str) -> dict[str, Any]:
    return {
        "value": None,
        "quality": "unknown",
        "completeness": "unknown",
        "unknownReason": "missing",
        "authority": "native_response",
        "nativeField": native_field,
        "normalizationContractVersion": 1,
    }


def _usage_buckets(native_usage: dict[str, Any]) -> dict[str, dict[str, Any]] | None:
    buckets: dict[str, dict[str, Any]] = {}
    for bucket in BUCKETS:
        native_field = f"message.usage.{bucket}"
        if bucket not in native_usage:
            buckets[bucket] = _qualified_missing(native_field)
            continue
        value = native_usage[bucket]
        if isinstance(value, bool) or not isinstance(value, int) or not 0 <= value <= U64_MAX:
            return None
        buckets[bucket] = _qualified_exact(value, native_field)
    return buckets


def _same_semantic_snapshot(left: dict[str, Any], right: dict[str, Any]) -> bool:
    fields = (
        "sessionId",
        "actorId",
        "nativeMessageId",
        "requestId",
        "buckets",
        "model",
        "effort",
        "sourceTime",
    )
    return all(left[field] == right[field] for field in fields)


def _is_downward(left: dict[str, Any], right: dict[str, Any]) -> bool:
    for bucket in BUCKETS:
        previous = left["buckets"][bucket]["value"]
        current = right["buckets"][bucket]["value"]
        if previous is not None and current is not None and current < previous:
            return True
    return False


def _totals(responses: Iterable[dict[str, Any]]) -> dict[str, dict[str, Any]]:
    materialized = list(responses)
    totals: dict[str, dict[str, Any]] = {}
    for bucket in BUCKETS:
        values = [response["buckets"][bucket]["value"] for response in materialized]
        known = [value for value in values if value is not None]
        unknown_count = len(values) - len(known)
        totals[bucket] = {
            "knownValue": sum(known),
            "knownResponses": len(known),
            "unknownResponses": unknown_count,
            "completeness": "complete" if unknown_count == 0 else "partial",
        }
    return totals


def _groups(responses: list[dict[str, Any]], field: str) -> list[dict[str, Any]]:
    grouped: dict[str, list[dict[str, Any]]] = defaultdict(list)
    for response in responses:
        grouped[response[field]].append(response)
    label = "actorId" if field == "actorId" else "sessionId"
    return [
        {
            label: key,
            "responseCount": len(grouped[key]),
            "totals": _totals(grouped[key]),
        }
        for key in sorted(grouped)
    ]


def _canonical_json(value: Any) -> bytes:
    return json.dumps(
        value,
        ensure_ascii=False,
        sort_keys=True,
        separators=(",", ":"),
    ).encode("utf-8")


def analyze_document(document: Any, fixture_sha256: str) -> dict[str, Any]:
    root = _object(document, "fixture")
    metadata = _object(root.get("_fixture"), "_fixture")
    if metadata.get("sanitizer_version") != 1:
        raise OracleError("_fixture.sanitizer_version must be 1")
    scenario = _object(root.get("scenario"), "scenario")
    if scenario.get("oracleContractVersion") != ORACLE_CONTRACT_VERSION:
        raise OracleError(
            f"scenario.oracleContractVersion must be {ORACLE_CONTRACT_VERSION}"
        )
    objects = _array(scenario.get("objects"), "scenario.objects")
    if not objects:
        raise OracleError("scenario.objects must not be empty")

    state: dict[tuple[str, int, str, bytes], dict[str, Any]] = {}
    object_ids: set[str] = set()
    request_responses: dict[tuple[str, int, str], set[tuple[str, bytes]]] = defaultdict(set)
    metrics = {
        "records": 0,
        "usageCandidates": 0,
        "acceptedRevisions": 0,
        "malformedSnapshots": 0,
        "newResponses": 0,
        "replacementRevisions": 0,
        "exactRepeatRevisions": 0,
        "changedRevisions": 0,
        "downwardRevisions": 0,
        "generationResets": 0,
        "responsesRetractedByReset": 0,
        "rowsWithoutMessageId": 0,
        "rowsWithoutRequestId": 0,
    }
    generation_count = 0

    for object_index, raw_object in enumerate(objects):
        native_object = _object(raw_object, f"scenario.objects[{object_index}]")
        object_id = _nonempty_string(
            native_object.get("objectId"), f"scenario.objects[{object_index}].objectId"
        )
        if object_id in object_ids:
            raise OracleError(f"duplicate objectId: {object_id}")
        object_ids.add(object_id)
        session_id = _nonempty_string(
            native_object.get("sessionId"), f"object {object_id}.sessionId"
        )
        actor_id = _nonempty_string(
            native_object.get("actorId"), f"object {object_id}.actorId"
        )
        role = native_object.get("role")
        if role not in ("root", "child"):
            raise OracleError(f"object {object_id}.role must be root or child")
        generations = _array(native_object.get("generations"), f"object {object_id}.generations")
        if not generations:
            raise OracleError(f"object {object_id}.generations must not be empty")

        prior_generation: int | None = None
        for generation_index, raw_generation in enumerate(generations):
            generation_doc = _object(
                raw_generation, f"object {object_id}.generations[{generation_index}]"
            )
            generation = _u64(
                generation_doc.get("generation"),
                f"object {object_id}.generations[{generation_index}].generation",
            )
            if generation == 0:
                raise OracleError(f"object {object_id} generation must be greater than zero")
            if prior_generation is not None and generation <= prior_generation:
                raise OracleError(f"object {object_id} generations must strictly increase")
            if prior_generation is not None:
                metrics["generationResets"] += 1
                stale = [key for key in state if key[0] == object_id]
                metrics["responsesRetractedByReset"] += len(stale)
                for key in stale:
                    del state[key]
            prior_generation = generation
            generation_count += 1

            records = _array(
                generation_doc.get("records"),
                f"object {object_id} generation {generation}.records",
            )
            cursor = 0
            for record_index, raw_entry in enumerate(records):
                entry = _object(
                    raw_entry,
                    f"object {object_id} generation {generation} record {record_index}",
                )
                cursor_start = _u64(entry.get("cursorStart"), "cursorStart")
                cursor_end = _u64(entry.get("cursorEnd"), "cursorEnd")
                if cursor_start != cursor or cursor_end <= cursor_start:
                    raise OracleError(
                        f"object {object_id} generation {generation} has a non-contiguous cursor range"
                    )
                cursor = cursor_end
                record = _object(entry.get("record"), "record")
                metrics["records"] += 1
                if record.get("type") != "assistant":
                    continue
                message = record.get("message")
                if not isinstance(message, dict) or "usage" not in message:
                    continue
                metrics["usageCandidates"] += 1
                native_message_id = _optional_native_string(message.get("id"))
                request_id = _optional_native_string(record.get("requestId"))
                metrics["rowsWithoutMessageId"] += int(native_message_id is None)
                metrics["rowsWithoutRequestId"] += int(request_id is None)
                native_usage = message.get("usage")
                if not isinstance(native_usage, dict):
                    metrics["malformedSnapshots"] += 1
                    continue
                buckets = _usage_buckets(native_usage)
                if buckets is None:
                    metrics["malformedSnapshots"] += 1
                    continue

                if native_message_id is None:
                    response_identity = "source_record_fallback"
                    response_key = source_record_fallback_key(cursor_start, cursor_end)
                else:
                    response_identity = "native_message_id"
                    response_key = native_message_id.encode("utf-8")
                state_key = (object_id, generation, response_identity, response_key)
                model = _optional_native_string(message.get("model"))
                source_time = _optional_native_string(record.get("timestamp"))
                response = {
                    "objectId": object_id,
                    "generation": generation,
                    "sessionId": session_id,
                    "actorId": actor_id,
                    "role": role,
                    "responseIdentity": response_identity,
                    "responseKeyBase64Url": _base64url(response_key),
                    "nativeMessageId": native_message_id,
                    "requestId": request_id,
                    "cursorStart": cursor_start,
                    "cursorEnd": cursor_end,
                    "buckets": buckets,
                    "model": (
                        _qualified_exact(model, "message.model") if model is not None else None
                    ),
                    "effort": None,
                    "sourceTime": (
                        {"value": source_time, "quality": "native_exact"}
                        if source_time is not None
                        else None
                    ),
                }
                prior = state.get(state_key)
                if prior is None:
                    metrics["newResponses"] += 1
                else:
                    metrics["replacementRevisions"] += 1
                    if _same_semantic_snapshot(prior, response):
                        metrics["exactRepeatRevisions"] += 1
                    else:
                        metrics["changedRevisions"] += 1
                    metrics["downwardRevisions"] += int(_is_downward(prior, response))
                metrics["acceptedRevisions"] += 1
                state[state_key] = response
                if request_id is not None:
                    request_responses[(object_id, generation, request_id)].add(
                        (response_identity, response_key)
                    )

    responses = sorted(
        state.values(),
        key=lambda response: (
            response["objectId"],
            response["generation"],
            response["responseIdentity"],
            response["responseKeyBase64Url"],
        ),
    )
    metrics["requestIdsMappingToMultipleResponses"] = sum(
        len(response_keys) > 1 for response_keys in request_responses.values()
    )
    detailed_state = {
        "responseCount": len(responses),
        "responses": responses,
        "aggregate": _totals(responses),
        "actors": _groups(responses, "actorId"),
        "sessions": _groups(responses, "sessionId"),
    }
    state_sha256 = hashlib.sha256(_canonical_json(detailed_state)).hexdigest()
    final_state = {
        key: value for key, value in detailed_state.items() if key != "responses"
    }
    return {
        "schemaVersion": 1,
        "oracleContractVersion": ORACLE_CONTRACT_VERSION,
        "fixtureSha256": f"sha256:{fixture_sha256}",
        "input": {
            "objects": len(objects),
            "generations": generation_count,
            "records": metrics["records"],
        },
        "observations": metrics,
        "finalState": final_state,
        "stateSha256": f"sha256:{state_sha256}",
        "privacy": "Synthetic sanitized identifiers and qualified numeric usage only; no native path, prompt, answer, or raw payload is emitted.",
    }


def analyze_fixture(path: Path) -> dict[str, Any]:
    fixture_bytes = path.read_bytes()
    try:
        document = json.loads(fixture_bytes)
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise OracleError(f"cannot parse fixture {path}: {error}") from error
    return analyze_document(document, hashlib.sha256(fixture_bytes).hexdigest())


def _render(report: dict[str, Any]) -> str:
    return json.dumps(report, ensure_ascii=False, indent=2, sort_keys=True) + "\n"


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("fixture", type=Path)
    parser.add_argument("--out", type=Path)
    parser.add_argument("--check", type=Path)
    args = parser.parse_args(argv)
    try:
        report = analyze_fixture(args.fixture)
    except (OSError, OracleError) as error:
        parser.error(str(error))
    rendered = _render(report)
    if args.check is not None:
        try:
            expected = args.check.read_text(encoding="utf-8")
        except OSError as error:
            parser.error(str(error))
        if expected != rendered:
            print(f"usage-v2 oracle report drift: {args.check}")
            return 1
    if args.out is not None:
        args.out.parent.mkdir(parents=True, exist_ok=True)
        args.out.write_text(rendered, encoding="utf-8")
    elif args.check is None:
        print(rendered, end="")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

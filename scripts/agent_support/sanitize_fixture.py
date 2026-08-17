#!/usr/bin/env python3
"""Deterministically sanitize native JSON/JSONL captures for RFC 012A fixtures.

Raw captures stay outside the repository. This tool preserves JSON shape,
referential equality between repeated identifiers, numeric counters, booleans,
and structural discriminants while replacing identity, path, timestamp, text,
and secret values. It deliberately emits ordinals rather than hashes of raw
values so committed fixtures do not expose reversible low-entropy digests.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import sys
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Iterable


SANITIZER_VERSION = 2

# Numeric placeholders retain JSON number shape for native readers while living
# in reserved ranges that ordinary process IDs and wall-clock timestamps cannot
# occupy. Keep both ranges within JavaScript's exact-integer and Date bounds.
NUMERIC_IDENTIFIER_PLACEHOLDER_BASE = 4_294_000_000
NUMERIC_TIMESTAMP_PLACEHOLDER_BASE = 8_000_000_000_000_000
MAX_NUMERIC_PLACEHOLDER_ORDINAL = 100_000

_SECRET_KEY_PARTS = (
    "apikey",
    "api_key",
    "authorization",
    "cookie",
    "credential",
    "password",
    "privatekey",
    "private_key",
    "secret",
)
_PATH_KEYS = {
    "cwd",
    "directory",
    "filepath",
    "file_path",
    "homedir",
    "home_dir",
    "messagingsocketpath",
    "path",
    "projectpath",
    "root",
    "socketpath",
    "transcriptpath",
}
_TEXT_KEYS = {
    "aititle",
    "answer",
    "command",
    "content",
    "description",
    "displayname",
    "entrypoint",
    "error",
    "label",
    "message",
    "name",
    "option",
    "preview",
    "prompt",
    "question",
    "reason",
    "script",
    "summary",
    "text",
    "title",
    "username",
}
_SAFE_STRUCTURAL_KEYS = {
    "availability",
    "event",
    "eventtype",
    "family",
    "fixture_kind",
    "format",
    "kind",
    "level",
    "media_type",
    "mode",
    "model",
    "namesource",
    "permissionmode",
    "phase",
    "role",
    "sanitization",
    "source",
    "state",
    "status",
    "subtype",
    "type",
    "version",
}
_TIMESTAMP_PARTS = ("timestamp", "createdat", "updatedat", "startedat", "endedat", "namesince")
_ID_PARTS = (
    "account",
    "agentid",
    "bridge",
    "interactionid",
    "messageid",
    "organization",
    "parentuuid",
    "pid",
    "requestid",
    "responseid",
    "runid",
    "sessionid",
    "taskid",
    "teamid",
    "tooluseid",
    "uuid",
    "workflowid",
)

_PROHIBITED_PATTERNS: tuple[tuple[str, re.Pattern[str]], ...] = (
    ("macOS home path", re.compile(r"/Users/[^/\s]+")),
    ("Unix home path", re.compile(r"/home/[^/\s]+")),
    ("Windows home path", re.compile(r"[A-Za-z]:\\Users\\[^\\\s]+", re.IGNORECASE)),
    ("email address", re.compile(r"\b[A-Z0-9._%+-]+@[A-Z0-9.-]+\.[A-Z]{2,}\b", re.IGNORECASE)),
    ("UUID", re.compile(r"\b[0-9a-f]{8}-[0-9a-f]{4}-[1-5][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}\b", re.IGNORECASE)),
    ("API token", re.compile(r"\b(?:sk-[A-Za-z0-9_-]{12,}|gh[pousr]_[A-Za-z0-9]{12,}|xox[baprs]-[A-Za-z0-9-]{10,})\b")),
    ("bearer credential", re.compile(r"\bBearer\s+[A-Za-z0-9._~+/-]{8,}", re.IGNORECASE)),
    ("private key", re.compile(r"-----BEGIN (?:RSA |EC |OPENSSH )?PRIVATE KEY-----")),
)


def _normalized_key(key: str | None) -> str:
    return "" if key is None else re.sub(r"[^a-z0-9_]", "", key.lower())


def _category(key: str | None, value: Any) -> str:
    normalized = _normalized_key(key)
    if any(part in normalized for part in _SECRET_KEY_PARTS) or normalized == "token" or normalized.endswith("token"):
        return "secret"
    if normalized in _PATH_KEYS or normalized.endswith("path") or normalized.endswith("dir"):
        return "path"
    if any(part in normalized for part in _TIMESTAMP_PARTS):
        return "timestamp"
    if normalized == "id" or normalized.endswith("id") or any(part in normalized for part in _ID_PARTS):
        return "identifier"
    if normalized in _TEXT_KEYS or normalized.endswith("text") or normalized.endswith("content"):
        return "text"
    if normalized in _SAFE_STRUCTURAL_KEYS:
        return "structural"
    if isinstance(value, str):
        return "text"
    return "ordinary"


def _canonical_scalar(value: Any) -> str:
    return json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(",", ":"))


def _walk_scalars(value: Any, key: str | None = None) -> Iterable[tuple[str, Any]]:
    if isinstance(value, dict):
        for child_key in sorted(value):
            yield from _walk_scalars(value[child_key], child_key)
    elif isinstance(value, list):
        for child in value:
            yield from _walk_scalars(child, key)
    elif value is not None and not isinstance(value, bool):
        yield _category(key, value), value


def _ordinal_maps(value: Any) -> dict[str, dict[str, int]]:
    categories = {"identifier", "path", "timestamp", "text"}
    collected: dict[str, set[str]] = {category: set() for category in categories}
    for category, scalar in _walk_scalars(value):
        if category in collected:
            collected[category].add(_canonical_scalar(scalar))

    result: dict[str, dict[str, int]] = {}
    for category, values in collected.items():
        # Sort by a private-in-process digest so output ordering reveals neither
        # lexical native values nor a committed digest of any native value.
        ordered = sorted(
            values,
            key=lambda item: hashlib.sha256(f"rfc012a:{category}:{item}".encode()).digest(),
        )
        result[category] = {item: index + 1 for index, item in enumerate(ordered)}
    return result


def _sanitize_scalar(category: str, value: Any, ordinals: dict[str, dict[str, int]]) -> Any:
    if category == "secret":
        return "[REDACTED]"
    if category not in ordinals:
        return value

    ordinal = ordinals[category][_canonical_scalar(value)]
    if ordinal > MAX_NUMERIC_PLACEHOLDER_ORDINAL:
        raise ValueError(f"too many distinct {category} values for the sanitizer contract")
    if category == "identifier":
        return (
            NUMERIC_IDENTIFIER_PLACEHOLDER_BASE + ordinal
            if isinstance(value, (int, float))
            else f"fixture-id-{ordinal:03d}"
        )
    if category == "path":
        return f"fixture://path/{ordinal:03d}"
    if category == "timestamp":
        if isinstance(value, (int, float)):
            return NUMERIC_TIMESTAMP_PLACEHOLDER_BASE + ordinal * 1_000
        return f"2000-01-01T00:00:00.{ordinal:03d}Z"
    if category == "text":
        return f"[fixture-text-{ordinal:03d}]"
    return value


def sanitize_value(value: Any) -> Any:
    """Return a deterministic, shape-preserving sanitized JSON value."""

    ordinals = _ordinal_maps(value)

    def visit(current: Any, key: str | None = None) -> Any:
        if isinstance(current, dict):
            return {child_key: visit(current[child_key], child_key) for child_key in sorted(current)}
        if isinstance(current, list):
            return [visit(child, key) for child in current]
        if current is None or isinstance(current, bool):
            return current
        return _sanitize_scalar(_category(key, current), current, ordinals)

    return visit(value)


def sanitize_document(text: str, format_hint: str | None = None) -> dict[str, Any]:
    """Parse JSON or JSONL and return a self-describing sanitized fixture."""

    selected_format = format_hint
    if selected_format is None:
        try:
            parsed = json.loads(text)
            selected_format = "json"
        except json.JSONDecodeError:
            selected_format = "jsonl"
    if selected_format == "json":
        parsed = json.loads(text)
        payload_key = "data"
    elif selected_format == "jsonl":
        parsed = [json.loads(line) for line in text.splitlines() if line.strip()]
        payload_key = "records"
    else:
        raise ValueError(f"unsupported input format: {selected_format}")

    return {
        "_fixture": {
            "fixture_kind": "sanitized-native-shape",
            "format": selected_format,
            "sanitization": "deterministic",
            "sanitizer_version": SANITIZER_VERSION,
        },
        payload_key: sanitize_value(parsed),
    }


@dataclass(frozen=True)
class ScanFinding:
    path: str
    reason: str

    def __str__(self) -> str:
        return f"{self.path}: {self.reason}"


def scan_prohibited(value: Any) -> list[ScanFinding]:
    """Find raw/sensitive fixture values and malformed sanitizer placeholders."""

    findings: list[ScanFinding] = []

    def add(path: str, reason: str) -> None:
        findings.append(ScanFinding(path, reason))

    def visit(current: Any, path: str, key: str | None = None) -> None:
        if isinstance(current, dict):
            for child_key, child in current.items():
                visit(child, f"{path}.{child_key}", child_key)
            return
        if isinstance(current, list):
            for index, child in enumerate(current):
                visit(child, f"{path}[{index}]", key)
            return
        if current is None or isinstance(current, bool):
            return

        category = _category(key, current)
        if not isinstance(current, str):
            if category == "secret":
                add(path, "secret field is not redacted")
            elif category == "identifier":
                valid_identifier = isinstance(current, int) and (
                    1
                    <= current - NUMERIC_IDENTIFIER_PLACEHOLDER_BASE
                    <= MAX_NUMERIC_PLACEHOLDER_ORDINAL
                )
                if not valid_identifier:
                    add(path, "numeric identifier field is not a sanitizer placeholder")
            elif category == "timestamp":
                delta = (
                    current - NUMERIC_TIMESTAMP_PLACEHOLDER_BASE
                    if isinstance(current, int)
                    else 0
                )
                valid_timestamp = (
                    isinstance(current, int)
                    and delta % 1_000 == 0
                    and 1 <= delta // 1_000 <= MAX_NUMERIC_PLACEHOLDER_ORDINAL
                )
                if not valid_timestamp:
                    add(path, "numeric timestamp field is not a sanitizer placeholder")
            return

        for label, pattern in _PROHIBITED_PATTERNS:
            if pattern.search(current):
                add(path, f"contains prohibited {label}")

        if category == "secret" and current != "[REDACTED]":
            add(path, "secret field is not redacted")
        elif category == "identifier" and not re.fullmatch(r"fixture-id-[0-9]{3,}", current):
            add(path, "identifier field is not a sanitizer placeholder")
        elif category == "path" and not re.fullmatch(r"fixture://path/[0-9]{3,}", current):
            add(path, "path field is not a sanitizer placeholder")
        elif category == "timestamp" and not re.fullmatch(
            r"2000-01-01T00:00:00\.[0-9]{3,}Z", current
        ):
            add(path, "timestamp field is not a sanitizer placeholder")
        elif category == "text" and not re.fullmatch(r"\[fixture-text-[0-9]{3,}\]", current):
            add(path, "free-text field is not a sanitizer placeholder")

    visit(value, "$")
    return findings


def scan_fixture_file(path: Path) -> list[ScanFinding]:
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        return [ScanFinding("$", f"cannot parse fixture {path}: {error}")]
    return scan_prohibited(value)


def _write_json(path: Path, value: Any) -> None:
    path.write_text(json.dumps(value, ensure_ascii=False, indent=2, sort_keys=True) + "\n", encoding="utf-8")


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("input", type=Path, nargs="?", help="private JSON or JSONL capture")
    parser.add_argument("output", type=Path, nargs="?", help="sanitized fixture destination")
    parser.add_argument("--format", choices=("json", "jsonl"), dest="format_hint")
    parser.add_argument("--check", nargs="+", type=Path, help="scan existing committed fixtures")
    args = parser.parse_args(argv)

    if args.check:
        failed = False
        for fixture in args.check:
            findings = scan_fixture_file(fixture)
            if findings:
                failed = True
                for finding in findings:
                    print(f"{fixture}: {finding}", file=sys.stderr)
        return 1 if failed else 0

    if args.input is None or args.output is None:
        parser.error("input and output are required unless --check is used")
    sanitized = sanitize_document(args.input.read_text(encoding="utf-8"), args.format_hint)
    findings = scan_prohibited(sanitized)
    if findings:
        for finding in findings:
            print(finding, file=sys.stderr)
        return 1
    _write_json(args.output, sanitized)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

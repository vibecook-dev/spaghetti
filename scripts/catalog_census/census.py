#!/usr/bin/env python3
"""Measure whether a display-ready agent catalog can be built with bounded I/O.

This is an experiment, not a production parser. It deliberately reads native
agent data without going through Spaghetti adapters, then optionally compares
the resulting project/session identity sets with a read-only Spaghetti SQLite
database. Output is privacy-reduced by default: native identities, paths, titles,
and prompts are reduced to aggregate counts and SHA-256 digests.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import platform
import re
import resource
import sqlite3
import sys
import time
import urllib.parse
from collections import Counter
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Iterable


CLAUDE_ID = "claude-code"
CODEX_ID = "codex"
GROK_ID = "grok"
ADAPTER_IDS = (CLAUDE_ID, CODEX_ID, GROK_ID)
UUID_RE = re.compile(
    r"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$",
    re.IGNORECASE,
)
GROK_KNOWN_FILES = {
    "chat_history.jsonl",
    "summary.json",
    "events.jsonl",
    "signals.json",
    "updates.jsonl",
}


def utc_timestamp() -> str:
    from datetime import datetime, timezone

    return datetime.now(timezone.utc).isoformat()


def encode_project_key(cwd: str) -> str:
    return cwd.replace("/", "-").replace("\\", "-")


def nonempty(value: Any) -> str | None:
    if not isinstance(value, str):
        return None
    value = value.strip()
    return value or None


def scalar_text(value: Any) -> str | None:
    if value is None:
        return None
    if isinstance(value, (str, int, float)):
        value = str(value).strip()
        return value or None
    return None


def readable_text(value: Any) -> str | None:
    """Extract bounded display text from common native content shapes."""

    parts: list[str] = []

    def visit(item: Any, depth: int = 0) -> None:
        if depth > 5 or sum(len(part) for part in parts) >= 4_096:
            return
        if isinstance(item, str):
            text = item.strip()
            if text:
                parts.append(text)
            return
        if isinstance(item, list):
            for child in item:
                visit(child, depth + 1)
            return
        if not isinstance(item, dict):
            return
        for key in ("text", "input_text", "output_text"):
            text = item.get(key)
            if isinstance(text, str) and text.strip():
                parts.append(text.strip())
        if not parts:
            for key in ("content", "message", "summary"):
                if key in item:
                    visit(item[key], depth + 1)

    visit(value)
    if not parts:
        return None
    return "\n".join(parts)[:4_096].strip() or None


def is_injected_user_text(text: str) -> bool:
    text = text.lstrip()
    if not text:
        return True
    prefixes = (
        "<environment_context>",
        "<recommended_plugins>",
        "<permissions instructions>",
        "<collaboration_mode>",
        "<skills_instructions>",
        "<apps_instructions>",
        "<plugins_instructions>",
        "<multi_agent_mode>",
        "<INSTRUCTIONS>",
        "# AGENTS.md instructions",
        "The following is the Codex agent history whose request action you are assessing.",
        "The following is the Codex agent history added since your last approval assessment.",
    )
    if text.startswith(prefixes):
        return True
    return text.startswith("<") and "<cwd>" in text and (
        "</cwd>" in text or "<shell>" in text
    )


def truncate_utf16(value: str, code_units: int = 200) -> str:
    encoded = value.encode("utf-16-le")[: code_units * 2]
    return encoded.decode("utf-16-le", errors="ignore")


def claude_human_prompt(row: dict[str, Any]) -> str | None:
    if row.get("type") != "user" or any(
        row.get(flag) is True
        for flag in ("isMeta", "isSidechain", "isCompactSummary", "isVisibleInTranscriptOnly")
    ):
        return None
    message = row.get("message")
    if not isinstance(message, dict):
        return None
    content = message.get("content")
    text: str | None = None
    if isinstance(content, str):
        text = nonempty(content)
    elif isinstance(content, list):
        for block in content:
            if isinstance(block, dict) and block.get("type") == "text":
                text = nonempty(block.get("text"))
                if text:
                    break
    if not text:
        return None
    synthetic_prefixes = (
        "<local-command-caveat>",
        "<local-command-stdout>",
        "<command-name>",
        "<command-message>",
        "<task-notification>",
        "<system-reminder>",
        "<ide_opened_file>",
        "<ide_selection>",
    )
    if text.lstrip().lower().startswith(synthetic_prefixes):
        return None
    return truncate_utf16(text)


def identity_digest(values: Iterable[tuple[str, ...]]) -> str:
    digest = hashlib.sha256()
    for value in sorted(values):
        encoded = json.dumps(value, ensure_ascii=False, separators=(",", ":")).encode()
        digest.update(len(encoded).to_bytes(8, "big"))
        digest.update(encoded)
    return digest.hexdigest()


def hashed_sample(values: Iterable[tuple[str, ...]], limit: int = 5) -> list[str]:
    return [
        hashlib.sha256(
            json.dumps(value, ensure_ascii=False, separators=(",", ":")).encode()
        ).hexdigest()[:16]
        for value in sorted(values)[:limit]
    ]


def display_path(path: Path) -> str:
    try:
        return "~/" + str(path.resolve().relative_to(Path.home().resolve()))
    except ValueError:
        try:
            return str(path.resolve().relative_to(Path.cwd().resolve()))
        except ValueError:
            return str(path)


@dataclass
class SessionRecord:
    project_key: str
    session_id: str
    cwd: str | None = None
    first_prompt: str | None = None
    title: str | None = None
    created_at: str | None = None
    modified_at: str | None = None
    message_count: int | None = None
    evidence: set[str] = field(default_factory=set)

    def merge(self, other: "SessionRecord") -> None:
        for name in (
            "cwd",
            "first_prompt",
            "title",
            "created_at",
            "modified_at",
            "message_count",
        ):
            if getattr(self, name) is None and getattr(other, name) is not None:
                setattr(self, name, getattr(other, name))
        self.evidence.update(other.evidence)


@dataclass
class Catalog:
    adapter_id: str
    projects: set[str] = field(default_factory=set)
    project_cwds: dict[str, str] = field(default_factory=dict)
    sessions: dict[tuple[str, str], SessionRecord] = field(default_factory=dict)

    def add_project(self, project_key: str, cwd: str | None = None) -> None:
        if not project_key:
            return
        self.projects.add(project_key)
        if cwd and project_key not in self.project_cwds:
            self.project_cwds[project_key] = cwd

    def add_session(self, record: SessionRecord) -> None:
        if not record.project_key or not record.session_id:
            return
        self.add_project(record.project_key, record.cwd)
        key = (record.project_key, record.session_id)
        current = self.sessions.get(key)
        if current is None:
            self.sessions[key] = record
        else:
            current.merge(record)

    def propagate_project_cwds(self) -> None:
        for record in self.sessions.values():
            if record.cwd is None:
                record.cwd = self.project_cwds.get(record.project_key)

    def project_identities(self) -> set[tuple[str, str]]:
        return {(self.adapter_id, project) for project in self.projects}

    def session_identities(self) -> set[tuple[str, str, str]]:
        return {
            (self.adapter_id, project, session)
            for project, session in self.sessions
        }


@dataclass
class ScanMetrics:
    entries_visited: int = 0
    files_opened: int = 0
    bytes_read: int = 0
    primary_files: int = 0
    primary_bytes: int = 0
    metadata_files: int = 0
    metadata_bytes: int = 0
    errors: Counter[str] = field(default_factory=Counter)
    evidence: Counter[str] = field(default_factory=Counter)
    _primary_paths: set[Path] = field(default_factory=set, repr=False)
    _metadata_paths: set[Path] = field(default_factory=set, repr=False)

    def scan_entries(self, directory: Path) -> list[os.DirEntry[str]]:
        try:
            with os.scandir(directory) as iterator:
                entries = list(iterator)
            self.entries_visited += len(entries)
            return sorted(entries, key=lambda entry: entry.name)
        except OSError:
            self.errors["directory_read"] += 1
            return []

    def register(self, path: Path, *, primary: bool = False, metadata: bool = False) -> int:
        try:
            size = path.stat().st_size
        except OSError:
            self.errors["file_stat"] += 1
            return 0
        if primary and path not in self._primary_paths:
            self._primary_paths.add(path)
            self.primary_files += 1
            self.primary_bytes += size
        if metadata and path not in self._metadata_paths:
            self._metadata_paths.add(path)
            self.metadata_files += 1
            self.metadata_bytes += size
        return size

    def read_prefix(self, path: Path, limit: int) -> tuple[bytes, bool]:
        try:
            size = path.stat().st_size
            with open(path, "rb") as source:
                data = source.read(limit)
            self.files_opened += 1
            self.bytes_read += len(data)
            return data, size > len(data)
        except OSError:
            self.errors["file_read"] += 1
            return b"", False

    def read_json_document(self, path: Path, limit: int) -> dict[str, Any] | None:
        data, truncated = self.read_prefix(path, limit)
        if truncated:
            self.errors["document_over_limit"] += 1
            return None
        try:
            value = json.loads(data)
        except (UnicodeDecodeError, json.JSONDecodeError):
            self.errors["document_json"] += 1
            return None
        if not isinstance(value, dict):
            self.errors["document_not_object"] += 1
            return None
        return value

    def read_jsonl_prefix(self, path: Path, limit: int) -> list[dict[str, Any]]:
        data, truncated = self.read_prefix(path, limit)
        if truncated and not data.endswith(b"\n"):
            boundary = data.rfind(b"\n")
            data = data[: boundary + 1] if boundary >= 0 else b""
        rows: list[dict[str, Any]] = []
        for raw in data.splitlines():
            if not raw.strip():
                continue
            try:
                value = json.loads(raw)
            except (UnicodeDecodeError, json.JSONDecodeError):
                self.errors["jsonl_record"] += 1
                continue
            if isinstance(value, dict):
                rows.append(value)
            else:
                self.errors["jsonl_not_object"] += 1
        return rows


@dataclass
class AdapterScan:
    catalog: Catalog
    metrics: ScanMetrics
    elapsed_ms: float
    root: Path


def claude_head_record(
    project_key: str,
    session_id: str,
    rows: list[dict[str, Any]],
) -> SessionRecord:
    record = SessionRecord(
        project_key=project_key,
        session_id=session_id,
        evidence={"transcript-head"},
    )
    for row in rows:
        record.cwd = record.cwd or nonempty(row.get("cwd"))
        record.created_at = record.created_at or scalar_text(row.get("timestamp"))
        kind = row.get("type")
        if kind == "ai-title" and record.title is None:
            record.title = nonempty(row.get("aiTitle"))
        elif kind == "custom-title" and record.title is None:
            record.title = nonempty(row.get("customTitle"))
        if record.first_prompt is None:
            record.first_prompt = claude_human_prompt(row)
    return record


def find_claude_subagent_sessions(
    project_dir: Path,
    direct_entries: list[os.DirEntry[str]],
    metrics: ScanMetrics,
) -> set[str]:
    """Enumerate parent sessions proven by nested subagent transcript membership."""

    sessions: set[str] = set()
    for entry in direct_entries:
        try:
            if not entry.is_dir(follow_symlinks=False):
                continue
        except OSError:
            metrics.errors["entry_type"] += 1
            continue
        session_id = entry.name
        subagents = project_dir / session_id / "subagents"
        if not subagents.is_dir():
            continue
        stack = [subagents]
        found = False
        while stack:
            directory = stack.pop()
            for child in metrics.scan_entries(directory):
                try:
                    if child.is_dir(follow_symlinks=False):
                        stack.append(Path(child.path))
                    elif (
                        child.is_file(follow_symlinks=False)
                        and child.name.startswith("agent-")
                        and child.name.endswith(".jsonl")
                    ):
                        metrics.register(Path(child.path), primary=True)
                        found = True
                except OSError:
                    metrics.errors["entry_type"] += 1
        if found:
            sessions.add(session_id)
            metrics.evidence["subagent-membership"] += 1
    return sessions


def scan_claude(root: Path, *, head_bytes: int, document_bytes: int) -> AdapterScan:
    start = time.perf_counter()
    metrics = ScanMetrics()
    catalog = Catalog(CLAUDE_ID)
    projects = root / "projects"

    for project_entry in metrics.scan_entries(projects):
        try:
            is_dir = project_entry.is_dir(follow_symlinks=False)
        except OSError:
            metrics.errors["entry_type"] += 1
            continue
        if not is_dir:
            continue
        project_key = project_entry.name
        project_dir = Path(project_entry.path)
        entries = metrics.scan_entries(project_dir)
        subagent_sessions = find_claude_subagent_sessions(
            project_dir, entries, metrics
        )
        session_paths: list[Path] = []
        index_path: Path | None = None
        has_memory = False
        for entry in entries:
            try:
                if entry.is_file(follow_symlinks=False):
                    if entry.name == "sessions-index.json":
                        index_path = Path(entry.path)
                    elif entry.name.endswith(".jsonl") and UUID_RE.fullmatch(entry.name[:-6]):
                        path = Path(entry.path)
                        metrics.register(path, primary=True)
                        session_paths.append(path)
                elif entry.name == "memory" and entry.is_dir(follow_symlinks=False):
                    memory_path = Path(entry.path) / "MEMORY.md"
                    has_memory = memory_path.is_file()
            except OSError:
                metrics.errors["entry_type"] += 1
        if session_paths or subagent_sessions or index_path is not None or has_memory:
            catalog.add_project(project_key)

        if index_path is not None:
            metrics.register(index_path, metadata=True)
            document = metrics.read_json_document(index_path, document_bytes)
            if document is not None:
                catalog.add_project(project_key, nonempty(document.get("originalPath")))
                entries_value = document.get("entries")
                if isinstance(entries_value, list):
                    for item in entries_value:
                        if not isinstance(item, dict):
                            metrics.errors["claude_index_entry"] += 1
                            continue
                        session_id = nonempty(item.get("sessionId"))
                        if session_id is None:
                            metrics.errors["claude_index_identity"] += 1
                            continue
                        count = item.get("messageCount")
                        catalog.add_session(
                            SessionRecord(
                                project_key=project_key,
                                session_id=session_id,
                                cwd=nonempty(item.get("projectPath")),
                                first_prompt=nonempty(item.get("firstPrompt")),
                                title=nonempty(item.get("summary")),
                                created_at=scalar_text(item.get("created")),
                                modified_at=scalar_text(item.get("modified"))
                                or scalar_text(item.get("fileMtime")),
                                message_count=count if isinstance(count, int) else None,
                                evidence={"session-index"},
                            )
                        )
                        metrics.evidence["session-index"] += 1
                else:
                    metrics.errors["claude_index_entries"] += 1

        for path in session_paths:
            session_id = path.stem
            current = catalog.sessions.get((project_key, session_id))
            needs_head = current is None or not (
                current.cwd and (current.first_prompt or current.title) and current.created_at
            )
            if needs_head:
                head = claude_head_record(
                    project_key,
                    session_id,
                    metrics.read_jsonl_prefix(path, head_bytes),
                )
            else:
                head = SessionRecord(project_key, session_id, evidence={"transcript-path"})
            try:
                head.modified_at = head.modified_at or str(path.stat().st_mtime_ns)
            except OSError:
                metrics.errors["file_stat"] += 1
            catalog.add_session(head)
            metrics.evidence["transcript-path"] += 1

        for session_id in subagent_sessions:
            catalog.add_session(
                SessionRecord(
                    project_key=project_key,
                    session_id=session_id,
                    evidence={"subagent-membership"},
                )
            )

    catalog.propagate_project_cwds()
    return AdapterScan(catalog, metrics, (time.perf_counter() - start) * 1_000, root)


def iter_rollouts(root: Path, metrics: ScanMetrics) -> list[Path]:
    sessions = root / "sessions"
    found: list[Path] = []
    stack = [sessions]
    while stack:
        directory = stack.pop()
        for entry in metrics.scan_entries(directory):
            try:
                if entry.is_dir(follow_symlinks=False):
                    stack.append(Path(entry.path))
                elif entry.is_file(follow_symlinks=False) and entry.name.startswith(
                    "rollout-"
                ) and entry.name.endswith(".jsonl"):
                    found.append(Path(entry.path))
            except OSError:
                metrics.errors["entry_type"] += 1
    return sorted(found)


def codex_content_text(payload: dict[str, Any]) -> str | None:
    content = payload.get("content")
    return readable_text(content)


def is_internal_codex_session(payload: dict[str, Any]) -> bool:
    if payload.get("thread_source") == "subagent":
        return True
    source = payload.get("source")
    if isinstance(source, dict) and "subagent" in source:
        return True
    session_id = nonempty(payload.get("id")) or ""
    logical = nonempty(payload.get("session_id")) or ""
    parent = nonempty(payload.get("parent_thread_id")) or ""
    return bool(parent and session_id and logical and session_id != logical)


def scan_codex(root: Path, *, head_bytes: int) -> AdapterScan:
    start = time.perf_counter()
    metrics = ScanMetrics()
    catalog = Catalog(CODEX_ID)
    for path in iter_rollouts(root, metrics):
        metrics.register(path, primary=True)
        rows = metrics.read_jsonl_prefix(path, head_bytes)
        metadata: dict[str, Any] | None = None
        first_prompt: str | None = None
        for row in rows:
            kind = row.get("type")
            payload = row.get("payload")
            if kind == "session_meta" and isinstance(payload, dict) and metadata is None:
                metadata = payload
                metadata = dict(metadata)
                metadata["_record_timestamp"] = row.get("timestamp")
                continue
            if not isinstance(payload, dict) or first_prompt is not None:
                continue
            prompt: str | None = None
            if (
                kind == "response_item"
                and payload.get("type") == "message"
                and payload.get("role") == "user"
            ):
                prompt = codex_content_text(payload)
            elif kind == "event_msg" and payload.get("type") == "user_message":
                prompt = nonempty(payload.get("message"))
            if prompt and not is_injected_user_text(prompt):
                first_prompt = truncate_utf16(prompt).strip()

        if metadata is None:
            metrics.errors["codex_missing_session_meta"] += 1
            continue
        if is_internal_codex_session(metadata):
            metrics.evidence["internal-session-skipped"] += 1
            continue
        session_id = nonempty(metadata.get("id"))
        cwd = nonempty(metadata.get("cwd"))
        if session_id is None or cwd is None:
            metrics.errors["codex_invalid_session_meta"] += 1
            continue
        project_key = encode_project_key(cwd)
        catalog.add_session(
            SessionRecord(
                project_key=project_key,
                session_id=session_id,
                cwd=cwd,
                first_prompt=first_prompt,
                created_at=scalar_text(metadata.get("_record_timestamp"))
                or scalar_text(metadata.get("timestamp")),
                modified_at=str(path.stat().st_mtime_ns),
                evidence={"rollout-head"},
            )
        )
        metrics.evidence["rollout-head"] += 1
    catalog.propagate_project_cwds()
    return AdapterScan(catalog, metrics, (time.perf_counter() - start) * 1_000, root)


def grok_user_prompt(row: dict[str, Any]) -> str | None:
    if row.get("type") != "user" or nonempty(row.get("synthetic_reason")):
        return None
    text = readable_text(row.get("content"))
    if not text:
        return None
    match = re.search(r"<user_query>(.*?)</user_query>", text, re.DOTALL)
    if match:
        text = match.group(1).strip()
    return None if is_injected_user_text(text) else text


def scan_grok(root: Path, *, head_bytes: int, document_bytes: int) -> AdapterScan:
    start = time.perf_counter()
    metrics = ScanMetrics()
    catalog = Catalog(GROK_ID)
    sessions_root = root / "sessions"
    for project_entry in metrics.scan_entries(sessions_root):
        try:
            if not project_entry.is_dir(follow_symlinks=False):
                continue
        except OSError:
            metrics.errors["entry_type"] += 1
            continue
        encoded_cwd = project_entry.name
        cwd_from_path = urllib.parse.unquote(encoded_cwd)
        project_dir = Path(project_entry.path)
        for session_entry in metrics.scan_entries(project_dir):
            try:
                if not session_entry.is_dir(follow_symlinks=False):
                    continue
            except OSError:
                metrics.errors["entry_type"] += 1
                continue
            session_dir = Path(session_entry.path)
            files = {
                entry.name: Path(entry.path)
                for entry in metrics.scan_entries(session_dir)
                if entry.name in GROK_KNOWN_FILES
                and entry.is_file(follow_symlinks=False)
            }
            if not files:
                continue
            chat = files.get("chat_history.jsonl")
            if chat is not None:
                metrics.register(chat, primary=True)
            summary_path = files.get("summary.json")
            summary: dict[str, Any] | None = None
            if summary_path is not None:
                metrics.register(summary_path, metadata=True)
                summary = metrics.read_json_document(summary_path, document_bytes)

            cwd = cwd_from_path
            session_id = session_entry.name
            title = None
            created_at = None
            modified_at = None
            message_count = None
            if summary is not None:
                info = summary.get("info")
                if isinstance(info, dict):
                    session_id = nonempty(info.get("id")) or session_id
                    cwd = nonempty(info.get("cwd")) or cwd
                if not cwd:
                    cwd = nonempty(summary.get("git_root_dir")) or cwd
                title = nonempty(summary.get("generated_title")) or nonempty(
                    summary.get("session_summary")
                )
                created_at = scalar_text(summary.get("created_at"))
                modified_at = scalar_text(summary.get("updated_at")) or scalar_text(
                    summary.get("last_active_at")
                )
                count = summary.get("num_chat_messages")
                message_count = count if isinstance(count, int) else None

            first_prompt = None
            if title is None and chat is not None:
                for row in metrics.read_jsonl_prefix(chat, head_bytes):
                    first_prompt = grok_user_prompt(row)
                    if first_prompt:
                        break
            catalog.add_session(
                SessionRecord(
                    project_key=encode_project_key(cwd),
                    session_id=session_id,
                    cwd=cwd,
                    first_prompt=first_prompt,
                    title=title,
                    created_at=created_at,
                    modified_at=modified_at,
                    message_count=message_count,
                    evidence={"membership", "summary" if summary is not None else "chat-head"},
                )
            )
            metrics.evidence["membership"] += 1
            metrics.evidence["summary" if summary is not None else "chat-head"] += 1
    catalog.propagate_project_cwds()
    return AdapterScan(catalog, metrics, (time.perf_counter() - start) * 1_000, root)


@dataclass
class OracleCatalog:
    catalogs: dict[str, Catalog]
    database_size: int
    database_mtime_ns: int


def open_oracle(path: Path) -> sqlite3.Connection:
    encoded = urllib.parse.quote(str(path.resolve()), safe="/")
    connection = sqlite3.connect(f"file:{encoded}?mode=ro", uri=True)
    connection.execute("PRAGMA query_only = ON")
    return connection


def load_oracle(path: Path) -> OracleCatalog:
    catalogs = {adapter_id: Catalog(adapter_id) for adapter_id in ADAPTER_IDS}
    with open_oracle(path) as connection:
        session_rows = connection.execute(
            """
            SELECT si.adapter_id,
                   cs.native_project_key,
                   cs.native_session_id,
                   cs.cwd,
                   cs.first_prompt,
                   cs.ai_title,
                   cs.source_time
              FROM canonical_sessions cs
              JOIN fact_records fr ON fr.fact_id = cs.fact_id
              JOIN source_instances si
                ON si.source_instance_id = fr.source_instance_id
            """
        )
        for adapter_id, project, session, cwd, prompt, title, created in session_rows:
            catalog = catalogs.get(adapter_id)
            if catalog is None:
                continue
            catalog.add_session(
                SessionRecord(
                    project_key=project,
                    session_id=session,
                    cwd=nonempty(cwd),
                    first_prompt=nonempty(prompt),
                    title=nonempty(title),
                    created_at=scalar_text(created),
                    evidence={"canonical-session"},
                )
            )

        index_rows = connection.execute(
            """
            SELECT si.adapter_id,
                   csi.native_project_key,
                   csie.native_session_id,
                   csie.project_path,
                   csie.first_prompt,
                   csie.summary,
                   csie.created_at,
                   csie.modified_at,
                   csie.message_count
              FROM canonical_session_index_entries csie
              JOIN canonical_session_indexes csi
                ON csi.project_key = csie.project_key
              JOIN fact_records fr ON fr.fact_id = csi.decisive_fact_id
              JOIN source_instances si
                ON si.source_instance_id = fr.source_instance_id
            """
        )
        for row in index_rows:
            adapter_id, project, session, cwd, prompt, title, created, modified, count = row
            catalog = catalogs.get(adapter_id)
            if catalog is None:
                continue
            catalog.add_session(
                SessionRecord(
                    project_key=project,
                    session_id=session,
                    cwd=nonempty(cwd),
                    first_prompt=nonempty(prompt),
                    title=nonempty(title),
                    created_at=scalar_text(created),
                    modified_at=scalar_text(modified),
                    message_count=count if isinstance(count, int) else None,
                    evidence={"canonical-session-index"},
                )
            )

        project_rows = connection.execute(
            """
            WITH project_evidence(adapter_id, native_project_key) AS (
              SELECT si.adapter_id, cs.native_project_key
                FROM canonical_sessions cs
                JOIN fact_records fr ON fr.fact_id = cs.fact_id
                JOIN source_instances si
                  ON si.source_instance_id = fr.source_instance_id
              UNION
              SELECT si.adapter_id, csi.native_project_key
                FROM canonical_session_indexes csi
                JOIN fact_records fr ON fr.fact_id = csi.decisive_fact_id
                JOIN source_instances si
                  ON si.source_instance_id = fr.source_instance_id
              UNION
              SELECT si.adapter_id, pm.native_project_key
                FROM canonical_project_memory_documents pm
                JOIN fact_records fr ON fr.fact_id = pm.decisive_fact_id
                JOIN source_instances si
                  ON si.source_instance_id = fr.source_instance_id
            )
            SELECT adapter_id, native_project_key FROM project_evidence
            """
        )
        for adapter_id, project in project_rows:
            catalog = catalogs.get(adapter_id)
            if catalog is not None:
                catalog.add_project(project)

    stat = path.stat()
    return OracleCatalog(catalogs, stat.st_size, stat.st_mtime_ns)


def coverage(catalog: Catalog) -> dict[str, Any]:
    sessions = list(catalog.sessions.values())
    total = len(sessions)

    def count(name: str) -> int:
        return sum(getattr(session, name) is not None for session in sessions)

    labels = sum(
        session.first_prompt is not None or session.title is not None for session in sessions
    )

    def field(value: int) -> dict[str, Any]:
        return {
            "count": value,
            "percent": round((value / total * 100) if total else 100.0, 3),
        }

    project_total = len(catalog.projects)
    return {
        "projectDisplayPath": {
            "count": len(catalog.project_cwds),
            "percent": round(
                (len(catalog.project_cwds) / project_total * 100)
                if project_total
                else 100.0,
                3,
            ),
        },
        "cwd": field(count("cwd")),
        "displayLabel": field(labels),
        "firstPrompt": field(count("first_prompt")),
        "titleOrSummary": field(count("title")),
        "createdAt": field(count("created_at")),
        "modifiedAt": field(count("modified_at")),
        "nativeMessageCount": field(count("message_count")),
    }


def compare_catalog(actual: Catalog, oracle: Catalog) -> dict[str, Any]:
    actual_projects = actual.project_identities()
    oracle_projects = oracle.project_identities()
    actual_sessions = actual.session_identities()
    oracle_sessions = oracle.session_identities()
    missing_projects = oracle_projects - actual_projects
    extra_projects = actual_projects - oracle_projects
    missing_sessions = oracle_sessions - actual_sessions
    extra_sessions = actual_sessions - oracle_sessions

    common_keys = set(actual.sessions) & set(oracle.sessions)
    field_comparison: dict[str, Any] = {}
    for name in ("cwd", "first_prompt", "title", "created_at", "message_count"):
        available = 0
        comparable = 0
        exact = 0
        for key in common_keys:
            actual_value = getattr(actual.sessions[key], name)
            oracle_value = getattr(oracle.sessions[key], name)
            if actual_value is not None:
                available += 1
            if actual_value is not None and oracle_value is not None:
                comparable += 1
                exact += actual_value == oracle_value
        field_comparison[name] = {
            "available": available,
            "comparable": comparable,
            "exact": exact,
            "exactPercent": round((exact / comparable * 100) if comparable else 100.0, 3),
        }

    return {
        "projects": {
            "oracle": len(oracle_projects),
            "actual": len(actual_projects),
            "missing": len(missing_projects),
            "extra": len(extra_projects),
            "exact": not missing_projects and not extra_projects,
            "missingHashes": hashed_sample(missing_projects),
            "extraHashes": hashed_sample(extra_projects),
        },
        "sessions": {
            "oracle": len(oracle_sessions),
            "actual": len(actual_sessions),
            "missing": len(missing_sessions),
            "extra": len(extra_sessions),
            "exact": not missing_sessions and not extra_sessions,
            "missingHashes": hashed_sample(missing_sessions),
            "extraHashes": hashed_sample(extra_sessions),
        },
        "fields": field_comparison,
    }


def scan_json(scan: AdapterScan, oracle: Catalog | None) -> dict[str, Any]:
    catalog = scan.catalog
    metrics = scan.metrics
    sessions = catalog.session_identities()
    projects = catalog.project_identities()
    primary_ratio = (
        metrics.bytes_read / metrics.primary_bytes * 100 if metrics.primary_bytes else 0.0
    )
    result: dict[str, Any] = {
        "adapterId": catalog.adapter_id,
        "root": display_path(scan.root),
        "elapsedMs": round(scan.elapsed_ms, 3),
        "catalog": {
            "projects": len(projects),
            "sessions": len(sessions),
            "projectIdentityDigest": identity_digest(projects),
            "sessionIdentityDigest": identity_digest(sessions),
            "metadataCoverage": coverage(catalog),
        },
        "io": {
            "entriesVisited": metrics.entries_visited,
            "filesOpened": metrics.files_opened,
            "bytesRead": metrics.bytes_read,
            "primaryFiles": metrics.primary_files,
            "primaryBytes": metrics.primary_bytes,
            "metadataFiles": metrics.metadata_files,
            "metadataBytes": metrics.metadata_bytes,
            "bytesReadAsPercentOfPrimary": round(primary_ratio, 6),
        },
        "evidence": dict(sorted(metrics.evidence.items())),
        "errors": {
            "count": sum(metrics.errors.values()),
            "byCode": dict(sorted(metrics.errors.items())),
        },
    }
    if oracle is not None:
        result["oracleComparison"] = compare_catalog(catalog, oracle)
    return result


def print_summary(report: dict[str, Any]) -> None:
    print("Bounded catalog census")
    print(
        f"  head budget: {report['configuration']['headBytes']:,} B; "
        f"document budget: {report['configuration']['documentBytes']:,} B"
    )
    for adapter in report["adapters"]:
        catalog = adapter["catalog"]
        io = adapter["io"]
        label = catalog["metadataCoverage"]["displayLabel"]
        line = (
            f"  {adapter['adapterId']:<11} "
            f"{catalog['projects']:>4} projects, {catalog['sessions']:>5} sessions, "
            f"{adapter['elapsedMs']:>8.1f} ms, {io['bytesRead']:>10,} B read "
            f"({io['bytesReadAsPercentOfPrimary']:.4f}% of primary), "
            f"labels {label['count']}/{catalog['sessions']}"
        )
        comparison = adapter.get("oracleComparison")
        if comparison is not None:
            line += (
                f", oracle P={'exact' if comparison['projects']['exact'] else 'DIFF'}"
                f" S={'exact' if comparison['sessions']['exact'] else 'DIFF'}"
            )
        print(line)
        if adapter["errors"]["count"]:
            print(f"    errors: {adapter['errors']['byCode']}")
    totals = report["totals"]
    print(
        f"  total       {totals['projects']:>4} projects, {totals['sessions']:>5} sessions, "
        f"{totals['elapsedMs']:.1f} adapter-ms, {totals['bytesRead']:,} B read"
    )


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--adapter",
        action="append",
        choices=ADAPTER_IDS,
        help="Adapter to scan; repeatable. Defaults to all three.",
    )
    parser.add_argument("--claude-root", type=Path, default=Path("~/.claude"))
    parser.add_argument("--codex-root", type=Path, default=Path("~/.codex"))
    parser.add_argument("--grok-root", type=Path, default=Path("~/.grok"))
    parser.add_argument(
        "--oracle-db",
        type=Path,
        help="Optional Spaghetti database opened strictly read-only for parity comparison.",
    )
    parser.add_argument(
        "--head-bytes",
        type=int,
        default=64 * 1024,
        help="Maximum bytes read from one transcript/rollout head (default 65536).",
    )
    parser.add_argument(
        "--document-bytes",
        type=int,
        default=1024 * 1024,
        help="Maximum bytes read from one bounded metadata document (default 1 MiB).",
    )
    parser.add_argument(
        "--out",
        type=Path,
        default=Path("/private/tmp/spaghetti-catalog-census.json"),
        help="Aggregate JSON output path.",
    )
    parser.add_argument(
        "--fail-on-oracle-mismatch",
        action="store_true",
        help="Exit non-zero if any project/session identity set differs from the oracle.",
    )
    return parser


def main(argv: list[str] | None = None) -> int:
    process_start = time.perf_counter()
    args = build_parser().parse_args(argv)
    if args.head_bytes <= 0 or args.document_bytes <= 0:
        raise SystemExit("byte budgets must be positive")
    adapters = args.adapter or list(ADAPTER_IDS)
    roots = {
        CLAUDE_ID: args.claude_root.expanduser().resolve(),
        CODEX_ID: args.codex_root.expanduser().resolve(),
        GROK_ID: args.grok_root.expanduser().resolve(),
    }
    missing = [adapter for adapter in adapters if not roots[adapter].is_dir()]
    if missing:
        print(
            "missing agent roots: "
            + ", ".join(f"{adapter}={roots[adapter]}" for adapter in missing),
            file=sys.stderr,
        )
        return 2

    oracle = None
    if args.oracle_db is not None:
        oracle_path = args.oracle_db.expanduser().resolve()
        if not oracle_path.is_file():
            print(f"oracle database not found: {oracle_path}", file=sys.stderr)
            return 2
        oracle = load_oracle(oracle_path)

    scans: list[AdapterScan] = []
    for adapter in adapters:
        if adapter == CLAUDE_ID:
            scans.append(
                scan_claude(
                    roots[adapter],
                    head_bytes=args.head_bytes,
                    document_bytes=args.document_bytes,
                )
            )
        elif adapter == CODEX_ID:
            scans.append(scan_codex(roots[adapter], head_bytes=args.head_bytes))
        else:
            scans.append(
                scan_grok(
                    roots[adapter],
                    head_bytes=args.head_bytes,
                    document_bytes=args.document_bytes,
                )
            )

    adapter_json = [
        scan_json(scan, oracle.catalogs.get(scan.catalog.adapter_id) if oracle else None)
        for scan in scans
    ]
    report: dict[str, Any] = {
        "schemaVersion": 1,
        "generatedAt": utc_timestamp(),
        "privacy": (
            "Aggregate output only. Native paths, IDs, prompts, and titles are not emitted; "
            "identity sets are represented by SHA-256 digests and hashed mismatch samples."
        ),
        "configuration": {
            "adapters": adapters,
            "headBytes": args.head_bytes,
            "documentBytes": args.document_bytes,
        },
        "adapters": adapter_json,
        "totals": {
            "projects": sum(item["catalog"]["projects"] for item in adapter_json),
            "sessions": sum(item["catalog"]["sessions"] for item in adapter_json),
            "elapsedMs": round(sum(item["elapsedMs"] for item in adapter_json), 3),
            "bytesRead": sum(item["io"]["bytesRead"] for item in adapter_json),
            "primaryBytes": sum(item["io"]["primaryBytes"] for item in adapter_json),
            "filesOpened": sum(item["io"]["filesOpened"] for item in adapter_json),
            "entriesVisited": sum(item["io"]["entriesVisited"] for item in adapter_json),
        },
    }
    max_rss = resource.getrusage(resource.RUSAGE_SELF).ru_maxrss
    if platform.system() != "Darwin":
        max_rss *= 1024
    report["totals"]["wallElapsedMs"] = round(
        (time.perf_counter() - process_start) * 1_000, 3
    )
    report["totals"]["maxRssBytes"] = max_rss
    if oracle is not None:
        report["oracle"] = {
            "database": display_path(args.oracle_db.expanduser().resolve()),
            "sizeBytes": oracle.database_size,
            "mtimeNs": oracle.database_mtime_ns,
        }

    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    print_summary(report)
    print(f"  wrote {args.out}")

    if args.fail_on_oracle_mismatch and oracle is not None:
        for adapter in adapter_json:
            comparison = adapter["oracleComparison"]
            if not comparison["projects"]["exact"] or not comparison["sessions"]["exact"]:
                return 3
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

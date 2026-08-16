#!/usr/bin/env python3
"""Measure Claude runtime-observation and response-level usage semantics.

This is an independent, read-only native corpus experiment. It deliberately
does not import Spaghetti's adapter or SDK. Reports contain only aggregate
counts, token totals, timings, and a digest of file metadata; native paths,
identifiers, prompts, answers, and raw payloads are never emitted.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import time
from collections import Counter
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Iterable


UsageTuple = tuple[int, int, int, int]
UsageKnownTuple = tuple[bool, bool, bool, bool]

USAGE_KEYS = (
    "input_tokens",
    "output_tokens",
    "cache_creation_input_tokens",
    "cache_read_input_tokens",
)

OBSERVABLE_TOOLS = {
    "Agent",
    "AskUserQuestion",
    "EnterPlanMode",
    "ExitPlanMode",
    "Task",
    "TaskCreate",
    "TaskGet",
    "TaskList",
    "TaskOutput",
    "TaskStop",
    "TaskUpdate",
    "TodoWrite",
}

OBSERVABLE_RECORD_TYPES = {
    "assistant",
    "file-history-snapshot",
    "progress",
    "queue-operation",
    "summary",
    "system",
    "user",
}


def nonempty_string(value: Any) -> str | None:
    if not isinstance(value, str):
        return None
    value = value.strip()
    return value or None


def nonnegative_integer(value: Any) -> int | None:
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        return None
    if value < 0 or int(value) != value:
        return None
    return int(value)


def usage_tuple(value: Any) -> UsageTuple | None:
    if not isinstance(value, dict):
        return None
    parsed: list[int] = []
    for key in USAGE_KEYS:
        raw = value.get(key, 0)
        number = nonnegative_integer(raw)
        if number is None:
            return None
        parsed.append(number)
    return tuple(parsed)  # type: ignore[return-value]


def add_usage(left: UsageTuple, right: UsageTuple) -> UsageTuple:
    return tuple(a + b for a, b in zip(left, right, strict=True))  # type: ignore[return-value]


def subtract_usage(left: UsageTuple, right: UsageTuple) -> UsageTuple:
    return tuple(a - b for a, b in zip(left, right, strict=True))  # type: ignore[return-value]


def usage_json(value: UsageTuple) -> dict[str, int]:
    return dict(zip(USAGE_KEYS, value, strict=True))


def is_child_transcript(relative: Path) -> bool:
    return "subagents" in relative.parts


def is_declared_transcript(relative: Path) -> bool:
    """Mirror the current Claude transcript selectors, not every JSONL file."""
    if len(relative.parts) == 2:
        return True
    return (
        len(relative.parts) >= 4
        and relative.parts[2] == "subagents"
        and relative.name.startswith("agent-")
        and relative.name.endswith(".jsonl")
    )


def count_related_objects(projects_root: Path) -> dict[str, int]:
    home = projects_root.parent
    return {
        "activeSessionPresence": sum(1 for path in (home / "sessions").glob("*.json") if path.is_file()),
        "plans": sum(1 for path in (home / "plans").glob("*.md") if path.is_file()),
        "subagentMetadata": sum(
            1
            for path in projects_root.rglob("agent-*.meta.json")
            if path.is_file() and "subagents" in path.relative_to(projects_root).parts
        ),
        "taskItems": sum(1 for path in (home / "tasks").glob("*/*.json") if path.is_file()),
        "teamConfigs": sum(1 for path in (home / "teams").glob("*/config.json") if path.is_file()),
        "teamInboxDocuments": sum(
            1 for path in (home / "teams").glob("*/inboxes/*.json") if path.is_file()
        ),
        "todoDocuments": sum(1 for path in (home / "todos").glob("*-agent-*.json") if path.is_file()),
        "workflowJournals": sum(
            1
            for path in projects_root.glob("*/*/subagents/workflows/*/journal.jsonl")
            if path.is_file()
        ),
        "workflowRuns": sum(
            1 for path in projects_root.glob("*/*/workflows/wf_*.json") if path.is_file()
        ),
    }


def source_set_digest(files: Iterable[tuple[Path, int, int]], root: Path) -> str:
    digest = hashlib.sha256()
    for path, size, modified_ns in files:
        digest.update(str(path.relative_to(root)).encode())
        digest.update(b"\0")
        digest.update(str(size).encode())
        digest.update(b"\0")
        digest.update(str(modified_ns).encode())
        digest.update(b"\n")
    return digest.hexdigest()


@dataclass
class UsageGroup:
    first: UsageTuple
    latest: UsageTuple
    latest_known: UsageKnownTuple
    rows: int = 1
    changed_revisions: int = 0
    exact_repeats: int = 0
    downward_correction: bool = False
    request_id: str | None = None
    mixed_request_ids: bool = False
    model_present: bool = False
    effort_present: bool = False
    latest_model_present: bool = False
    latest_effort_present: bool = False

    def revise(
        self,
        usage: UsageTuple,
        known: UsageKnownTuple,
        request_id: str | None,
        model_present: bool,
        effort_present: bool,
    ) -> None:
        self.rows += 1
        if usage == self.latest:
            self.exact_repeats += 1
        else:
            self.changed_revisions += 1
        if any(current < previous for current, previous in zip(usage, self.latest, strict=True)):
            self.downward_correction = True
        if request_id != self.request_id:
            self.mixed_request_ids = True
        self.latest = usage
        self.latest_known = known
        self.model_present |= model_present
        self.effort_present |= effort_present
        self.latest_model_present = model_present
        self.latest_effort_present = effort_present


def content_blocks(record: dict[str, Any]) -> Iterable[dict[str, Any]]:
    message = record.get("message")
    if not isinstance(message, dict):
        return ()
    content = message.get("content")
    if isinstance(content, list):
        return (item for item in content if isinstance(item, dict))
    return ()


def has_effort(record: dict[str, Any]) -> bool:
    message = record.get("message")
    candidates = [record.get("effort")]
    if isinstance(message, dict):
        candidates.extend((message.get("effort"), message.get("reasoning_effort")))
        metadata = message.get("metadata")
        if isinstance(metadata, dict):
            candidates.extend((metadata.get("effort"), metadata.get("reasoning_effort")))
    return any(value is not None for value in candidates)


def field_presence(record: dict[str, Any], counts: Counter[str]) -> None:
    for key in ("mode", "permissionMode", "permission_mode", "effort", "reasoning_effort"):
        if key in record:
            counts[f"record.{key}"] += 1
    message = record.get("message")
    if not isinstance(message, dict):
        return
    for key in ("model", "mode", "permissionMode", "permission_mode", "effort", "reasoning_effort"):
        if key in message:
            counts[f"message.{key}"] += 1
    metadata = message.get("metadata")
    if isinstance(metadata, dict):
        for key in ("mode", "permissionMode", "permission_mode", "effort", "reasoning_effort"):
            if key in metadata:
                counts[f"message.metadata.{key}"] += 1


def analyze(root: Path) -> dict[str, Any]:
    started = time.perf_counter()
    root = root.expanduser().resolve()
    paths = sorted(
        path
        for path in root.rglob("*.jsonl")
        if path.is_file() and is_declared_transcript(path.relative_to(root))
    )
    source_metadata: list[tuple[Path, int, int]] = []
    initial_stats: dict[Path, tuple[int, int]] = {}
    for path in paths:
        stat = path.stat()
        initial_stats[path] = (stat.st_size, stat.st_mtime_ns)
        source_metadata.append((path, stat.st_size, stat.st_mtime_ns))

    records = 0
    bytes_read = 0
    malformed_lines = 0
    partial_final_lines = 0
    changed_during_scan = 0
    files_with_records = 0
    record_types: Counter[str] = Counter()
    other_record_types = 0
    field_counts: Counter[str] = Counter()
    actor_rows: Counter[str] = Counter()
    tool_counts: Counter[str] = Counter()
    tool_calls = 0
    tool_results = 0
    ask_opened = 0
    ask_resolved = 0
    ask_failed = 0
    ask_question_shapes = 0
    ask_pending_total = 0

    usage_rows = 0
    malformed_usage_rows = 0
    usage_rows_without_response_id = 0
    usage_rows_without_request_id = 0
    usage_rows_with_model = 0
    usage_rows_with_effort = 0
    legacy_delta_total: UsageTuple = (0, 0, 0, 0)
    groups: dict[tuple[int, str], UsageGroup] = {}
    usage_file_indexes: set[int] = set()
    usage_sessions: set[tuple[str, str]] = set()
    root_response_groups = 0
    child_response_groups = 0
    request_to_response: dict[tuple[int, str], str] = {}
    request_ids_with_multiple_responses: set[tuple[int, str]] = set()

    for file_index, path in enumerate(paths):
        relative = path.relative_to(root)
        role = "child" if is_child_transcript(relative) else "root"
        pending_asks: set[str] = set()
        file_records = 0
        with path.open("rb") as handle:
            for raw_line in handle:
                bytes_read += len(raw_line)
                if not raw_line.endswith(b"\n"):
                    partial_final_lines += 1
                    continue
                try:
                    record = json.loads(raw_line)
                except (json.JSONDecodeError, UnicodeDecodeError):
                    malformed_lines += 1
                    continue
                if not isinstance(record, dict):
                    malformed_lines += 1
                    continue
                records += 1
                file_records += 1
                actor_rows[role] += 1
                field_presence(record, field_counts)

                record_type = nonempty_string(record.get("type"))
                if record_type in OBSERVABLE_RECORD_TYPES:
                    record_types[record_type] += 1
                else:
                    other_record_types += 1

                for block in content_blocks(record):
                    block_type = block.get("type")
                    if block_type == "tool_use":
                        tool_calls += 1
                        name = nonempty_string(block.get("name"))
                        if name in OBSERVABLE_TOOLS:
                            tool_counts[name] += 1
                        if name == "AskUserQuestion":
                            ask_opened += 1
                            native_id = nonempty_string(block.get("id"))
                            if native_id:
                                pending_asks.add(native_id)
                            tool_input = block.get("input")
                            if isinstance(tool_input, dict) and isinstance(tool_input.get("questions"), list):
                                ask_question_shapes += 1
                    elif block_type == "tool_result":
                        tool_results += 1
                        native_id = nonempty_string(block.get("tool_use_id"))
                        if native_id and native_id in pending_asks:
                            pending_asks.remove(native_id)
                            if block.get("is_error") is True:
                                ask_failed += 1
                            else:
                                ask_resolved += 1
                        elif native_id:
                            # This is only an AskUserQuestion mismatch if the same
                            # file has an observed AskUserQuestion identifier. The
                            # aggregate is finalized after processing the file.
                            pass

                if record_type != "assistant":
                    continue
                message = record.get("message")
                if not isinstance(message, dict) or "usage" not in message:
                    continue
                usage = usage_tuple(message.get("usage"))
                if usage is None:
                    malformed_usage_rows += 1
                    continue
                native_usage = message["usage"]
                known = tuple(key in native_usage for key in USAGE_KEYS)
                usage_rows += 1
                legacy_delta_total = add_usage(legacy_delta_total, usage)
                response_id = nonempty_string(message.get("id"))
                request_id = nonempty_string(record.get("requestId"))
                row_uuid = nonempty_string(record.get("uuid"))
                model_present = nonempty_string(message.get("model")) is not None
                effort_present = has_effort(record)
                usage_rows_with_model += int(model_present)
                usage_rows_with_effort += int(effort_present)
                if response_id is None:
                    usage_rows_without_response_id += 1
                    response_key = f"fallback:{row_uuid or 'row'}:{file_records}"
                else:
                    response_key = f"native:{response_id}"
                if request_id is None:
                    usage_rows_without_request_id += 1
                elif response_id is not None:
                    request_key = (file_index, request_id)
                    prior_response = request_to_response.setdefault(request_key, response_id)
                    if prior_response != response_id:
                        request_ids_with_multiple_responses.add(request_key)
                group_key = (file_index, response_key)
                group = groups.get(group_key)
                if group is None:
                    usage_file_indexes.add(file_index)
                    session_component = relative.stem if len(relative.parts) == 2 else relative.parts[1]
                    usage_sessions.add((relative.parts[0], session_component))
                    if role == "root":
                        root_response_groups += 1
                    else:
                        child_response_groups += 1
                    groups[group_key] = UsageGroup(
                        first=usage,
                        latest=usage,
                        latest_known=known,  # type: ignore[arg-type]
                        request_id=request_id,
                        model_present=model_present,
                        effort_present=effort_present,
                        latest_model_present=model_present,
                        latest_effort_present=effort_present,
                    )
                else:
                    group.revise(
                        usage,
                        known,  # type: ignore[arg-type]
                        request_id,
                        model_present,
                        effort_present,
                    )

        if file_records:
            files_with_records += 1
        ask_pending_total += len(pending_asks)
        try:
            final_stat = path.stat()
        except FileNotFoundError:
            changed_during_scan += 1
        else:
            if (final_stat.st_size, final_stat.st_mtime_ns) != initial_stats[path]:
                changed_during_scan += 1

    response_snapshot_total: UsageTuple = (0, 0, 0, 0)
    response_first_total: UsageTuple = (0, 0, 0, 0)
    repeated_rows = 0
    repeated_groups = 0
    changed_revision_groups = 0
    exact_repeat_rows = 0
    downward_groups = 0
    mixed_request_groups = 0
    groups_with_model = 0
    groups_with_effort = 0
    latest_groups_with_model = 0
    latest_groups_with_effort = 0
    groups_with_all_buckets_known = 0
    latest_unknown_groups = [0, 0, 0, 0]
    for group in groups.values():
        response_snapshot_total = add_usage(response_snapshot_total, group.latest)
        response_first_total = add_usage(response_first_total, group.first)
        repeated_rows += group.rows - 1
        repeated_groups += int(group.rows > 1)
        changed_revision_groups += int(group.changed_revisions > 0)
        exact_repeat_rows += group.exact_repeats
        downward_groups += int(group.downward_correction)
        mixed_request_groups += int(group.mixed_request_ids)
        groups_with_model += int(group.model_present)
        groups_with_effort += int(group.effort_present)
        latest_groups_with_model += int(group.latest_model_present)
        latest_groups_with_effort += int(group.latest_effort_present)
        groups_with_all_buckets_known += int(all(group.latest_known))
        for index, known in enumerate(group.latest_known):
            latest_unknown_groups[index] += int(not known)

    root_files = sum(not is_child_transcript(path.relative_to(root)) for path in paths)
    child_files = len(paths) - root_files
    workflow_child_files = sum(
        is_child_transcript(path.relative_to(root))
        and "workflows" in path.relative_to(root).parts
        for path in paths
    )
    overcount = subtract_usage(legacy_delta_total, response_snapshot_total)
    elapsed_ms = (time.perf_counter() - started) * 1_000
    return {
        "schemaVersion": 1,
        "input": {
            "root": "~/.claude/projects" if root == (Path.home() / ".claude/projects").resolve() else "<provided-root>",
            "sourceSetDigest": source_set_digest(source_metadata, root),
            "files": len(paths),
            "rootTranscriptFiles": root_files,
            "childTranscriptFiles": child_files,
            "standardChildTranscriptFiles": child_files - workflow_child_files,
            "workflowChildTranscriptFiles": workflow_child_files,
            "filesWithRecords": files_with_records,
            "initialBytes": sum(size for _, size, _ in source_metadata),
            "bytesRead": bytes_read,
            "changedDuringScan": changed_during_scan,
        },
        "parse": {
            "records": records,
            "malformedCompleteLines": malformed_lines,
            "partialFinalLinesHeld": partial_final_lines,
            "recordTypes": dict(sorted(record_types.items())),
            "otherRecordTypes": other_record_types,
        },
        "actors": {
            "recordRowsByPathRole": dict(sorted(actor_rows.items())),
            "rootAndChildPathIdentityAvailable": root_files + child_files,
            "relatedScopedObjects": count_related_objects(root),
        },
        "usage": {
            "usageBearingAssistantRows": usage_rows,
            "malformedUsageRows": malformed_usage_rows,
            "fileScopedResponseGroups": len(groups),
            "usageActorFiles": len(usage_file_indexes),
            "usageSessions": len(usage_sessions),
            "rootResponseGroups": root_response_groups,
            "childResponseGroups": child_response_groups,
            "repeatedRowsBeyondFirst": repeated_rows,
            "groupsWithMultipleRows": repeated_groups,
            "groupsWithChangedCounters": changed_revision_groups,
            "exactRepeatRowsBeyondFirst": exact_repeat_rows,
            "groupsWithDownwardCorrection": downward_groups,
            "rowsWithoutMessageId": usage_rows_without_response_id,
            "rowsWithoutRequestId": usage_rows_without_request_id,
            "requestIdsMappingToMultipleMessageIds": len(request_ids_with_multiple_responses),
            "groupsWithMixedRequestIds": mixed_request_groups,
            "rowsWithModel": usage_rows_with_model,
            "groupsWithModel": groups_with_model,
            "rowsWithEffort": usage_rows_with_effort,
            "groupsWithEffort": groups_with_effort,
            "latestGroupsWithModel": latest_groups_with_model,
            "latestGroupsWithEffort": latest_groups_with_effort,
            "groupsWithAllBucketsKnown": groups_with_all_buckets_known,
            "latestResponseUnknownGroups": usage_json(tuple(latest_unknown_groups)),
            "legacyPerRowDeltaTotal": usage_json(legacy_delta_total),
            "firstResponseSnapshotTotal": usage_json(response_first_total),
            "latestResponseSnapshotTotal": usage_json(response_snapshot_total),
            "legacyMinusLatestSnapshot": usage_json(overcount),
        },
        "typedEvidence": {
            "fieldPresence": dict(sorted(field_counts.items())),
            "toolCalls": tool_calls,
            "toolResults": tool_results,
            "observableToolCalls": dict(sorted(tool_counts.items())),
            "userInputRequests": {
                "askUserQuestionCalls": ask_opened,
                "callsWithQuestionsArray": ask_question_shapes,
                "matchedSuccessfulResults": ask_resolved,
                "matchedErrorResults": ask_failed,
                "pendingAtFileEnd": ask_pending_total,
            },
        },
        "timing": {"elapsedMs": round(elapsed_ms, 3)},
        "privacy": (
            "Aggregate counts and token totals only. Native paths, identifiers, prompts, "
            "answers, model values, and raw payloads are not emitted."
        ),
        "limitations": [
            "This static census does not measure watcher latency, bootstrap barriers, queue backpressure, or reset delivery.",
            "Latest means the last complete record in current file order; a production observer also keys by source generation.",
            "Effort coverage checks known transcript field locations and does not infer launch settings or hook-only state.",
            "Pending AskUserQuestion calls at file end may reflect compaction, deletion, or results represented outside the scanned transcript.",
        ],
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--claude-projects",
        type=Path,
        default=Path.home() / ".claude/projects",
    )
    parser.add_argument(
        "--out",
        type=Path,
        default=Path("/private/tmp/spaghetti-runtime-observation-census.json"),
    )
    args = parser.parse_args()
    root = args.claude_projects.expanduser().resolve()
    if not root.is_dir():
        parser.error(f"Claude projects root does not exist: {root}")
    report = analyze(root)
    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    usage = report["usage"]
    print("Runtime observation census")
    print(f"  files:                    {report['input']['files']:,}")
    print(f"  records:                  {report['parse']['records']:,}")
    print(f"  usage rows:               {usage['usageBearingAssistantRows']:,}")
    print(f"  response groups:          {usage['fileScopedResponseGroups']:,}")
    print(f"  repeated rows:            {usage['repeatedRowsBeyondFirst']:,}")
    print(f"  changed-counter groups:   {usage['groupsWithChangedCounters']:,}")
    print(f"  downward-correction groups:{usage['groupsWithDownwardCorrection']:>8,}")
    print(f"  AskUserQuestion calls:    {report['typedEvidence']['userInputRequests']['askUserQuestionCalls']:,}")
    print(f"  elapsed:                  {report['timing']['elapsedMs']:.1f} ms")
    print(f"  wrote {args.out}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

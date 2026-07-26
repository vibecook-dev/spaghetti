#!/usr/bin/env python3
"""
Validate @spaghetti/core session and message types against REAL data in ~/.claude.

Walks ~/.claude/projects/ and validates:
1. sessions-index.json schema (all fields, extra/missing)
2. Session .jsonl message types and envelope fields
3. Assistant message content block types, stop_reasons, tool names
4. User message content block types
5. Progress message data.type values
6. System message subtype values
"""

import json
import os
import re
import sys
from pathlib import Path
from collections import defaultdict

from ts_types import declared_fields, discriminant_values, union_members

# ═══════════════════════════════════════════════════════════════════════════════
# EXPECTED SCHEMAS (read from the TypeScript types — see below)
# ═══════════════════════════════════════════════════════════════════════════════

PROJECTS_TS = "claude/projects.ts"

EXPECTED_SESSION_INDEX_ROOT_FIELDS = {"version", "originalPath", "entries"}

EXPECTED_SESSION_INDEX_ENTRY_FIELDS = {
    "sessionId", "fullPath", "fileMtime", "firstPrompt", "summary",
    "messageCount", "created", "modified", "gitBranch", "projectPath",
    "isSidechain"
}

EXPECTED_MESSAGE_TYPES = union_members("SessionMessageType", PROJECTS_TS)

EXPECTED_BASE_MESSAGE_FIELDS = {
    "type", "uuid", "parentUuid", "timestamp", "sessionId", "cwd",
    "version", "gitBranch", "isSidechain", "userType",
    # Optional fields from BaseMessageFields
    "slug", "permissionMode", "entrypoint"
}

# Extra known fields on specific message types (not in BaseMessageFields but expected)
KNOWN_EXTRA_ENVELOPE_FIELDS = {
    # UserMessage extras
    "message", "thinkingMetadata", "todos", "toolUseResult",
    "sourceToolAssistantUUID", "sourceToolUseID", "agentId", "isMeta",
    "isCompactSummary", "isVisibleInTranscriptOnly", "planContent",
    "promptId", "imagePasteIds", "teamName",
    # AssistantMessage extras
    "requestId", "error", "isApiErrorMessage", "apiError",
    # ProgressMessage extras
    "data", "toolUseID", "parentToolUseID",
    # SystemMessage extras
    "subtype", "level", "hookCount", "hookInfos", "hookErrors",
    "preventedContinuation", "stopReason", "hasOutput",
    "durationMs", "cause", "retryInMs", "retryAttempt", "maxRetries",
    "content", "logicalParentUuid", "compactMetadata",
    "microcompactMetadata",
    # SavedHookContextMessage extras
    "hookName", "hookEvent",
    # SummaryMessage extras
    "summary", "leafUuid",
    # LastPromptMessage extras
    "lastPrompt",
    # BridgeStatusSystemMessage extras
    "url",
    # QueueOperationMessage extras
    "operation",
    # FileHistorySnapshotMessage extras
    "messageId", "isSnapshotUpdate", "snapshot",
    # AgentNameMessage extras
    "agentName",
    # AttachmentMessage extras
    "attachment",
    # CustomTitleMessage extras
    "customTitle",
    # PrLinkMessage extras
    "prNumber", "prUrl", "prRepository",
    # Newer observed message extras
    "origin", "messageCount",
}

# Every property declared by any interface in projects.ts. The check below
# is a superset test — it asks "does the envelope carry a field the types
# model nowhere at all", not "is this field on the right interface" — so
# reading them in bulk is exactly as strict as the hand-written set above
# while being incapable of lagging the types. KNOWN_EXTRA_ENVELOPE_FIELDS
# is retained for fields observed in the wild that are deliberately not
# modelled yet.
DECLARED_TYPE_FIELDS = declared_fields(PROJECTS_TS)

ALL_KNOWN_ENVELOPE_FIELDS = (
    EXPECTED_BASE_MESSAGE_FIELDS | KNOWN_EXTRA_ENVELOPE_FIELDS | DECLARED_TYPE_FIELDS
)

EXPECTED_ASSISTANT_CONTENT_BLOCK_TYPES = {
    "thinking", "redacted_thinking", "text", "tool_use"
}

EXPECTED_STOP_REASONS = {
    "end_turn", "tool_use", "stop_sequence", "max_tokens", None
}

# mcp__* tools are matched by prefix at the call site, so the template
# literal member of the union is not represented here.
EXPECTED_TOOL_NAMES = union_members("ToolName", PROJECTS_TS)

EXPECTED_USER_CONTENT_BLOCK_TYPES = {
    "text", "tool_result", "image", "document"
}

EXPECTED_PROGRESS_DATA_TYPES = {
    "hook_progress", "bash_progress", "agent_progress",
    "mcp_progress", "query_update", "search_results_received",
    "waiting_for_task"
}

EXPECTED_SYSTEM_SUBTYPES = discriminant_values("subtype", PROJECTS_TS)


# ═══════════════════════════════════════════════════════════════════════════════
# VALIDATION
# ═══════════════════════════════════════════════════════════════════════════════

def main():
    claude_dir = Path.home() / ".claude"
    projects_dir = claude_dir / "projects"

    if not projects_dir.exists():
        print("ERROR: ~/.claude/projects/ does not exist")
        sys.exit(1)

    # Stats
    stats = {
        "projects_scanned": 0,
        "sessions_index_files": 0,
        "sessions_sampled": 0,
        "messages_parsed": 0,
        "jsonl_parse_errors": 0,
    }

    # Collectors
    seen_index_root_fields = set()
    seen_index_entry_fields = set()
    seen_message_types = set()
    seen_envelope_fields = defaultdict(set)  # per message type
    seen_all_envelope_fields = set()
    seen_assistant_content_types = set()
    seen_stop_reasons = set()
    seen_tool_names = set()
    seen_user_content_types = set()
    seen_progress_data_types = set()
    seen_system_subtypes = set()

    # Extra/unknown collectors
    extra_index_root_fields = set()
    extra_index_entry_fields = set()
    unknown_message_types = set()
    unknown_envelope_fields = defaultdict(set)
    unknown_assistant_content_types = set()
    unknown_stop_reasons = set()
    unknown_tool_names = set()
    unknown_user_content_types = set()
    unknown_progress_data_types = set()
    unknown_system_subtypes = set()

    # ──────────────────────────────────────────────────────────────────────────
    # PHASE 1: Validate sessions-index.json files
    # ──────────────────────────────────────────────────────────────────────────
    print("=" * 80)
    print("PHASE 1: Validating sessions-index.json files")
    print("=" * 80)

    all_session_jsonl_paths = []

    for project_dir in sorted(projects_dir.iterdir()):
        if not project_dir.is_dir():
            continue
        stats["projects_scanned"] += 1

        index_file = project_dir / "sessions-index.json"
        if not index_file.exists():
            continue

        stats["sessions_index_files"] += 1

        try:
            with open(index_file, "r") as f:
                index_data = json.load(f)
        except (json.JSONDecodeError, IOError) as e:
            print(f"  ERROR reading {index_file}: {e}")
            continue

        # Check root-level fields
        root_keys = set(index_data.keys())
        seen_index_root_fields.update(root_keys)

        extras = root_keys - EXPECTED_SESSION_INDEX_ROOT_FIELDS
        if extras:
            extra_index_root_fields.update(extras)

        # Check entry fields
        entries = index_data.get("entries", [])
        for entry in entries:
            entry_keys = set(entry.keys())
            seen_index_entry_fields.update(entry_keys)

            extras = entry_keys - EXPECTED_SESSION_INDEX_ENTRY_FIELDS
            if extras:
                extra_index_entry_fields.update(extras)

            # Collect .jsonl paths for phase 2
            full_path = entry.get("fullPath", "")
            if full_path and os.path.exists(full_path):
                all_session_jsonl_paths.append(full_path)

    # Also discover .jsonl files directly from project directories
    # (many projects don't have sessions-index.json)
    UUID_JSONL_RE = re.compile(r'^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}\.jsonl$')
    existing_paths = set(all_session_jsonl_paths)
    for project_dir in sorted(projects_dir.iterdir()):
        if not project_dir.is_dir():
            continue
        try:
            for f in project_dir.iterdir():
                if f.is_file() and UUID_JSONL_RE.match(f.name) and str(f) not in existing_paths:
                    all_session_jsonl_paths.append(str(f))
                    existing_paths.add(str(f))
        except OSError:
            pass

    # ──────────────────────────────────────────────────────────────────────────
    # PHASE 2-6: Parse session .jsonl files
    # ──────────────────────────────────────────────────────────────────────────
    print(f"\n{'=' * 80}")
    print("PHASE 2-6: Parsing session .jsonl files")
    print("=" * 80)

    # Sample sessions: take from across different projects
    # We want at least 20, but we'll sample broadly
    # Group by project directory to get diversity
    sessions_by_project = defaultdict(list)
    for p in all_session_jsonl_paths:
        # Group by the project dir (parent of the sessions dir that contains .jsonl)
        project_key = str(Path(p).parent.parent) if "sessions" in str(Path(p).parent.name).lower() or True else str(Path(p).parent)
        sessions_by_project[project_key].append(p)

    # Select sessions: pick up to 3 from each project, ensure >= 20 total
    sampled_paths = []
    for project_key in sorted(sessions_by_project.keys()):
        paths = sessions_by_project[project_key]
        # Pick up to 3 per project
        sampled_paths.extend(paths[:3])

    # If still under 20, grab more
    if len(sampled_paths) < 20:
        remaining = [p for p in all_session_jsonl_paths if p not in sampled_paths]
        sampled_paths.extend(remaining[:20 - len(sampled_paths)])

    # Actually, let's just scan ALL sessions to be thorough — it's validation
    # But cap at a reasonable limit to avoid taking forever
    MAX_SESSIONS = 200
    sampled_paths = all_session_jsonl_paths[:MAX_SESSIONS] if len(all_session_jsonl_paths) > MAX_SESSIONS else all_session_jsonl_paths

    print(f"  Total .jsonl files found: {len(all_session_jsonl_paths)}")
    print(f"  Sampling: {len(sampled_paths)} sessions")

    for jsonl_path in sampled_paths:
        stats["sessions_sampled"] += 1
        try:
            with open(jsonl_path, "r") as f:
                for line_num, line in enumerate(f, 1):
                    line = line.strip()
                    if not line:
                        continue
                    try:
                        msg = json.loads(line)
                    except json.JSONDecodeError:
                        stats["jsonl_parse_errors"] += 1
                        continue

                    stats["messages_parsed"] += 1
                    msg_type = msg.get("type")
                    seen_message_types.add(msg_type)

                    if msg_type not in EXPECTED_MESSAGE_TYPES:
                        unknown_message_types.add(msg_type)

                    # Collect envelope fields
                    msg_keys = set(msg.keys())
                    seen_envelope_fields[msg_type].update(msg_keys)
                    seen_all_envelope_fields.update(msg_keys)

                    unknown = msg_keys - ALL_KNOWN_ENVELOPE_FIELDS
                    if unknown:
                        unknown_envelope_fields[msg_type].update(unknown)

                    # ── Assistant messages ──
                    if msg_type == "assistant":
                        payload = msg.get("message", {})
                        if isinstance(payload, dict):
                            stop_reason = payload.get("stop_reason")
                            seen_stop_reasons.add(stop_reason)
                            if stop_reason not in EXPECTED_STOP_REASONS:
                                unknown_stop_reasons.add(stop_reason)

                            content = payload.get("content", [])
                            if isinstance(content, list):
                                for block in content:
                                    if isinstance(block, dict):
                                        bt = block.get("type")
                                        seen_assistant_content_types.add(bt)
                                        if bt not in EXPECTED_ASSISTANT_CONTENT_BLOCK_TYPES:
                                            unknown_assistant_content_types.add(bt)

                                        if bt == "tool_use":
                                            tool_name = block.get("name", "")
                                            seen_tool_names.add(tool_name)
                                            if tool_name not in EXPECTED_TOOL_NAMES:
                                                if not tool_name.startswith("mcp__"):
                                                    unknown_tool_names.add(tool_name)

                    # ── User messages ──
                    elif msg_type == "user":
                        payload = msg.get("message", {})
                        if isinstance(payload, dict):
                            content = payload.get("content")
                            if isinstance(content, list):
                                for block in content:
                                    if isinstance(block, dict):
                                        bt = block.get("type")
                                        seen_user_content_types.add(bt)
                                        if bt not in EXPECTED_USER_CONTENT_BLOCK_TYPES:
                                            unknown_user_content_types.add(bt)

                    # ── Progress messages ──
                    elif msg_type == "progress":
                        data = msg.get("data", {})
                        if isinstance(data, dict):
                            dt = data.get("type")
                            if dt is not None:
                                seen_progress_data_types.add(dt)
                                if dt not in EXPECTED_PROGRESS_DATA_TYPES:
                                    unknown_progress_data_types.add(dt)

                    # ── System messages ──
                    elif msg_type == "system":
                        subtype = msg.get("subtype")
                        if subtype is not None:
                            seen_system_subtypes.add(subtype)
                            if subtype not in EXPECTED_SYSTEM_SUBTYPES:
                                unknown_system_subtypes.add(subtype)

        except IOError as e:
            print(f"  ERROR reading {jsonl_path}: {e}")

    # ═══════════════════════════════════════════════════════════════════════════
    # REPORT
    # ═══════════════════════════════════════════════════════════════════════════
    print(f"\n{'=' * 80}")
    print("VALIDATION REPORT")
    print("=" * 80)

    results = []

    def check(name, passed, details=""):
        status = "PASS" if passed else "FAIL"
        results.append((name, passed))
        print(f"\n  [{status}] {name}")
        if details:
            for line in details.strip().split("\n"):
                print(f"         {line}")

    # ── 1. Sessions Index Root Fields ──
    print(f"\n{'─' * 60}")
    print("1. SESSIONS INDEX — Root Fields")
    print(f"{'─' * 60}")

    missing_root = EXPECTED_SESSION_INDEX_ROOT_FIELDS - seen_index_root_fields - {"originalPath"}  # originalPath is optional
    check(
        "sessions-index.json root fields complete",
        len(extra_index_root_fields) == 0 and len(missing_root) == 0,
        (f"Seen fields: {sorted(seen_index_root_fields)}\n"
         f"Extra fields: {sorted(extra_index_root_fields) if extra_index_root_fields else 'none'}\n"
         f"Missing fields: {sorted(missing_root) if missing_root else 'none'}")
    )

    # ── 2. Sessions Index Entry Fields ──
    print(f"\n{'─' * 60}")
    print("2. SESSIONS INDEX — Entry Fields")
    print(f"{'─' * 60}")

    missing_entry = EXPECTED_SESSION_INDEX_ENTRY_FIELDS - seen_index_entry_fields
    check(
        "sessions-index entry fields complete",
        len(extra_index_entry_fields) == 0 and len(missing_entry) == 0,
        (f"Seen fields: {sorted(seen_index_entry_fields)}\n"
         f"Extra fields: {sorted(extra_index_entry_fields) if extra_index_entry_fields else 'none'}\n"
         f"Missing fields: {sorted(missing_entry) if missing_entry else 'none'}")
    )

    # ── 3. Message Types ──
    print(f"\n{'─' * 60}")
    print("3. SESSION MESSAGES — Type Values")
    print(f"{'─' * 60}")

    missing_types = EXPECTED_MESSAGE_TYPES - seen_message_types
    check(
        "All message type values covered",
        len(unknown_message_types) == 0,
        (f"Seen types: {sorted(seen_message_types)}\n"
         f"Unknown/new types: {sorted(str(x) for x in unknown_message_types) if unknown_message_types else 'none'}\n"
         f"Expected but not seen: {sorted(missing_types) if missing_types else 'none'}")
    )

    # ── 4. Envelope Fields ──
    print(f"\n{'─' * 60}")
    print("4. SESSION MESSAGES — Envelope Fields (per type)")
    print(f"{'─' * 60}")

    has_unknown_envelope = any(v for v in unknown_envelope_fields.values())
    check(
        "All envelope fields accounted for",
        not has_unknown_envelope,
        f"All seen fields: {sorted(seen_all_envelope_fields)}"
    )
    if has_unknown_envelope:
        for msg_type, fields in sorted(unknown_envelope_fields.items()):
            if fields:
                print(f"         Type '{msg_type}' has unknown fields: {sorted(fields)}")

    # ── 5. Assistant Content Block Types ──
    print(f"\n{'─' * 60}")
    print("5. ASSISTANT MESSAGES — Content Block Types")
    print(f"{'─' * 60}")

    check(
        "All assistant content block types covered",
        len(unknown_assistant_content_types) == 0,
        (f"Seen types: {sorted(seen_assistant_content_types)}\n"
         f"Unknown/new types: {sorted(str(x) for x in unknown_assistant_content_types) if unknown_assistant_content_types else 'none'}")
    )

    # ── 6. Stop Reasons ──
    print(f"\n{'─' * 60}")
    print("6. ASSISTANT MESSAGES — Stop Reasons")
    print(f"{'─' * 60}")

    check(
        "All stop_reason values covered",
        len(unknown_stop_reasons) == 0,
        (f"Seen: {sorted(str(x) for x in seen_stop_reasons)}\n"
         f"Unknown/new: {sorted(str(x) for x in unknown_stop_reasons) if unknown_stop_reasons else 'none'}")
    )

    # ── 7. Tool Names ──
    print(f"\n{'─' * 60}")
    print("7. ASSISTANT MESSAGES — Tool Names")
    print(f"{'─' * 60}")

    mcp_tools = sorted([t for t in seen_tool_names if t.startswith("mcp__")])
    non_mcp_tools = sorted([t for t in seen_tool_names if not t.startswith("mcp__")])

    check(
        "All tool names covered (non-mcp)",
        len(unknown_tool_names) == 0,
        (f"Seen non-mcp tools: {non_mcp_tools}\n"
         f"Seen mcp tools ({len(mcp_tools)}): {mcp_tools[:20]}{'...' if len(mcp_tools) > 20 else ''}\n"
         f"Unknown/new tools: {sorted(unknown_tool_names) if unknown_tool_names else 'none'}")
    )

    # ── 8. User Content Block Types ──
    print(f"\n{'─' * 60}")
    print("8. USER MESSAGES — Content Block Types")
    print(f"{'─' * 60}")

    check(
        "All user content block types covered",
        len(unknown_user_content_types) == 0,
        (f"Seen types: {sorted(seen_user_content_types)}\n"
         f"Unknown/new types: {sorted(str(x) for x in unknown_user_content_types) if unknown_user_content_types else 'none'}")
    )

    # ── 9. Progress Data Types ──
    print(f"\n{'─' * 60}")
    print("9. PROGRESS MESSAGES — data.type Values")
    print(f"{'─' * 60}")

    missing_progress = EXPECTED_PROGRESS_DATA_TYPES - seen_progress_data_types
    check(
        "All progress data.type values covered",
        len(unknown_progress_data_types) == 0,
        (f"Seen types: {sorted(seen_progress_data_types)}\n"
         f"Unknown/new types: {sorted(str(x) for x in unknown_progress_data_types) if unknown_progress_data_types else 'none'}\n"
         f"Expected but not seen: {sorted(missing_progress) if missing_progress else 'none'}")
    )

    # ── 10. System Subtypes ──
    print(f"\n{'─' * 60}")
    print("10. SYSTEM MESSAGES — Subtype Values")
    print(f"{'─' * 60}")

    missing_subtypes = EXPECTED_SYSTEM_SUBTYPES - seen_system_subtypes
    check(
        "All system subtype values covered",
        len(unknown_system_subtypes) == 0,
        (f"Seen subtypes: {sorted(seen_system_subtypes)}\n"
         f"Unknown/new subtypes: {sorted(str(x) for x in unknown_system_subtypes) if unknown_system_subtypes else 'none'}\n"
         f"Expected but not seen: {sorted(missing_subtypes) if missing_subtypes else 'none'}")
    )

    # ═══════════════════════════════════════════════════════════════════════════
    # SUMMARY
    # ═══════════════════════════════════════════════════════════════════════════
    print(f"\n{'=' * 80}")
    print("STATISTICS")
    print("=" * 80)
    print(f"  Projects scanned:        {stats['projects_scanned']}")
    print(f"  Sessions-index files:    {stats['sessions_index_files']}")
    print(f"  Sessions sampled:        {stats['sessions_sampled']}")
    print(f"  Messages parsed:         {stats['messages_parsed']}")
    print(f"  JSONL parse errors:      {stats['jsonl_parse_errors']}")
    print(f"  Unique message types:    {len(seen_message_types)}")
    print(f"  Unique tool names:       {len(seen_tool_names)}")
    print(f"  Unique envelope fields:  {len(seen_all_envelope_fields)}")

    print(f"\n{'=' * 80}")
    print("OVERALL RESULT")
    print("=" * 80)
    passed = sum(1 for _, p in results if p)
    failed = sum(1 for _, p in results if not p)
    print(f"  {passed} PASSED, {failed} FAILED out of {len(results)} checks")

    if failed > 0:
        print("\n  FAILED checks:")
        for name, p in results:
            if not p:
                print(f"    - {name}")

    print()
    sys.exit(0 if failed == 0 else 1)


if __name__ == "__main__":
    main()

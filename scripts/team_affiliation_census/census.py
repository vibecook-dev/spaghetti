#!/usr/bin/env python3
"""Measure Claude team-to-actor correlation inputs without emitting native values.

The experiment is read-only and adapter-independent. Its JSON report contains
only aggregate shape and join-cardinality counts; it never emits source paths,
team/member names, session/agent identifiers, prompts, or raw payloads.
"""

from __future__ import annotations

import argparse
import json
from collections import Counter
from dataclasses import dataclass
from pathlib import Path
from typing import Any


REPORT_CONTRACT_VERSION = 1


def nonempty_string(value: Any) -> str | None:
    if not isinstance(value, str):
        return None
    value = value.strip()
    return value or None


@dataclass(frozen=True)
class TeamConfig:
    directory_id: str
    document_name: str
    lead_session_id: str
    member_names: tuple[str, ...]
    lead_member_matches: int


def _read_object(path: Path) -> dict[str, Any] | None:
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError):
        return None
    return value if isinstance(value, dict) else None


def _parse_config(path: Path) -> TeamConfig | None:
    value = _read_object(path)
    if value is None:
        return None
    document_name = nonempty_string(value.get("name"))
    lead_agent_id = nonempty_string(value.get("leadAgentId"))
    lead_session_id = nonempty_string(value.get("leadSessionId"))
    members = value.get("members")
    if document_name is None or lead_agent_id is None or lead_session_id is None or not isinstance(members, list):
        return None
    parsed_members: list[tuple[str, str]] = []
    for member in members:
        if not isinstance(member, dict):
            return None
        agent_id = nonempty_string(member.get("agentId"))
        name = nonempty_string(member.get("name"))
        if agent_id is None or name is None:
            return None
        parsed_members.append((agent_id, name))
    return TeamConfig(
        directory_id=path.parent.name,
        document_name=document_name,
        lead_session_id=lead_session_id,
        member_names=tuple(name for _, name in parsed_members),
        lead_member_matches=sum(agent_id == lead_agent_id for agent_id, _ in parsed_members),
    )


def _metadata_transcript_sibling(path: Path) -> Path:
    suffix = ".meta.json"
    if not path.name.endswith(suffix):
        return path.with_name(path.name + ".jsonl")
    return path.with_name(path.name[: -len(suffix)] + ".jsonl")


def analyze(root: Path) -> dict[str, Any]:
    root = root.expanduser().resolve()
    teams_root = root / "teams"
    projects_root = root / "projects"
    config_paths = sorted(path for path in teams_root.glob("*/config.json") if path.is_file())
    metadata_paths = sorted(
        path
        for path in projects_root.glob("*/*/subagents/**/agent-*.meta.json")
        if path.is_file()
    )

    config_counts: Counter[str] = Counter(files=len(config_paths))
    configs: dict[str, TeamConfig] = {}
    for path in config_paths:
        config = _parse_config(path)
        if config is None:
            config_counts["invalid"] += 1
            continue
        config_counts["valid"] += 1
        config_counts["members"] += len(config.member_names)
        config_counts["nameMatchesDirectory"] += config.document_name == config.directory_id
        config_counts["uniqueMemberNames"] += len(config.member_names) == len(set(config.member_names))
        config_counts["exactlyOneLeadMember"] += config.lead_member_matches == 1
        configs[config.directory_id] = config

    metadata_counts: Counter[str] = Counter(files=len(metadata_paths))
    for path in metadata_paths:
        value = _read_object(path)
        if value is None:
            metadata_counts["invalid"] += 1
            continue
        metadata_counts["valid"] += 1
        team_name = nonempty_string(value.get("teamName"))
        member_name = nonempty_string(value.get("name"))
        if team_name is None or member_name is None:
            metadata_counts["withoutTeamAndName"] += 1
            continue
        metadata_counts["withTeamAndName"] += 1
        relative = path.relative_to(projects_root)
        if len(relative.parts) < 4:
            metadata_counts["invalidLayout"] += 1
            continue
        native_session_id = relative.parts[1]
        config = configs.get(team_name)
        if config is None:
            metadata_counts["noCurrentConfigMatch"] += 1
            continue
        member_matches = sum(name == member_name for name in config.member_names)
        if config.lead_session_id != native_session_id or member_matches == 0:
            metadata_counts["noCurrentConfigMatch"] += 1
            continue
        if member_matches > 1:
            metadata_counts["ambiguousCurrentConfigMatch"] += 1
            continue
        metadata_counts["uniqueCurrentConfigMatch"] += 1
        if _metadata_transcript_sibling(path).is_file():
            metadata_counts["uniqueMatchWithTranscript"] += 1
        else:
            metadata_counts["uniqueMatchWithoutTranscript"] += 1

    valid_configs = config_counts["valid"]
    return {
        "reportContractVersion": REPORT_CONTRACT_VERSION,
        "adapterId": "claude-code",
        "experiment": "native-team-affiliation-shape-census",
        "privacy": {
            "classification": "aggregate-only",
            "nativeValuesRetained": False,
            "rawPayloadsRetained": False,
            "sourcePathsRetained": False,
        },
        "teamConfigs": {
            "files": config_counts["files"],
            "valid": valid_configs,
            "invalid": config_counts["invalid"],
            "members": config_counts["members"],
            "nameMatchesDirectory": config_counts["nameMatchesDirectory"],
            "uniqueMemberNames": config_counts["uniqueMemberNames"],
            "exactlyOneLeadMember": config_counts["exactlyOneLeadMember"],
        },
        "subagentMetadata": {
            "files": metadata_counts["files"],
            "valid": metadata_counts["valid"],
            "invalid": metadata_counts["invalid"],
            "withTeamAndName": metadata_counts["withTeamAndName"],
            "withoutTeamAndName": metadata_counts["withoutTeamAndName"],
            "invalidLayout": metadata_counts["invalidLayout"],
            "uniqueCurrentConfigMatch": metadata_counts["uniqueCurrentConfigMatch"],
            "ambiguousCurrentConfigMatch": metadata_counts["ambiguousCurrentConfigMatch"],
            "noCurrentConfigMatch": metadata_counts["noCurrentConfigMatch"],
            "uniqueMatchWithTranscript": metadata_counts["uniqueMatchWithTranscript"],
            "uniqueMatchWithoutTranscript": metadata_counts["uniqueMatchWithoutTranscript"],
        },
        "conclusion": {
            "rootLeadRuleSupported": valid_configs > 0
            and config_counts["exactlyOneLeadMember"] == valid_configs,
            "teamDirectoryIdentitySupported": valid_configs > 0
            and config_counts["nameMatchesDirectory"] == valid_configs,
            "childMetadataRuleSupported": metadata_counts["uniqueCurrentConfigMatch"] > 0
            and metadata_counts["ambiguousCurrentConfigMatch"] == 0,
        },
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", required=True, type=Path, help="Claude data root to inspect read-only")
    parser.add_argument("--report", type=Path, help="optional aggregate-only JSON output")
    args = parser.parse_args()
    report = analyze(args.root)
    encoded = json.dumps(report, indent=2, sort_keys=True) + "\n"
    if args.report is not None:
        args.report.write_text(encoded, encoding="utf-8")
    print(encoded, end="")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

#!/usr/bin/env python3
"""
Validate an agent coverage claim against a ground-truth scan.

Honesty rules (default policy):
1. Every non-zero ground-truth bucket the scanner exposes must match a claim
   surface (or be covered by a prefix/glob rule) — no silent unknowns.
2. Every top-level root entry with size ≥ threshold must be claimed.
3. Claim surfaces with invalid status / missing match fail schema checks.
4. Optional: report ingested fraction of rollout/session records (informational).

Does not require Spaghetti to be installed or an index DB to exist.
"""

from __future__ import annotations

import argparse
import fnmatch
import sys
from pathlib import Path
from typing import Any

sys.path.insert(0, str(Path(__file__).resolve().parent))
from common import (  # noqa: E402
    CheckResult,
    claim_surface_map,
    ground_truth_path,
    load_claim,
    read_json,
    summarize_checks,
)


def surface_covers_record_type(surface: dict[str, Any], record_type: str) -> bool:
    m = surface.get("match") or {}
    if m.get("recordType") == record_type:
        return True
    prefix = m.get("recordTypePrefix")
    if prefix and record_type.startswith(prefix):
        return True
    # wildcard id patterns like event_msg/* already handled via prefix
    return False


def surface_covers_toplevel(surface: dict[str, Any], name: str) -> bool:
    m = surface.get("match") or {}
    if m.get("toplevel") == name:
        return True
    glob = m.get("toplevelGlob")
    if glob and fnmatch.fnmatch(name, glob):
        return True
    return False


def surface_covers_bucket(surface: dict[str, Any], bucket_key: str) -> bool:
    m = surface.get("match") or {}
    if m.get("bucket") == bucket_key:
        return True
    prefix = m.get("bucketPrefix")
    if prefix and bucket_key.startswith(prefix):
        return True
    return False


def find_covering_surfaces(
    surfaces: list[dict[str, Any]],
    *,
    record_type: str | None = None,
    toplevel: str | None = None,
    bucket: str | None = None,
) -> list[dict[str, Any]]:
    hits = []
    for s in surfaces:
        if record_type and surface_covers_record_type(s, record_type):
            hits.append(s)
        if toplevel and surface_covers_toplevel(s, toplevel):
            hits.append(s)
        if bucket and surface_covers_bucket(s, bucket):
            hits.append(s)
    return hits


def validate(claim: dict[str, Any], gt: dict[str, Any]) -> list[CheckResult]:
    results: list[CheckResult] = []
    agent = claim["agentId"]
    if gt.get("agentId") != agent:
        results.append(
            CheckResult(
                "agentId match",
                False,
                f"claim={agent} ground_truth={gt.get('agentId')}",
            )
        )
        return results
    results.append(CheckResult("agentId match", True, agent))

    if not gt.get("rootExists", True):
        results.append(CheckResult("root exists", False, gt.get("root", "?")))
        return results
    results.append(CheckResult("root exists", True, gt.get("root", "")))

    surfaces = claim["surfaces"]
    policy = claim.get("policy") or {}
    min_bytes = int(policy.get("minBytesForToplevelClaim", 1))
    min_count = int(policy.get("minCountForRecordTypeClaim", 1))
    require_tl = bool(policy.get("requireClaimForToplevelWithBytes", True))
    require_rt = bool(policy.get("requireClaimForRecordTypesWithCount", True))

    # Schema: unique ids + valid status already checked by claim_surface_map
    try:
        smap = claim_surface_map(claim)
        results.append(CheckResult("claim schema (unique ids, status)", True, f"{len(smap)} surfaces"))
    except Exception as e:
        results.append(CheckResult("claim schema (unique ids, status)", False, str(e)))
        return results

    buckets = gt.get("buckets") or {}

    # --- Record types (Codex rollouts / Grok chat_history) ---
    record_types: dict[str, int] = {}
    if "rollout.record_type" in buckets and isinstance(buckets["rollout.record_type"], dict):
        record_types = {k: int(v) for k, v in buckets["rollout.record_type"].items()}
    elif "chat_history.record_type" in buckets and isinstance(
        buckets["chat_history.record_type"], dict
    ):
        record_types = {k: int(v) for k, v in buckets["chat_history.record_type"].items()}

    undocumented_rt: list[str] = []
    for rt, count in sorted(record_types.items(), key=lambda kv: -kv[1]):
        if count < min_count:
            continue
        hits = find_covering_surfaces(surfaces, record_type=rt)
        if not hits:
            undocumented_rt.append(f"{rt} (n={count})")
        else:
            # at least one surface documents it
            pass

    if require_rt and record_types:
        label = (
            "chat_history"
            if "chat_history.record_type" in buckets
            else "rollout"
        )
        results.append(
            CheckResult(
                f"all non-zero {label} record types claimed",
                len(undocumented_rt) == 0,
                (
                    "ok"
                    if not undocumented_rt
                    else f"{len(undocumented_rt)} missing: " + "; ".join(undocumented_rt[:12])
                    + (" …" if len(undocumented_rt) > 12 else "")
                ),
            )
        )

    # --- Named scalar buckets (Claude + shared) ---
    # Primary inventory buckets only (not derived rollups like chat_messages /
    # tool_ish — those are informational in the scan JSON).
    named_bucket_keys = [
        "session.jsonl.line",
        "project.memory",
        "project.sessions_index",
        "subagent.jsonl",
        "tool_result.file",
        "workflow.file",
        "rollout.file",
        "chat_history.file",
        "chat_history.embedded_tool_call",
        "sibling.summary.json",
        "sibling.events.jsonl",
        "sibling.signals.json",
        "sibling.updates.jsonl",
        "compaction.segment.file",
    ]
    for key in named_bucket_keys:
        if key not in buckets:
            continue
        b = buckets[key]
        count = int(b.get("count", 0) if isinstance(b, dict) else 0)
        if count <= 0:
            continue
        hits = find_covering_surfaces(surfaces, bucket=key)
        results.append(
            CheckResult(
                f"bucket claimed: {key}",
                len(hits) > 0,
                f"count={count}" if hits else f"count={count} — no claim surface",
            )
        )

    # secondary.* for Claude
    secondary = buckets.get("secondary")
    if isinstance(secondary, dict):
        for name, meta in secondary.items():
            if not isinstance(meta, dict):
                continue
            if not meta.get("exists"):
                continue
            fc = int(meta.get("file_count", 0))
            if fc <= 0:
                continue
            bkey = f"secondary.{name}"
            hits = find_covering_surfaces(surfaces, bucket=bkey)
            results.append(
                CheckResult(
                    f"secondary claimed: {name}",
                    len(hits) > 0,
                    f"files={fc}" if hits else f"files={fc} — no claim",
                )
            )

    # --- Top-level root entries ---
    toplevel = gt.get("toplevel") or {}
    undocumented_tl: list[str] = []
    if require_tl:
        for name, meta in sorted(toplevel.items()):
            if not isinstance(meta, dict):
                continue
            nbytes = int(meta.get("bytes") or 0)
            if nbytes < min_bytes:
                continue
            # skip WAL/SHM companions of claimed sqlite bases if glob covers base only —
            # require claim on exact name or glob
            hits = find_covering_surfaces(surfaces, toplevel=name)
            # Secondary trees are often claimed as secondary.<name> (Claude)
            # rather than toplevel.<name> — treat that as covering the root entry.
            if not hits:
                hits = find_covering_surfaces(surfaces, bucket=f"secondary.{name}")
            if not hits and name == "sessions":
                # Claude PID registry claimed as secondary.sessions_pid
                hits = find_covering_surfaces(surfaces, bucket="secondary.sessions_pid")
            if not hits:
                # SQLite companions: state_5.sqlite-wal covered if state_*.sqlite claimed
                base = name
                for suf in ("-shm", "-wal"):
                    if name.endswith(suf):
                        base = name[: -len(suf)]
                        break
                if base != name:
                    hits = find_covering_surfaces(surfaces, toplevel=base)
            if not hits:
                undocumented_tl.append(f"{name} ({nbytes} B)")

        results.append(
            CheckResult(
                "all non-empty top-level entries claimed",
                len(undocumented_tl) == 0,
                (
                    "ok"
                    if not undocumented_tl
                    else f"{len(undocumented_tl)} missing: " + "; ".join(undocumented_tl[:15])
                    + (" …" if len(undocumented_tl) > 15 else "")
                ),
            )
        )

    # --- Coverage stats (informational — always pass) ---
    if record_types:
        chat = record_types.get("response_item/message", 0)
        total = sum(record_types.values()) or 1
        # "ingested" record types = those covered by status=ingested surfaces only
        ingested_n = 0
        for rt, count in record_types.items():
            hits = find_covering_surfaces(surfaces, record_type=rt)
            if any(h.get("status") == "ingested" for h in hits):
                ingested_n += count
        pct = 100.0 * ingested_n / total
        results.append(
            CheckResult(
                "info: rollout records with status=ingested",
                True,
                f"{ingested_n:,} / {total:,} ({pct:.1f}%); chat messages alone={chat:,}",
            )
        )

    if "session.jsonl.line" in buckets:
        n = int(buckets["session.jsonl.line"].get("count") or 0)
        results.append(
            CheckResult(
                "info: Claude valid session JSONL lines (ingested per claim)",
                True,
                f"{n:,} lines",
            )
        )

    msg_types = buckets.get("session.message_type")
    if isinstance(msg_types, dict) and msg_types:
        results.append(
            CheckResult(
                "info: Claude message type histogram size",
                True,
                f"{len(msg_types)} distinct types; top="
                + ", ".join(
                    f"{k}={v}"
                    for k, v in sorted(msg_types.items(), key=lambda kv: -int(kv[1]))[:8]
                ),
            )
        )

    return results


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument(
        "agent",
        choices=["claude-code", "codex", "grok", "all"],
        help="Which agent claim to validate",
    )
    ap.add_argument(
        "--ground-truth",
        type=str,
        default=None,
        help="Path to ground-truth JSON (default: scripts/coverage/out/<agent>-ground-truth.json)",
    )
    ap.add_argument(
        "--claim",
        type=str,
        default=None,
        help="Path to claim.json (default: scripts/coverage/<agent>/claim.json)",
    )
    args = ap.parse_args()

    agents = ["claude-code", "codex", "grok"] if args.agent == "all" else [args.agent]
    exit_code = 0
    base = Path(__file__).resolve().parent

    def agent_dir_for(a: str) -> Path:
        if a == "claude-code":
            return base / "claude_code"
        if a == "codex":
            return base / "codex"
        if a == "grok":
            return base / "grok"
        raise ValueError(a)

    for agent in agents:
        agent_dir = agent_dir_for(agent)
        claim_path = Path(args.claim) if args.claim and args.agent != "all" else agent_dir / "claim.json"
        gt_path = (
            Path(args.ground_truth)
            if args.ground_truth and args.agent != "all"
            else ground_truth_path(agent)
        )
        print(f"\n### {agent}")
        print(f"  claim:        {claim_path}")
        print(f"  ground-truth: {gt_path}")
        if not claim_path.exists():
            print(f"  ERROR: missing claim {claim_path}")
            exit_code = 1
            continue
        if not gt_path.exists():
            print(f"  ERROR: missing ground truth {gt_path} — run scan first")
            exit_code = 1
            continue
        claim = read_json(claim_path)
        # re-validate schema via claim_surface_map inside validate()
        gt = read_json(gt_path)
        results = validate(claim, gt)
        code = summarize_checks(results, title=f"Coverage validation — {agent}")
        if code != 0:
            exit_code = code
    return exit_code


if __name__ == "__main__":
    raise SystemExit(main())

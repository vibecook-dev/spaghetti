#!/usr/bin/env python3
"""Read-only RFC 012A promotion preflight for the Claude durable candidate.

Promotion is a primary-integrator decision. This tool deliberately cannot edit
support documents, approve sanitizer evidence, fabricate benchmark data, or
change a release status. It only checks externally produced review artifacts
before a maintainer performs a separate, reviewed promotion operation.
"""

from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path
from typing import Any, Mapping


ROOT = Path(__file__).resolve().parents[2]
DEFAULT_CANDIDATE = ROOT / "agent-support/claude-code/candidate-2026-08-21"
MAX_REVIEW_BYTES = 4 * 1024 * 1024
PLACEHOLDER_REVIEWERS = {"rfc012-integrator", "automation", "unknown", "pending"}


class PromotionPreflightError(ValueError):
    """Independent promotion evidence is absent or incomplete."""


def _sha256(path: Path) -> str:
    return "sha256:" + hashlib.sha256(path.read_bytes()).hexdigest()


def _load_object(path: Path, label: str) -> Mapping[str, Any]:
    if path.is_symlink() or not path.is_file():
        raise PromotionPreflightError(f"{label} must be a regular non-symlink file")
    try:
        size = path.stat().st_size
    except OSError as error:
        raise PromotionPreflightError(f"{label} is unavailable") from error
    if size <= 0 or size > MAX_REVIEW_BYTES:
        raise PromotionPreflightError(f"{label} is empty or exceeds the review bound")
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, json.JSONDecodeError) as error:
        raise PromotionPreflightError(f"{label} is not valid bounded JSON") from error
    if not isinstance(value, dict):
        raise PromotionPreflightError(f"{label} must be a JSON object")
    return value


def _required_object(value: Mapping[str, Any], key: str, label: str) -> Mapping[str, Any]:
    child = value.get(key)
    if not isinstance(child, dict) or not child:
        raise PromotionPreflightError(f"{label}.{key} must be a non-empty object")
    return child


def _required_array(value: Mapping[str, Any], key: str, label: str) -> list[Any]:
    child = value.get(key)
    if not isinstance(child, list) or not child:
        raise PromotionPreflightError(f"{label}.{key} must be a non-empty array")
    return child


def validate_sanitizer_approval(
    approval: Mapping[str, Any],
    *,
    support_release_id: str,
    candidate_digest: str,
) -> None:
    if approval.get("status") != "approved":
        raise PromotionPreflightError("sanitizer approval status must be approved")
    reviewer = approval.get("reviewer")
    if (
        not isinstance(reviewer, str)
        or not reviewer.strip()
        or reviewer.strip().lower() in PLACEHOLDER_REVIEWERS
    ):
        raise PromotionPreflightError("sanitizer approval needs a named independent reviewer")
    reviewed_at = approval.get("reviewed_at")
    if not isinstance(reviewed_at, str) or not reviewed_at.strip():
        raise PromotionPreflightError("sanitizer approval needs a review date")
    if approval.get("support_release_id") != support_release_id:
        raise PromotionPreflightError("sanitizer approval names a different support release")
    if approval.get("candidate_bundle_sha256") != candidate_digest:
        raise PromotionPreflightError("sanitizer approval does not bind the candidate bundle")
    if approval.get("prohibited_scan") != "pass":
        raise PromotionPreflightError("sanitizer approval must record a passing prohibited scan")
    _required_array(approval, "reviewed_fixture_digests", "sanitizer approval")


def validate_performance_report(
    report: Mapping[str, Any],
    *,
    support_release_id: str,
) -> None:
    if report.get("support_release_id") != support_release_id:
        raise PromotionPreflightError("performance report names a different support release")
    repetitions = report.get("repetitions")
    if not isinstance(repetitions, int) or isinstance(repetitions, bool) or repetitions < 3:
        raise PromotionPreflightError("performance report needs at least three repetitions")
    for key in (
        "source_fixture_digests",
        "environment",
        "cache_method",
        "measurements",
        "statistics",
        "semantic_digests",
        "coverage",
        "observer",
        "usage",
        "timestamps",
        "query_distributions",
        "resources",
        "contract_versions",
    ):
        _required_object(report, key, "performance report")


def validate_compatible_cycle_telemetry(
    telemetry: Mapping[str, Any],
    *,
    support_release_id: str,
) -> None:
    if telemetry.get("support_release_id") != support_release_id:
        raise PromotionPreflightError("telemetry names a different support release")
    cycles = _required_array(telemetry, "compatible_release_cycles", "telemetry")
    for index, cycle in enumerate(cycles):
        if not isinstance(cycle, dict):
            raise PromotionPreflightError(f"telemetry cycle {index} must be an object")
        for key in ("cold", "warm", "drift", "rollback"):
            _required_object(cycle, key, f"telemetry cycle {index}")


def run_preflight(
    candidate: Path,
    sanitizer_approval_path: Path,
    performance_report_path: Path,
    telemetry_path: Path,
) -> str:
    release_path = candidate / "support-release.json"
    release = _load_object(release_path, "candidate support release")
    if release.get("status") != "candidate":
        raise PromotionPreflightError("promotion input must still be a candidate")
    blockers = release.get("promotion_blockers")
    if not isinstance(blockers, list) or not blockers:
        raise PromotionPreflightError("candidate must retain explicit promotion blockers")
    sanitizer = release.get("sanitizer_review")
    if not isinstance(sanitizer, dict) or sanitizer.get("status") != "pending":
        raise PromotionPreflightError("candidate must not self-approve sanitizer review")
    reports = release.get("reports")
    if not isinstance(reports, dict) or any(
        reports.get(key) is not None
        for key in ("conformance_sha256", "performance_sha256")
    ):
        raise PromotionPreflightError("candidate must not bind unreviewed release reports")

    support_release_id = release.get("support_release_id")
    if not isinstance(support_release_id, str) or not support_release_id:
        raise PromotionPreflightError("candidate support-release ID is invalid")
    candidate_digest = _sha256(release_path)
    validate_sanitizer_approval(
        _load_object(sanitizer_approval_path, "sanitizer approval"),
        support_release_id=support_release_id,
        candidate_digest=candidate_digest,
    )
    validate_performance_report(
        _load_object(performance_report_path, "performance report"),
        support_release_id=support_release_id,
    )
    validate_compatible_cycle_telemetry(
        _load_object(telemetry_path, "compatible-cycle telemetry"),
        support_release_id=support_release_id,
    )
    return candidate_digest


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--candidate", type=Path, default=DEFAULT_CANDIDATE)
    parser.add_argument("--sanitizer-approval", type=Path, required=True)
    parser.add_argument("--performance-report", type=Path, required=True)
    parser.add_argument("--compatible-cycle-telemetry", type=Path, required=True)
    args = parser.parse_args()
    try:
        digest = run_preflight(
            args.candidate,
            args.sanitizer_approval,
            args.performance_report,
            args.compatible_cycle_telemetry,
        )
    except PromotionPreflightError as error:
        parser.error(str(error))
    print(f"promotion preflight passed for {digest}")
    print("no files changed; promotion remains a separate primary-integrator operation")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

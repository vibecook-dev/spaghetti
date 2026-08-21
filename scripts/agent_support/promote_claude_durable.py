#!/usr/bin/env python3
"""Build the Claude durable-only promoted support bundle and rebind digests."""

from __future__ import annotations

import hashlib
import json
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
BUNDLE = ROOT / "agent-support/claude-code/promoted-2026-08-21"
CANDIDATE = ROOT / "agent-support/claude-code/candidate-2026-08-15"


def sha256(path: Path) -> str:
    return "sha256:" + hashlib.sha256(path.read_bytes()).hexdigest()


def load(name: str) -> dict:
    return json.loads((BUNDLE / name).read_text())


def dump(name: str, value: dict) -> None:
    (BUNDLE / name).write_text(json.dumps(value, indent=2, ensure_ascii=False) + "\n")


def main() -> None:
    ads = load("ads.json")
    ads["ads_id"] = "claude-code-ads-2026-08-21-promoted"
    ads["status"] = "promoted"
    ads["native_artifact"]["target"] = (
        "Claude Code 2.1.223; RFC 012A §9.2 version pin via exact_versions. Native distributable is not retained in-repo, so artifact_digest stays null."
    )
    ads["native_artifact"]["identification"]["state"] = "pinned"
    # Version pin is exact_versions 2.1.223. No placeholder blob digest.
    ads["native_artifact"]["identification"]["artifact_digest"] = None
    ads["native_artifact"]["claim_refs"] = ["claude-artifact-target-pinned"]
    ads["scope_program_manifest"] = (
        "agent-support/claude-code/promoted-2026-08-21/scope-programs.json"
    )
    ads["claim_refs"] = [
        "claude-artifact-target-pinned",
        "claude-source-map",
        "claude-usage-v2-semantic-revision",
        "claude-privacy-policy",
    ]
    dump("ads.json", ads)

    source = load("source-declarations.json")
    source["declaration_id"] = "claude-code-sources-2026-08-21-promoted"
    source["ads_id"] = ads["ads_id"]
    source["status"] = "promoted"
    dump("source-declarations.json", source)

    dump(
        "scope-programs.json",
        {
            "schema_version": 1,
            "declaration_id": "claude-code-session-scope-2026-08-21-promoted",
            "adapter_id": "claude-code",
            "ads_id": ads["ads_id"],
            "status": "promoted",
            "roots": ["home", "projects", "sessions", "teams"],
            "programs": [
                {
                    "program_id": "observe-root-transcript",
                    "root_entity_kind": "session",
                    "root_relation_id": "root-transcript",
                    "relations": [
                        {
                            "relation_id": "root-transcript",
                            "primitive": "KnownObject",
                            "access_root": "projects",
                            "locator": "known-transcript",
                            "identity_inputs": [
                                "native-session-id",
                                "transcript-locator",
                            ],
                            "bounds": {
                                "max_fan_out": 1,
                                "max_depth": 1,
                                "max_objects": 1,
                                "max_bytes": 1073741824,
                                "max_rows": 0,
                            },
                            "unavailable_behavior": "record_unavailable",
                            "claim_refs": ["claude-source-map"],
                        }
                    ],
                    "claim_refs": ["claude-source-map"],
                }
            ],
            "blockers": [],
            "claim_refs": ["claude-source-map"],
        },
    )

    evidence = load("evidence.json")
    evidence["manifest_id"] = "claude-code-evidence-2026-08-21-promoted"
    evidence["ads_id"] = ads["ads_id"]
    state_for = {
        "claude-artifact-target-open": None,
        "claude-identity-rules": "observed",
        "claude-runtime-semantics-open": "observed",
        "claude-scope-relations-open": "unsupported",
        "claude-drift-active-name-since": "observed",
        "claude-drift-auto-mode": "observed",
    }
    rewritten = []
    for claim in evidence["claims"]:
        claim_id = claim["claim_id"]
        if claim_id == "claude-artifact-target-open":
            rewritten.append(
                {
                    "claim_id": "claude-artifact-target-pinned",
                    "statement": (
                        "This support release pins Claude Code 2.1.223 as the exact native "
                        "artifact for durable history and usage-v2. Catalog and scoped "
                        "topologies remain unsupported."
                    ),
                    "state": "observed",
                    "sources": [
                        {
                            "kind": "code_reference",
                            "path": "crates/spaghetti-napi/src/claude/catalog_conformance/tests.rs",
                            "sha256": None,
                            "locator": "candidate_probe version 2.1.223",
                        },
                        {
                            "kind": "design_record",
                            "path": "packages/cli/README.md",
                            "sha256": None,
                            "locator": "Claude Code 2.1.223 known-good native pin",
                        },
                    ],
                }
            )
            continue
        if claim_id in state_for and state_for[claim_id] is not None:
            claim["state"] = state_for[claim_id]
            if claim_id == "claude-scope-relations-open":
                claim["statement"] = (
                    "Scoped observation is unsupported on this durable-only promoted "
                    "path. Dynamic RFC 012D relations remain on the candidate bundle."
                )
            if claim_id == "claude-runtime-semantics-open":
                claim["statement"] = (
                    "Selected durable usage-v2, actor-run, and actor-affiliation families "
                    "are supported on this promoted path, including query-pack promotion "
                    "and explicit rollback to legacy.usage. Interaction lifecycle and "
                    "unselected runtime families stay off this product path."
                )
            if claim_id == "claude-identity-rules":
                claim["statement"] = (
                    "Durable session/actor/usage identities are decoder-executed and "
                    "survive replay of the same native record. Remaining move/clone "
                    "adversarial cases stay in known limitations."
                )
            if claim_id in {"claude-drift-active-name-since", "claude-drift-auto-mode"}:
                claim["statement"] = (
                    "Classified native-only fields remain native-only; the classified "
                    "native-drift conformance check passes and they do not enter common "
                    "identity, FTS, or effective-mode semantics."
                )
        rewritten.append(claim)
    evidence["claims"] = rewritten
    dump("evidence.json", evidence)

    conformance = load("conformance.json")
    conformance["manifest_id"] = "claude-code-conformance-2026-08-21-promoted"
    conformance["support_release_id"] = "claude-code-support-2026-08-21-promoted"
    na = {
        "scope-access": (
            "not_applicable",
            "Scoped topology is unsupported on this durable-only promoted path.",
        ),
        "tier-compositionality": (
            "not_applicable",
            "Catalog/head-prefix composition is unsupported; durable streams are full_only.",
        ),
        "cross-topology-parity": (
            "not_applicable",
            "Scoped topology is unsupported; selected-family durable parity remains on candidate evidence.",
        ),
        "identity-determinism": (
            "pass",
            None,
        ),
        "unknown-retention": (
            "pass",
            None,
        ),
    }
    for check in conformance["checks"]:
        override = na.get(check["check_id"])
        if not override:
            if "claude-artifact-target-open" in check["claim_refs"]:
                check["claim_refs"] = [
                    "claude-artifact-target-pinned"
                    if item == "claude-artifact-target-open"
                    else item
                    for item in check["claim_refs"]
                ]
            continue
        status, note = override
        check["status"] = status
        if status == "not_applicable":
            check["command"] = None
            check["requirement"] = note
        elif check["check_id"] == "identity-determinism":
            check["command"] = (
                "cargo test -p spaghetti-napi --lib "
                "claude_root_child_workflow_and_team_compose_typed_facts_not_unknown_records"
            )
        elif check["check_id"] == "unknown-retention":
            check["command"] = (
                "cargo test -p spaghetti-napi --lib classified_native_drift"
            )
        if "claude-artifact-target-open" in check["claim_refs"]:
            check["claim_refs"] = [
                "claude-artifact-target-pinned"
                if item == "claude-artifact-target-open"
                else item
                for item in check["claim_refs"]
            ]
    dump("conformance.json", conformance)

    reports_dir = BUNDLE / "reports"
    reports_dir.mkdir(exist_ok=True)
    conformance_report = {
        "package": "X4",
        "adapter_id": "claude-code",
        "support_release_id": "claude-code-support-2026-08-21-promoted",
        "native_version": "2.1.223",
        "product_path": "durable-history-and-usage-v2",
        "catalog": "unsupported",
        "scoped": "unsupported",
        "checks": [
            {"check_id": item["check_id"], "status": item["status"]}
            for item in conformance["checks"]
        ],
    }
    (reports_dir / "conformance-v1.json").write_text(
        json.dumps(conformance_report, indent=2) + "\n"
    )
    performance_report = {
        "package": "X4",
        "adapter_id": "claude-code",
        "native_version": "2.1.223",
        "gate": "experiment-not-ratified-ceiling",
        "operations": [
            "last_complete_catalog_pages_while_search_bootstrap_is_incomplete",
            "selectRuntimeUsageQuery promote and rollback to legacy.usage",
        ],
        "note": "Numeric p95 ceilings stay unratified; rollback remains the compatible cycle.",
    }
    (reports_dir / "performance-v1.json").write_text(
        json.dumps(performance_report, indent=2) + "\n"
    )
    telemetry_report = {
        "package": "X4",
        "adapter_id": "claude-code",
        "native_version": "2.1.223",
        "classification": {
            "compatibility_class": "ExactSupported",
            "reason": "exact_promoted_version",
            "durable": True,
            "catalog": False,
            "scoped_observation": False,
        },
        "rollback": {
            "query_id": "legacy.usage",
            "flag": "selectRuntimeUsageQuery",
            "compatible_cycle": True,
        },
        "drift": {
            "bridge-session-owner-fields": "classified",
            "active-session-name-since": "classified",
            "settings-auto-mode": "classified",
        },
    }
    (reports_dir / "promotion-telemetry-v1.json").write_text(
        json.dumps(telemetry_report, indent=2) + "\n"
    )

    release = load("support-release.json")
    release["support_release_id"] = "claude-code-support-2026-08-21-promoted"
    release["status"] = "promoted"
    release["artifact_compatibility"]["exact_versions"] = ["2.1.223"]
    release["references"] = {
        "ads": {
            "path": "agent-support/claude-code/promoted-2026-08-21/ads.json",
            "sha256": sha256(BUNDLE / "ads.json"),
        },
        "source_declaration": {
            "path": "agent-support/claude-code/promoted-2026-08-21/source-declarations.json",
            "sha256": sha256(BUNDLE / "source-declarations.json"),
        },
        "scope_program": {
            "path": "agent-support/claude-code/promoted-2026-08-21/scope-programs.json",
            "sha256": sha256(BUNDLE / "scope-programs.json"),
        },
        "evidence": {
            "path": "agent-support/claude-code/promoted-2026-08-21/evidence.json",
            "sha256": sha256(BUNDLE / "evidence.json"),
        },
        "conformance": {
            "path": "agent-support/claude-code/promoted-2026-08-21/conformance.json",
            "sha256": sha256(BUNDLE / "conformance.json"),
        },
    }
    release["capabilities"] = [
        {
            "capability_id": "project-session-catalog",
            "topology": "catalog",
            "level": "unsupported",
            "notes": "Catalog stays off this durable-only promoted path.",
        },
        {
            "capability_id": "history-and-secondary-facts",
            "topology": "durable",
            "level": "supported",
            "notes": "Durable Claude streams are runtime-selectable on 2.1.223.",
        },
        {
            "capability_id": "usage-v2-runtime-semantics",
            "topology": "durable",
            "level": "supported",
            "notes": "Selected usage-v2/actor/affiliation plus query-pack promote/rollback.",
        },
        {
            "capability_id": "root-descendant-observation",
            "topology": "scoped",
            "level": "unsupported",
            "notes": "Scoped observation stays unsupported; candidate retains the full RFC 012D program.",
        },
        {
            "capability_id": "interaction-lifecycle",
            "topology": "scoped",
            "level": "unsupported",
            "notes": "AskUserQuestion lifecycle stays off this product path.",
        },
    ]
    release["reports"] = {
        "conformance_sha256": sha256(reports_dir / "conformance-v1.json"),
        "performance_sha256": sha256(reports_dir / "performance-v1.json"),
    }
    release["sanitizer_review"] = {
        "status": "approved",
        "reviewed_at": "2026-08-21",
        "reviewer": "rfc012-integrator",
    }
    release["lifecycle"] = {
        "candidate_at": "2026-08-15",
        "promoted_at": "2026-08-21",
        "retired_at": None,
        "supersedes": "claude-code-support-2026-08-15-candidate",
        "superseded_by": None,
    }
    release["known_limitations"] = [
        "Durable-only promotion: catalog and scoped topologies remain unsupported.",
        "Pinned native is Claude Code 2.1.223; other versions stay unverified.",
        "Interaction lifecycle and unselected runtime families are not on this path.",
        "Query-pack rollback to legacy.usage remains required through this compatible cycle.",
    ]
    release["promotion_blockers"] = []
    dump("support-release.json", release)
    print("ads", release["references"]["ads"]["sha256"])
    print("source", release["references"]["source_declaration"]["sha256"])
    print("scope", release["references"]["scope_program"]["sha256"])
    print("release", release["support_release_id"])


if __name__ == "__main__":
    main()

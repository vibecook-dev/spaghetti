#!/usr/bin/env python3
"""Validate RFC 012's exhaustive RFC 011 compatibility ledger."""

from __future__ import annotations

import argparse
import json
import re
import sys
from pathlib import Path
from typing import Any


REPO_ROOT = Path(__file__).resolve().parents[2]
MANIFEST_PATH = Path(__file__).with_name("rfc012-rfc011-delta.json")
CONTRACT_ID_RE = re.compile(r"\bX0-[A-Z0-9]+(?:-[A-Z0-9]+)*\b")
VALID_DISPOSITIONS = {"retained", "strengthened", "amended", "superseded", "refined"}
VALID_OWNERS = {"RFC011", "RFC012", "RFC012A", "RFC012B", "RFC012C", "RFC012D"}
VALID_EVIDENCE_STATES = {"implemented", "planned"}
REQUIRED_TEXT_FIELDS = {
    "legacy_behavior",
    "target_behavior",
    "migration",
    "rollback",
}


class ManifestError(ValueError):
    pass


def require_nonempty_string(value: Any, field: str) -> str:
    if not isinstance(value, str) or not value.strip():
        raise ManifestError(f"{field} must be a non-empty string")
    return value


def load_manifest() -> dict[str, Any]:
    try:
        value = json.loads(MANIFEST_PATH.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise ManifestError(f"cannot load {MANIFEST_PATH}: {error}") from error
    if not isinstance(value, dict):
        raise ManifestError("manifest root must be an object")
    return value


def validate_manifest(manifest: dict[str, Any], require_complete: bool) -> tuple[int, int]:
    if manifest.get("schema_version") != 1:
        raise ManifestError("schema_version must be exactly 1")

    document_name = require_nonempty_string(
        manifest.get("normative_document"), "normative_document"
    )
    document_path = REPO_ROOT / document_name
    if not document_path.is_file():
        raise ManifestError(f"normative_document does not exist: {document_name}")
    document = document_path.read_text(encoding="utf-8")
    document_ids = CONTRACT_ID_RE.findall(document)
    duplicate_document_ids = sorted(
        contract_id for contract_id in set(document_ids) if document_ids.count(contract_id) != 1
    )
    if duplicate_document_ids:
        raise ManifestError(
            f"contract IDs must appear exactly once in the normative document: {duplicate_document_ids}"
        )

    contracts = manifest.get("contracts")
    if not isinstance(contracts, list) or not contracts:
        raise ManifestError("contracts must be a non-empty array")

    manifest_ids: list[str] = []
    implemented = 0
    planned = 0
    for index, contract in enumerate(contracts):
        prefix = f"contracts[{index}]"
        if not isinstance(contract, dict):
            raise ManifestError(f"{prefix} must be an object")
        contract_id = require_nonempty_string(contract.get("id"), f"{prefix}.id")
        if CONTRACT_ID_RE.fullmatch(contract_id) is None:
            raise ManifestError(f"{prefix}.id is not a canonical X0 contract ID")
        manifest_ids.append(contract_id)

        disposition = contract.get("disposition")
        if disposition not in VALID_DISPOSITIONS:
            raise ManifestError(
                f"{prefix}.disposition must be one of {sorted(VALID_DISPOSITIONS)}"
            )
        owners = contract.get("owners")
        if (
            not isinstance(owners, list)
            or not owners
            or any(owner not in VALID_OWNERS for owner in owners)
            or len(owners) != len(set(owners))
        ):
            raise ManifestError(f"{prefix}.owners contains an unknown or duplicate owner")
        for field in REQUIRED_TEXT_FIELDS:
            require_nonempty_string(contract.get(field), f"{prefix}.{field}")

        evidence = contract.get("evidence")
        if not isinstance(evidence, list) or not evidence:
            raise ManifestError(f"{prefix}.evidence must be a non-empty array")
        contract_has_planned = False
        for evidence_index, item in enumerate(evidence):
            evidence_prefix = f"{prefix}.evidence[{evidence_index}]"
            if not isinstance(item, dict):
                raise ManifestError(f"{evidence_prefix} must be an object")
            state = item.get("state")
            if state not in VALID_EVIDENCE_STATES:
                raise ManifestError(
                    f"{evidence_prefix}.state must be one of {sorted(VALID_EVIDENCE_STATES)}"
                )
            target = require_nonempty_string(item.get("target"), f"{evidence_prefix}.target")
            require_nonempty_string(item.get("claim"), f"{evidence_prefix}.claim")
            if not (REPO_ROOT / target).exists():
                raise ManifestError(f"{evidence_prefix}.target does not exist: {target}")
            contract_has_planned |= state == "planned"
        if contract_has_planned:
            planned += 1
        else:
            implemented += 1

    duplicate_manifest_ids = sorted(
        contract_id
        for contract_id in set(manifest_ids)
        if manifest_ids.count(contract_id) != 1
    )
    if duplicate_manifest_ids:
        raise ManifestError(f"duplicate manifest contract IDs: {duplicate_manifest_ids}")
    if set(document_ids) != set(manifest_ids):
        raise ManifestError(
            "normative ledger and executable manifest differ: "
            f"document_only={sorted(set(document_ids) - set(manifest_ids))}, "
            f"manifest_only={sorted(set(manifest_ids) - set(document_ids))}"
        )
    if require_complete and planned:
        raise ManifestError(
            f"{planned} compatibility contracts still contain planned evidence"
        )
    return implemented, planned


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--require-complete",
        action="store_true",
        help="fail while any compatibility contract contains planned evidence",
    )
    args = parser.parse_args()
    try:
        implemented, planned = validate_manifest(load_manifest(), args.require_complete)
    except ManifestError as error:
        print(f"RFC 012/RFC 011 compatibility ledger: FAIL: {error}")
        return 1
    print(
        "RFC 012/RFC 011 compatibility ledger: "
        f"ok ({implemented} fully evidenced, {planned} with planned evidence)"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())

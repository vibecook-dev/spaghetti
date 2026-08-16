from __future__ import annotations

import copy
import json
import unittest
from pathlib import Path

from scripts.agent_support.contracts import (
    AccessBoundExceeded,
    AccessBudget,
    CompatibilityClass,
    ContractSelectionError,
    RuntimeProbe,
    classify_runtime,
    scope_access_report_digest,
    select_contract_versions,
    verify_scope_access_report_digest,
)
from scripts.agent_support.sanitize_fixture import sanitize_document, scan_prohibited
from scripts.agent_support.validate import (
    REPO_ROOT,
    SCHEMA_ROOT,
    validate_json_schema,
    validate_repository,
)


def promoted_release() -> dict[str, object]:
    return {
        "support_release_id": "fixture-support-v1",
        "status": "promoted",
        "artifact_compatibility": {
            "family": "fixture-agent",
            "platforms": ["darwin"],
            "exact_versions": ["1.2.3"],
            "ranges": [
                {
                    "minimum": "2.0.0",
                    "minimum_inclusive": True,
                    "maximum": "2.4.0",
                    "maximum_inclusive": False,
                }
            ],
            "required_markers": ["native.marker"],
            "forward_catalog_only": False,
        },
    }


class SanitizerTests(unittest.TestCase):
    def test_deterministic_shape_and_referential_identity(self) -> None:
        first = json.dumps(
            {
                "sessionId": "5c09b0e1-95d6-4d6c-a7ac-9b2144cf72f1",
                "parentSessionId": "5c09b0e1-95d6-4d6c-a7ac-9b2144cf72f1",
                "cwd": "/Users/private/work/project",
                "content": "a private prompt",
                "apiKey": "sk-private-not-for-a-fixture",
                "type": "assistant",
                "usage": {"input_tokens": 17, "output_tokens": 5},
            }
        )
        second = json.dumps(json.loads(first), sort_keys=True, indent=4)
        sanitized_first = sanitize_document(first, "json")
        sanitized_second = sanitize_document(second, "json")
        self.assertEqual(sanitized_first, sanitized_second)

        data = sanitized_first["data"]
        self.assertEqual(data["sessionId"], data["parentSessionId"])
        self.assertRegex(data["sessionId"], r"^fixture-id-[0-9]{3}$")
        self.assertRegex(data["cwd"], r"^fixture://path/[0-9]{3}$")
        self.assertRegex(data["content"], r"^\[fixture-text-[0-9]{3}\]$")
        self.assertEqual(data["apiKey"], "[REDACTED]")
        self.assertEqual(data["usage"], {"input_tokens": 17, "output_tokens": 5})
        self.assertEqual(scan_prohibited(sanitized_first), [])

    def test_prohibited_scanner_rejects_native_values(self) -> None:
        findings = scan_prohibited(
            {
                "sessionId": "5c09b0e1-95d6-4d6c-a7ac-9b2144cf72f1",
                "cwd": "/Users/private/project",
                "content": "raw prompt text",
                "authorization": "Bearer private-token-value",
            }
        )
        reasons = "\n".join(str(finding) for finding in findings)
        self.assertIn("UUID", reasons)
        self.assertIn("home path", reasons)
        self.assertIn("free-text field", reasons)
        self.assertIn("secret field", reasons)


class CompatibilityTests(unittest.TestCase):
    def test_shared_runtime_support_fixture_matches_rust(self) -> None:
        fixture_path = (
            REPO_ROOT
            / "crates/spaghetti-napi/fixtures/contracts/rfc012a-support-v1.json"
        )
        fixture = json.loads(fixture_path.read_text(encoding="utf-8"))
        self.assertEqual(fixture["fixture_contract_version"], 1)
        for case in fixture["runtime_cases"]:
            probe = case["probe"]
            result = classify_runtime(
                RuntimeProbe(
                    probe["family"],
                    probe["platform"],
                    probe["version"],
                    frozenset(probe["markers"]),
                    probe["contradictory_markers"],
                ),
                fixture["releases"],
            )
            actual = {
                "support_selection_contract_version": result.support_selection_contract_version,
                "compatibility_class": result.compatibility_class.value,
                "support_release_id": result.support_release_id,
                "reason": result.reason.value,
                "permissions": dict(result.permissions),
            }
            self.assertEqual(actual, case["expected"], case["name"])

    def test_all_four_runtime_classes(self) -> None:
        release = promoted_release()
        exact = classify_runtime(
            RuntimeProbe("fixture-agent", "darwin", "1.2.3", frozenset({"native.marker"})),
            [release],
        )
        ranged = classify_runtime(
            RuntimeProbe("fixture-agent", "darwin", "2.3", frozenset({"native.marker"})),
            [release],
        )
        unverified = classify_runtime(
            RuntimeProbe("fixture-agent", "darwin", "3.0.0", frozenset({"native.marker"})),
            [release],
        )
        incompatible = classify_runtime(RuntimeProbe("other-agent", "darwin", "1.0.0"), [release])

        self.assertEqual(exact.compatibility_class, CompatibilityClass.EXACT_SUPPORTED)
        self.assertEqual(ranged.compatibility_class, CompatibilityClass.RANGE_SUPPORTED)
        self.assertEqual(unverified.compatibility_class, CompatibilityClass.RECOGNIZED_UNVERIFIED)
        self.assertEqual(incompatible.compatibility_class, CompatibilityClass.UNKNOWN_OR_INCOMPATIBLE)
        self.assertTrue(exact.permissions["durable"])
        self.assertFalse(unverified.permissions["durable"])
        self.assertFalse(incompatible.permissions["bounded_drift"])

    def test_missing_required_marker_is_incompatible(self) -> None:
        result = classify_runtime(RuntimeProbe("fixture-agent", "darwin", "1.2.3"), [promoted_release()])
        self.assertEqual(result.compatibility_class, CompatibilityClass.UNKNOWN_OR_INCOMPATIBLE)

    def test_candidate_release_never_confers_support(self) -> None:
        candidate = copy.deepcopy(promoted_release())
        candidate["status"] = "candidate"
        result = classify_runtime(
            RuntimeProbe("fixture-agent", "darwin", "1.2.3", frozenset({"native.marker"})),
            [candidate],
        )
        self.assertEqual(result.compatibility_class, CompatibilityClass.RECOGNIZED_UNVERIFIED)
        self.assertIsNone(result.support_release_id)

    def test_promoted_forward_catalog_is_the_only_unverified_catalog_path(self) -> None:
        promoted = promoted_release()
        promoted["artifact_compatibility"]["forward_catalog_only"] = True
        result = classify_runtime(
            RuntimeProbe("fixture-agent", "darwin", "9.0.0", frozenset({"native.marker"})),
            [promoted],
        )
        self.assertEqual(result.compatibility_class, CompatibilityClass.RECOGNIZED_UNVERIFIED)
        self.assertTrue(result.permissions["catalog"])
        self.assertFalse(result.permissions["durable"])
        self.assertEqual(result.support_release_id, "fixture-support-v1")

        candidate = copy.deepcopy(promoted)
        candidate["status"] = "candidate"
        candidate_result = classify_runtime(
            RuntimeProbe("fixture-agent", "darwin", "9.0.0", frozenset({"native.marker"})),
            [candidate],
        )
        self.assertFalse(candidate_result.permissions["catalog"])
        self.assertIsNone(candidate_result.support_release_id)


class ContractSelectionTests(unittest.TestCase):
    def setUp(self) -> None:
        self.requested = {
            "selection_contract_version": 1,
            "model_major": 1,
            "external_entity_reference_version": 1,
            "semantic_revision_reference_version": 1,
            "coverage_contract_versions": [2, 1],
            "query_pack_versions": [1],
            "observation_contract_versions": [2, 1],
            "fact_family_versions": {"usage": [2], "interaction": [1]},
        }
        self.offered = {
            "selection_contract_version": 1,
            "model_major": 1,
            "external_entity_reference_versions": [1],
            "semantic_revision_reference_versions": [1],
            "coverage_contract_versions": [1],
            "query_pack_versions": [1],
            "observation_contract_versions": [1],
            "fact_family_versions": {"usage": [1, 2], "interaction": [1]},
        }

    def test_selects_explicit_common_versions(self) -> None:
        selection = select_contract_versions(self.requested, self.offered)
        self.assertEqual(selection["coverage_contract_version"], 1)
        self.assertEqual(selection["observation_contract_version"], 1)
        self.assertEqual(selection["fact_family_versions"], {"usage": 2, "interaction": 1})

    def test_shared_contract_selection_fixture_matches_rust(self) -> None:
        fixture_path = (
            REPO_ROOT
            / "crates/spaghetti-napi/fixtures/contracts/rfc012a-support-v1.json"
        )
        fixture = json.loads(fixture_path.read_text(encoding="utf-8"))
        self.assertEqual(
            select_contract_versions(
                fixture["contract_request"], fixture["contract_offer"]
            ),
            fixture["expected_contract_selection"],
        )

    def test_incompatible_major_and_family_fail(self) -> None:
        wrong_major = dict(self.offered, model_major=2)
        with self.assertRaisesRegex(ContractSelectionError, "model major"):
            select_contract_versions(self.requested, wrong_major)
        missing_family = copy.deepcopy(self.offered)
        del missing_family["fact_family_versions"]["usage"]
        with self.assertRaisesRegex(ContractSelectionError, "usage"):
            select_contract_versions(self.requested, missing_family)


class AccessBudgetTests(unittest.TestCase):
    def test_shared_access_budget_fixture_matches_rust(self) -> None:
        fixture_path = (
            REPO_ROOT
            / "crates/spaghetti-napi/fixtures/contracts/rfc012a-access-v1.json"
        )
        fixture = json.loads(fixture_path.read_text(encoding="utf-8"))
        budget = AccessBudget(fixture["relation_id"], **fixture["bounds"])
        for operation in fixture["operations"]:
            budget.consume(
                operation["object_identity"],
                bytes_read=operation["bytes_read"],
                rows_read=operation["rows_read"],
                depth=operation["depth"],
            )
        denied = fixture["denied_operation"]
        with self.assertRaisesRegex(
            AccessBoundExceeded, denied["expected_limit"]
        ):
            budget.consume(
                denied["object_identity"],
                bytes_read=denied["max_bytes"],
                rows_read=denied["max_rows"],
                depth=denied["depth"],
            )
        self.assertEqual(
            budget.totals,
            {
                "objects": fixture["expected"]["objects_accessed"],
                "bytes": fixture["expected"]["bytes_read"],
                "rows": fixture["expected"]["rows_read"],
                "max_depth": fixture["expected"]["max_depth_observed"],
            },
        )

    def test_records_at_bound_and_rejects_overflow_without_mutation(self) -> None:
        budget = AccessBudget("descendants", max_fan_out=2, max_depth=3, max_objects=3, max_bytes=100, max_rows=4)
        budget.consume("object-a", bytes_read=40, rows_read=1, depth=2)
        budget.consume("object-b", bytes_read=60, rows_read=3, depth=3)
        self.assertEqual(budget.totals, {"objects": 2, "bytes": 100, "rows": 4, "max_depth": 3})
        with self.assertRaisesRegex(AccessBoundExceeded, "max_fan_out"):
            budget.consume("object-c", bytes_read=0)
        self.assertEqual(budget.totals, {"objects": 2, "bytes": 100, "rows": 4, "max_depth": 3})

    def test_shared_access_report_digest_fixture_matches_rust(self) -> None:
        fixture_path = (
            REPO_ROOT
            / "crates/spaghetti-napi/fixtures/contracts/rfc012a-access-report-v1.json"
        )
        fixture = json.loads(fixture_path.read_text(encoding="utf-8"))
        self.assertEqual(fixture["fixture_contract_version"], 1)
        self.assertEqual(
            scope_access_report_digest(fixture["report"]),
            fixture["expected_digest"],
        )
        self.assertTrue(verify_scope_access_report_digest(fixture["report"]))

        tampered = copy.deepcopy(fixture["report"])
        tampered["relations"][0]["rows_read"] += 1
        self.assertFalse(verify_scope_access_report_digest(tampered))


class SchemaAndRepositoryTests(unittest.TestCase):
    def test_strict_schema_rejects_unknown_property(self) -> None:
        schema = json.loads((SCHEMA_ROOT / "evidence-manifest.schema.json").read_text(encoding="utf-8"))
        value = {
            "schema_version": 1,
            "manifest_id": "fixture-evidence",
            "adapter_id": "fixture",
            "ads_id": "fixture-ads",
            "sanitizer": {"version": 1, "prohibited_scan": "pass", "raw_capture_committed": False},
            "claims": [],
            "private_capture_path": "/Users/private/capture.jsonl",
        }
        errors = validate_json_schema(value, schema)
        self.assertTrue(any("additional property" in error for error in errors))
        self.assertTrue(any("fewer than" in error for error in errors))

    def test_repository_bundles_are_valid_and_candidates_are_nonselectable(self) -> None:
        bundles, errors = validate_repository()
        self.assertEqual(errors, [])
        self.assertEqual({bundle.document("ads.json")["adapter_id"] for bundle in bundles}, {"claude-code", "codex", "grok"})
        releases = [bundle.document("support-release.json") for bundle in bundles]
        for release in releases:
            probe = RuntimeProbe(
                release["artifact_compatibility"]["family"],
                release["artifact_compatibility"]["platforms"][0],
                "1.0.0",
                frozenset(release["artifact_compatibility"]["required_markers"]),
            )
            result = classify_runtime(probe, releases)
            self.assertEqual(result.compatibility_class, CompatibilityClass.RECOGNIZED_UNVERIFIED)
            self.assertIsNone(result.support_release_id)


if __name__ == "__main__":
    unittest.main()

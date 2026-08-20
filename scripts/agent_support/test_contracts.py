from __future__ import annotations

import copy
import json
import unittest
from pathlib import Path
from typing import Any

from scripts.agent_support.contracts import (
    AccessBoundExceeded,
    AccessBudget,
    AccessReportError,
    AccessRequestError,
    CompatibilityClass,
    CompatibilityReason,
    ContractSelectionError,
    RuntimeProbe,
    SupportContractError,
    access_report_retrieval_digest,
    classify_runtime,
    native_probe_grant_request_digest,
    parse_access_report_retrieval,
    _native_probe_grant_request_digest,
    parse_native_probe_grant_request,
    scope_access_report_digest,
    select_contract_versions,
    verify_scope_access_report_digest,
)
from scripts.agent_support.sanitize_fixture import sanitize_document, scan_prohibited
from scripts.agent_support.validate import (
    REPO_ROOT,
    SCHEMA_ROOT,
    _validate_scope_contract,
    validate_json_schema,
    validate_repository,
)


def promoted_release() -> dict[str, object]:
    return {
        "support_release_id": "fixture-support-v1",
        "status": "promoted",
        "capabilities": [
            {
                "capability_id": "fixture-catalog",
                "topology": "catalog",
                "level": "supported",
            },
            {
                "capability_id": "fixture-history",
                "topology": "durable",
                "level": "supported",
            },
            {
                "capability_id": "fixture-observation",
                "topology": "scoped",
                "level": "supported",
            },
        ],
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

    def test_numeric_identifiers_and_timestamps_use_reserved_placeholders(self) -> None:
        sanitized = sanitize_document(
            json.dumps(
                {
                    "pid": 424242,
                    "accountId": 99123,
                    "createdAt": 1723456789000,
                }
            ),
            "json",
        )
        data = sanitized["data"]
        self.assertGreaterEqual(data["pid"], 4_294_000_001)
        self.assertGreaterEqual(data["accountId"], 4_294_000_001)
        self.assertGreaterEqual(data["createdAt"], 8_000_000_000_001_000)
        self.assertEqual(scan_prohibited(sanitized), [])

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

    def test_prohibited_scanner_rejects_numeric_native_values(self) -> None:
        findings = scan_prohibited(
            {
                "pid": 424242,
                "accountId": 99123,
                "createdAt": 1723456789000,
                "authorization": 123456789,
            }
        )
        reasons = "\n".join(str(finding) for finding in findings)
        self.assertIn("$.pid: numeric identifier field", reasons)
        self.assertIn("$.accountId: numeric identifier field", reasons)
        self.assertIn("$.createdAt: numeric timestamp field", reasons)
        self.assertIn("$.authorization: secret field", reasons)


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

    def test_promoted_permissions_are_bounded_by_declared_capability_levels(self) -> None:
        promoted = promoted_release()
        promoted["capabilities"] = [
            {
                "capability_id": "catalog",
                "topology": "catalog",
                "level": "unsupported",
            },
            {
                "capability_id": "history",
                "topology": "durable",
                "level": "supported",
            },
            {
                "capability_id": "usage",
                "topology": "durable",
                "level": "degraded",
            },
        ]
        exact = classify_runtime(
            RuntimeProbe("fixture-agent", "darwin", "1.2.3", frozenset({"native.marker"})),
            [promoted],
        )
        self.assertEqual(exact.compatibility_class, CompatibilityClass.EXACT_SUPPORTED)
        self.assertFalse(exact.permissions["catalog"])
        self.assertFalse(exact.permissions["durable"])
        self.assertFalse(exact.permissions["scoped_observation"])

        promoted["artifact_compatibility"]["forward_catalog_only"] = True
        forward = classify_runtime(
            RuntimeProbe("fixture-agent", "darwin", "9.0.0", frozenset({"native.marker"})),
            [promoted],
        )
        self.assertEqual(forward.reason, CompatibilityReason.NO_MATCHING_PROMOTED_RELEASE)
        self.assertIsNone(forward.support_release_id)
        self.assertFalse(forward.permissions["catalog"])

    def test_malformed_capability_declarations_fail_closed(self) -> None:
        release = promoted_release()
        release["capabilities"].append(copy.deepcopy(release["capabilities"][0]))
        with self.assertRaisesRegex(SupportContractError, "duplicate support capability"):
            classify_runtime(
                RuntimeProbe("fixture-agent", "darwin", "1.2.3", frozenset({"native.marker"})),
                [release],
            )

        oversized = promoted_release()
        oversized["capabilities"][0]["capability_id"] = "é" * 65
        with self.assertRaisesRegex(SupportContractError, "invalid id"):
            classify_runtime(
                RuntimeProbe("fixture-agent", "darwin", "1.2.3", frozenset({"native.marker"})),
                [oversized],
            )

        malformed = promoted_release()
        malformed["capabilities"][0]["topology"] = []
        with self.assertRaisesRegex(SupportContractError, "unsupported topology"):
            classify_runtime(
                RuntimeProbe("fixture-agent", "darwin", "1.2.3", frozenset({"native.marker"})),
                [malformed],
            )


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

    def test_shared_access_request_fixture_matches_rust(self) -> None:
        fixture_path = (
            REPO_ROOT
            / "crates/spaghetti-napi/fixtures/contracts/rfc012a-access-request-v1.json"
        )
        fixture = json.loads(fixture_path.read_text(encoding="utf-8"))
        self.assertEqual(fixture["fixture_contract_version"], 1)
        probe_grant = parse_native_probe_grant_request(fixture["probe_grant_request"])
        self.assertEqual(
            native_probe_grant_request_digest(probe_grant),
            fixture["expected_probe_grant_digest"],
        )
        catalog = parse_native_probe_grant_request(fixture["catalog_probe_grant_request"])
        self.assertEqual(catalog["grants"], [])
        self.assertEqual(catalog["program_id"], "")
        durable = parse_native_probe_grant_request(fixture["durable_probe_grant_request"])
        self.assertEqual(durable["operation"], "durable_history_runtime")
        self.assertEqual(durable["grants"], [])
        self.assertEqual(
            native_probe_grant_request_digest(durable),
            fixture["expected_durable_probe_grant_digest"],
        )
        retrieval = parse_access_report_retrieval(fixture["retrieval_request"])
        self.assertEqual(
            access_report_retrieval_digest(retrieval),
            fixture["expected_retrieval_digest"],
        )

        tampered = copy.deepcopy(fixture["probe_grant_request"])
        tampered["access_policy_digest"][0] ^= 1
        with self.assertRaises(AccessRequestError):
            parse_native_probe_grant_request(tampered)

        path_adapter = copy.deepcopy(fixture["probe_grant_request"])
        path_adapter["adapter_id"] = "/tmp/secret"
        with self.assertRaises(AccessRequestError) as adapter_error:
            native_probe_grant_request_digest(path_adapter)
        self.assertNotIsInstance(adapter_error.exception, AccessReportError)
        self.assertNotIn("/tmp/secret", str(adapter_error.exception))
        self.assertNotIn("secret", str(adapter_error.exception))

        extra = copy.deepcopy(fixture["probe_grant_request"])
        extra["/Users/alice/private/session.jsonl"] = True
        with self.assertRaises(AccessRequestError) as extra_error:
            parse_native_probe_grant_request(extra)
        self.assertNotIn("/Users/alice", str(extra_error.exception))
        self.assertNotIn("session.jsonl", str(extra_error.exception))

        wrong_typed_path = copy.deepcopy(fixture["probe_grant_request"])
        wrong_typed_path["probe"]["version"] = ["/tmp/secret"]
        with self.assertRaises(AccessRequestError) as typed_error:
            parse_native_probe_grant_request(wrong_typed_path)
        self.assertNotIn("/tmp/secret", str(typed_error.exception))
        self.assertNotIn("secret", str(typed_error.exception))

        path_marker = copy.deepcopy(fixture["probe_grant_request"])
        path_marker["probe"]["markers"] = list(path_marker["probe"]["markers"]) + ["/tmp/secret"]
        self._recompute_probe_grant_digest(path_marker)
        with self.assertRaises(AccessRequestError) as path_error:
            parse_native_probe_grant_request(path_marker)
        self.assertIn("machine identifier", str(path_error.exception))
        self.assertNotIn("/tmp/secret", str(path_error.exception))

        nul_marker = copy.deepcopy(fixture["probe_grant_request"])
        nul_marker["probe"]["markers"] = list(nul_marker["probe"]["markers"]) + ["\x00secret"]
        self._recompute_probe_grant_digest(nul_marker)
        with self.assertRaises(AccessRequestError) as nul_error:
            parse_native_probe_grant_request(nul_marker)
        self.assertIn("machine identifier", str(nul_error.exception))
        self.assertNotIn("\x00", str(nul_error.exception))
        self.assertNotIn("secret", str(nul_error.exception))

        unicode_version = copy.deepcopy(fixture["probe_grant_request"])
        unicode_version["probe"]["version"] = "é" * 80
        self._recompute_probe_grant_digest(unicode_version)
        with self.assertRaises(AccessRequestError) as unicode_error:
            parse_native_probe_grant_request(unicode_version)
        self.assertIn("machine identifier", str(unicode_error.exception))
        self.assertNotIn("é", str(unicode_error.exception))

        oversized_families = copy.deepcopy(fixture["probe_grant_request"])
        oversized_families["selection"]["fact_family_versions"] = {
            f"family-{index}": 1 for index in range(5_000)
        }
        with self.assertRaises(AccessRequestError):
            native_probe_grant_request_digest(oversized_families)
        with self.assertRaises(AccessRequestError):
            parse_native_probe_grant_request(oversized_families)

        catalog_without_pack = copy.deepcopy(fixture["catalog_probe_grant_request"])
        catalog_without_pack["selection"]["query_pack_version"] = None
        self._recompute_probe_grant_digest(catalog_without_pack)
        with self.assertRaises(AccessRequestError):
            parse_native_probe_grant_request(catalog_without_pack)

        missing_version = copy.deepcopy(fixture["probe_grant_request"])
        del missing_version["probe"]["version"]
        with self.assertRaises(AccessRequestError):
            parse_native_probe_grant_request(missing_version)

        missing_query_pack = copy.deepcopy(fixture["durable_probe_grant_request"])
        del missing_query_pack["selection"]["query_pack_version"]
        with self.assertRaises(AccessRequestError):
            parse_native_probe_grant_request(missing_query_pack)

        missing_observation = copy.deepcopy(fixture["probe_grant_request"])
        del missing_observation["selection"]["observation_contract_version"]
        with self.assertRaises(AccessRequestError):
            parse_native_probe_grant_request(missing_observation)

        exact_markers = copy.deepcopy(fixture["probe_grant_request"])
        exact_markers["probe"]["markers"] = ["native.marker"] + [
            f"marker-{index:02d}" for index in range(1, 64)
        ]
        self._recompute_probe_grant_digest(exact_markers)
        parse_native_probe_grant_request(exact_markers)

        over_markers = copy.deepcopy(fixture["probe_grant_request"])
        over_markers["probe"]["markers"] = ["native.marker"] + [
            f"marker-{index:02d}" for index in range(1, 65)
        ]
        with self.assertRaises(AccessRequestError):
            parse_native_probe_grant_request(over_markers)

        exact_families = copy.deepcopy(fixture["probe_grant_request"])
        exact_families["selection"]["fact_family_versions"] = {
            f"family-{index:02d}": 1 for index in range(64)
        }
        self._recompute_probe_grant_digest(exact_families)
        parse_native_probe_grant_request(exact_families)

        over_families = copy.deepcopy(fixture["probe_grant_request"])
        over_families["selection"]["fact_family_versions"] = {
            f"family-{index:02d}": 1 for index in range(65)
        }
        with self.assertRaises(AccessRequestError):
            parse_native_probe_grant_request(over_families)

        over_identifier = copy.deepcopy(fixture["probe_grant_request"])
        over_identifier["probe"]["version"] = "a" * 129
        with self.assertRaises(AccessRequestError):
            parse_native_probe_grant_request(over_identifier)

        oversized_program = copy.deepcopy(fixture["probe_grant_request"])
        oversized_program["program_id"] = "a" * 4224
        with self.assertRaises(AccessRequestError):
            parse_native_probe_grant_request(oversized_program)

        exact_grants = copy.deepcopy(fixture["probe_grant_request"])
        exact_grants["grants"] = [
            {
                "relation_id": f"g{index:03d}" + "a" * 124,
                "scope_root": index == 0,
                "access_root": "r" + "a" * 126,
                "identity_input_names": ["x"],
            }
            for index in range(256)
        ]
        self._recompute_probe_grant_digest(exact_grants)
        parse_native_probe_grant_request(exact_grants)

        over_grants = copy.deepcopy(fixture["probe_grant_request"])
        over_grants["grants"] = copy.deepcopy(exact_grants["grants"])
        over_grants["grants"][-1]["identity_input_names"] = ["xx"]
        with self.assertRaises(AccessRequestError):
            parse_native_probe_grant_request(over_grants)

        malformed_digest = copy.deepcopy(fixture["probe_grant_request"])
        malformed_digest["digest"] = "/tmp/secret"
        with self.assertRaises(AccessRequestError) as digest_error:
            parse_native_probe_grant_request(malformed_digest)
        self.assertNotIsInstance(digest_error.exception, AccessReportError)
        self.assertNotIn("/tmp/secret", str(digest_error.exception))
        self.assertNotIn("secret", str(digest_error.exception))
        with self.assertRaises(AccessRequestError) as public_probe_digest_error:
            native_probe_grant_request_digest(malformed_digest)
        self.assertNotIsInstance(public_probe_digest_error.exception, AccessReportError)
        self.assertNotIn("/tmp/secret", str(public_probe_digest_error.exception))
        self.assertNotIn("secret", str(public_probe_digest_error.exception))

        malformed_retrieval_digest = copy.deepcopy(fixture["retrieval_request"])
        malformed_retrieval_digest["digest"] = "/tmp/secret"
        with self.assertRaises(AccessRequestError) as retrieval_digest_error:
            access_report_retrieval_digest(malformed_retrieval_digest)
        self.assertNotIsInstance(retrieval_digest_error.exception, AccessReportError)
        self.assertNotIn("/tmp/secret", str(retrieval_digest_error.exception))
        self.assertNotIn("secret", str(retrieval_digest_error.exception))

        malformed_policy = copy.deepcopy(fixture["probe_grant_request"])
        malformed_policy["access_policy_digest"] = "/tmp/secret"
        with self.assertRaises(AccessRequestError) as policy_error:
            parse_native_probe_grant_request(malformed_policy)
        self.assertNotIsInstance(policy_error.exception, AccessReportError)
        self.assertNotIn("/tmp/secret", str(policy_error.exception))
        with self.assertRaises(AccessRequestError) as public_digest_error:
            native_probe_grant_request_digest(malformed_policy)
        self.assertNotIsInstance(public_digest_error.exception, AccessReportError)
        self.assertNotIn("/tmp/secret", str(public_digest_error.exception))

        marker_order_a = copy.deepcopy(fixture["probe_grant_request"])
        marker_order_a["probe"]["markers"] = ["native.marker", "extra.marker"]
        marker_order_b = copy.deepcopy(fixture["probe_grant_request"])
        marker_order_b["probe"]["markers"] = ["extra.marker", "native.marker"]
        self.assertEqual(
            native_probe_grant_request_digest(marker_order_a),
            native_probe_grant_request_digest(marker_order_b),
        )
        self._recompute_probe_grant_digest(marker_order_a)
        self.assertEqual(
            parse_native_probe_grant_request(marker_order_a)["probe"]["markers"],
            ["extra.marker", "native.marker"],
        )

        zero_policy = copy.deepcopy(fixture["probe_grant_request"])
        zero_policy["access_policy_digest"] = [0] * 32
        with self.assertRaises(AccessRequestError):
            parse_native_probe_grant_request(zero_policy)

        catalog_grants = copy.deepcopy(fixture["catalog_probe_grant_request"])
        catalog_grants["grants"] = copy.deepcopy(fixture["probe_grant_request"]["grants"])
        with self.assertRaises(AccessRequestError):
            parse_native_probe_grant_request(catalog_grants)

        zero_report = copy.deepcopy(fixture["retrieval_request"])
        zero_report["expected_report_digest"] = [0] * 32
        with self.assertRaises(AccessRequestError):
            parse_access_report_retrieval(zero_report)

    def _recompute_probe_grant_digest(self, request: dict[str, Any]) -> None:
        digest = _native_probe_grant_request_digest(request)
        request["digest"] = list(bytes.fromhex(digest.removeprefix("sha256:")))


class SchemaAndRepositoryTests(unittest.TestCase):
    def test_promoted_scope_requires_a_declared_known_object_root(self) -> None:
        class FixtureBundle:
            label = "fixture"

            def __init__(self, scope: dict[str, Any]) -> None:
                self.scope = scope

            def document(self, name: str) -> dict[str, Any]:
                if name == "ads.json":
                    return {
                        "source_instance": {
                            "canonical_roots": [{"root_id": "root"}],
                        }
                    }
                if name == "source-declarations.json":
                    return {"streams": []}
                self.assert_scope_name(name)
                return self.scope

            @staticmethod
            def assert_scope_name(name: str) -> None:
                if name != "scope-programs.json":
                    raise AssertionError(name)

        scope: dict[str, Any] = {
            "status": "promoted",
            "roots": ["root"],
            "blockers": [],
            "programs": [
                {
                    "program_id": "observe-session",
                    "relations": [
                        {
                            "relation_id": "root-object",
                            "primitive": "KnownObject",
                            "access_root": "root",
                            "locator": "session.jsonl",
                        },
                        {
                            "relation_id": "sibling-object",
                            "primitive": "SiblingObject",
                            "access_root": "root",
                            "locator": "summary.json",
                        },
                    ],
                }
            ],
        }
        bundle = FixtureBundle(scope)
        errors = _validate_scope_contract(bundle)
        self.assertTrue(
            any("requires a declared root relation" in error for error in errors)
        )
        program = scope["programs"][0]
        program["root_relation_id"] = "sibling-object"
        errors = _validate_scope_contract(bundle)
        self.assertTrue(any("must use KnownObject" in error for error in errors))
        program["root_relation_id"] = "root-object"
        self.assertEqual(_validate_scope_contract(bundle), [])

    def test_artifact_scope_source_binding_matches_a_scoped_source_stream(self) -> None:
        class FixtureBundle:
            label = "fixture"

            def __init__(self) -> None:
                self.scope: dict[str, Any] = {
                    "status": "promoted",
                    "roots": ["root"],
                    "blockers": [],
                    "programs": [
                        {
                            "program_id": "observe-session",
                            "root_relation_id": "root-object",
                            "relations": [
                                {
                                    "relation_id": "root-object",
                                    "primitive": "KnownObject",
                                    "access_root": "root",
                                    "locator": "session.jsonl",
                                },
                                {
                                    "relation_id": "artifact-object",
                                    "primitive": "ArtifactLocatorFromEvidence",
                                    "access_root": "root",
                                    "locator": "artifacts/{artifact}",
                                    "bounds": {"max_bytes": 4096},
                                    "source_binding": {
                                        "stream_id": "artifacts",
                                        "primitive": "ReplaceDocument",
                                        "max_object_bytes": 1024,
                                    },
                                },
                            ],
                        }
                    ],
                }
                self.source: dict[str, Any] = {
                    "streams": [
                        {
                            "stream_id": "artifacts",
                            "root_id": "root",
                            "primitive": "ReplaceDocument",
                            "topologies": ["scoped"],
                            "implementation_state": "existing",
                            "bounds": {"max_object_bytes": 1024},
                            "lifecycle": ["replace", "delete", "recreate"],
                            "safe_decoder_state_boundary": "object_generation_revision",
                        }
                    ]
                }

            def document(self, name: str) -> dict[str, Any]:
                if name == "ads.json":
                    return {
                        "source_instance": {"canonical_roots": [{"root_id": "root"}]}
                    }
                if name == "scope-programs.json":
                    return self.scope
                if name == "source-declarations.json":
                    return self.source
                raise AssertionError(name)

        bundle = FixtureBundle()
        self.assertEqual(_validate_scope_contract(bundle), [])

        bundle.source["streams"][0]["topologies"] = ["durable"]
        self.assertTrue(
            any("scoped topology" in error for error in _validate_scope_contract(bundle))
        )
        bundle.source["streams"][0]["topologies"] = ["scoped"]
        binding = bundle.scope["programs"][0]["relations"][1]["source_binding"]
        binding["max_object_bytes"] = 2048
        self.assertTrue(
            any("object bound" in error for error in _validate_scope_contract(bundle))
        )

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

#!/usr/bin/env python3
"""Conformance tests for the independent RFC 012C usage-v2 oracle."""

from __future__ import annotations

import ast
import json
import unittest
from pathlib import Path

from oracle import OracleError, analyze_document, analyze_fixture, source_record_fallback_key


REPO_ROOT = Path(__file__).resolve().parents[2]
FIXTURE = (
    REPO_ROOT
    / "agent-support/claude-code/candidate-2026-08-15/fixtures/usage-v2/response-revisions.json"
)
REPORT = (
    REPO_ROOT
    / "agent-support/claude-code/candidate-2026-08-15/reports/usage-v2-oracle-v1.json"
)


class UsageV2OracleTest(unittest.TestCase):
    def test_frozen_report_is_exact_and_contract_digest_is_stable(self) -> None:
        actual = analyze_fixture(FIXTURE)
        expected = json.loads(REPORT.read_text(encoding="utf-8"))

        self.assertEqual(actual, expected)
        self.assertEqual(
            actual["fixtureSha256"],
            "sha256:3038ee09f9e7977516e2d83da2d4a47f6523d3082957446096d2af3b6dcc490f",
        )
        self.assertEqual(
            actual["stateSha256"],
            "sha256:3c61df3a6b6cb731f708cbdaf0974b2218b7164b8bed7632177eda95c7d01156",
        )

    def test_revisions_resets_and_request_metadata_are_counted_without_duplication(self) -> None:
        report = analyze_fixture(FIXTURE)
        observations = report["observations"]

        self.assertEqual(observations["usageCandidates"], 11)
        self.assertEqual(observations["acceptedRevisions"], 10)
        self.assertEqual(observations["exactRepeatRevisions"], 1)
        self.assertEqual(observations["changedRevisions"], 2)
        self.assertEqual(observations["downwardRevisions"], 1)
        self.assertEqual(observations["malformedSnapshots"], 1)
        self.assertEqual(observations["generationResets"], 1)
        self.assertEqual(observations["responsesRetractedByReset"], 1)
        self.assertEqual(observations["requestIdsMappingToMultipleResponses"], 1)
        self.assertEqual(report["finalState"]["responseCount"], 6)

    def test_exact_zero_is_known_while_omitted_buckets_remain_unknown(self) -> None:
        aggregate = analyze_fixture(FIXTURE)["finalState"]["aggregate"]

        self.assertEqual(aggregate["input_tokens"]["knownResponses"], 6)
        self.assertEqual(aggregate["input_tokens"]["unknownResponses"], 0)
        self.assertEqual(aggregate["input_tokens"]["knownValue"], 22)
        self.assertEqual(aggregate["output_tokens"]["knownResponses"], 5)
        self.assertEqual(aggregate["output_tokens"]["unknownResponses"], 1)
        self.assertEqual(
            aggregate["cache_creation_input_tokens"]["unknownResponses"], 3
        )

    def test_fallback_key_is_the_versioned_framed_cursor_key(self) -> None:
        self.assertEqual(
            source_record_fallback_key(600, 700).hex(),
            "736f757263652d7265636f72642d763100"
            "000000000000000a01010000000000000258"
            "000000000000000a010100000000000002bc",
        )

    def test_non_contiguous_source_ranges_fail_closed(self) -> None:
        document = json.loads(FIXTURE.read_text(encoding="utf-8"))
        document["scenario"]["objects"][0]["generations"][0]["records"][1][
            "cursorStart"
        ] = 101

        with self.assertRaisesRegex(OracleError, "non-contiguous cursor range"):
            analyze_document(document, "0" * 64)

    def test_oracle_imports_only_the_python_standard_library(self) -> None:
        tree = ast.parse((Path(__file__).with_name("oracle.py")).read_text(encoding="utf-8"))
        imported_roots: set[str] = set()
        for node in ast.walk(tree):
            if isinstance(node, ast.Import):
                imported_roots.update(alias.name.split(".", 1)[0] for alias in node.names)
            elif isinstance(node, ast.ImportFrom) and node.module is not None:
                imported_roots.add(node.module.split(".", 1)[0])

        self.assertEqual(
            imported_roots,
            {
                "__future__",
                "argparse",
                "base64",
                "collections",
                "hashlib",
                "json",
                "pathlib",
                "typing",
            },
        )


if __name__ == "__main__":
    unittest.main()

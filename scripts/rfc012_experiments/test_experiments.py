import unittest

from scripts.rfc012_experiments.calibration import (
    catalog_calibration,
    observer_calibration,
    x1_report,
    x2_report,
)
from scripts.rfc012_experiments.diagnostic_aggregation import (
    DiagnosticRow,
    aggregate_diagnostics,
    row_reduction,
)
from scripts.rfc012_experiments.fts_finalization import compare_strategies


class DiagnosticAggregationTests(unittest.TestCase):
    def test_aggregates_count_and_provenance_without_dropping_repeats(self) -> None:
        rows = [
            DiagnosticRow("claude-code", "transcripts", "usage-v2", "malformed", "o1", 1, 1, "s1"),
            DiagnosticRow("claude-code", "transcripts", "usage-v2", "malformed", "o1", 1, 2, "s1"),
            DiagnosticRow("claude-code", "transcripts", "usage-v2", "malformed", "o2", 1, 3, "s2"),
        ]
        grouped = aggregate_diagnostics(rows, max_examples=8)
        self.assertEqual(len(grouped), 1)
        self.assertEqual(grouped[0].count, 3)
        self.assertEqual(grouped[0].first_commit_seq, 1)
        self.assertEqual(grouped[0].last_commit_seq, 3)
        self.assertEqual(grouped[0].provenance_objects, ("o1", "o2"))
        self.assertGreater(row_reduction(3, grouped), 0.0)


class FtsFinalizationTests(unittest.TestCase):
    def test_all_strategies_keep_search_complete_only(self) -> None:
        results = compare_strategies()
        self.assertEqual(len(results), 3)
        for item in results:
            self.assertFalse(item.search_visible_before_complete)
            self.assertEqual(item.search_available_at_ms, 11_000)


class CalibrationTests(unittest.TestCase):
    def test_catalog_and_observer_reports_bind_in_repo_fixtures(self) -> None:
        catalog = catalog_calibration()
        observer = observer_calibration()
        self.assertTrue(str(catalog["fixture_sha256"]).startswith("sha256:"))
        self.assertTrue(str(observer["fixture_sha256"]).startswith("sha256:"))
        self.assertEqual(catalog["gate"], "experiment-not-ratified-ceiling")
        self.assertIn("p50_ms", catalog["timing"])
        self.assertIn("p50_ms", observer["timing"])
        x1 = x1_report()
        x2 = x2_report()
        self.assertTrue(x1["search_remains_complete_only"])
        self.assertEqual(x2["raw_rows"], 4)
        self.assertEqual(x2["aggregated_rows"], 2)


if __name__ == "__main__":
    unittest.main()

import unittest
from pathlib import Path

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
    rows_from_sqlite_census,
)
from scripts.rfc012_experiments.fts_finalization import compare_strategies, load_frozen_trace
from scripts.rfc012_experiments.sqlite_diagnostics import load_diagnostic_rows, write_diagnostic_fixture


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

    def test_sqlite_census_rows_reduce_real_source_record_errors(self) -> None:
        path = Path("scripts/rfc012_experiments/fixtures/source-record-errors.sqlite")
        write_diagnostic_fixture(path)
        records = load_diagnostic_rows(path)
        self.assertGreaterEqual(len(records), 4)
        rows = rows_from_sqlite_census(records)
        grouped = aggregate_diagnostics(rows)
        self.assertEqual(sum(item.count for item in grouped), len(rows))
        self.assertLess(len(grouped), len(rows))


class FtsFinalizationTests(unittest.TestCase):
    def test_all_strategies_keep_search_complete_only(self) -> None:
        results = compare_strategies(load_frozen_trace())
        self.assertEqual(len(results), 3)
        for item in results:
            self.assertFalse(item.search_visible_before_complete)


class CalibrationTests(unittest.TestCase):
    def test_x2_report_uses_sqlite_fixture_not_four_hardcoded_rows(self) -> None:
        report = x2_report()
        self.assertGreaterEqual(report["raw_rows"], 4)
        self.assertIn("source-record-errors.sqlite", str(report["fixture"]))
        self.assertGreater(report["reduction"], 0.0)

    def test_x1_trace_is_bound_to_query_bootstrap_gate(self) -> None:
        milestones = load_frozen_trace()
        self.assertTrue(any(item.catalog_complete and not item.fts_complete for item in milestones))
        self.assertTrue(any(item.fts_complete for item in milestones))
        self.assertTrue(x1_report()["search_remains_complete_only"])

    def test_catalog_and_observer_reports_time_named_operations(self) -> None:
        catalog = catalog_calibration()
        observer = observer_calibration()
        self.assertEqual(catalog["operation"], "catalog-retained-page")
        self.assertEqual(observer["operation"], "observer-attach-poll")
        self.assertNotEqual(catalog["timing"]["label"], "catalog-core-transition-table")
        self.assertNotEqual(observer["timing"]["label"], "observation-negotiation-fixture")
        self.assertEqual(catalog["gate"], "experiment-not-ratified-ceiling")
        self.assertIn("p50_ms", catalog["timing"])
        self.assertIn("p50_ms", observer["timing"])


if __name__ == "__main__":
    unittest.main()

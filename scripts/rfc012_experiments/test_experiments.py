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
)
from scripts.rfc012_experiments.fts_finalization import compare_strategies, load_frozen_trace
from scripts.rfc012_experiments.sqlite_diagnostics import load_diagnostic_rows


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
    def test_all_strategies_keep_search_complete_only_on_emitted_trace(self) -> None:
        report = x1_report()
        self.assertTrue(report["search_remains_complete_only"])
        self.assertTrue(any(item["t_ms"] > 0 for item in report["milestones"]))
        self.assertEqual(report["operation"], "rfc012_x1_emit_complete_only_ingest_trace")
        results = compare_strategies(load_frozen_trace())
        self.assertEqual(len(results), 3)
        for item in results:
            self.assertFalse(item.search_visible_before_complete)


class CalibrationTests(unittest.TestCase):
    def test_x2_report_loads_engine_produced_source_record_errors(self) -> None:
        report = x2_report()
        self.assertEqual(report["source"], "rfc012_x2_dump_engine_source_record_errors")
        self.assertGreaterEqual(report["raw_rows"], 2)
        dump = Path(report["fixture"])
        if not dump.is_absolute():
            dump = Path.cwd() / dump
        records = load_diagnostic_rows(dump)
        self.assertEqual(len(records), report["raw_rows"])
        self.assertTrue(all(row[0] for row in records))
        self.assertGreater(report["reduction"], 0.0)

    def test_catalog_and_observer_reports_time_named_operations(self) -> None:
        catalog = catalog_calibration()
        observer = observer_calibration()
        self.assertEqual(catalog["operation"], "catalog-retained-page")
        self.assertEqual(observer["operation"], "scoped-resync-overflow")
        self.assertNotEqual(observer["timing"]["label"], "observer-attach-poll")
        self.assertNotEqual(observer["timing"]["label"], "observation-negotiation-fixture")
        self.assertEqual(catalog["gate"], "experiment-not-ratified-ceiling")
        self.assertIn("p50_ms", catalog["timing"])
        self.assertIn("p50_ms", observer["timing"])


if __name__ == "__main__":
    unittest.main()

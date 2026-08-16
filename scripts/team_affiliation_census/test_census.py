#!/usr/bin/env python3
"""Focused tests for the aggregate-only team-affiliation census."""

from __future__ import annotations

import json
import tempfile
import unittest
from pathlib import Path

from census import analyze


class TeamAffiliationCensusTest(unittest.TestCase):
    def test_counts_only_unique_session_scoped_native_joins(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            team = root / "teams" / "fixture-team"
            child = root / "projects" / "fixture-project" / "session-1" / "subagents"
            team.mkdir(parents=True)
            child.mkdir(parents=True)
            (team / "config.json").write_text(
                json.dumps(
                    {
                        "name": "fixture-team",
                        "leadAgentId": "lead-id",
                        "leadSessionId": "session-1",
                        "members": [
                            {"agentId": "lead-id", "name": "team-lead"},
                            {"agentId": "child-id", "name": "implementer"},
                        ],
                    }
                ),
                encoding="utf-8",
            )
            (child / "agent-child.meta.json").write_text(
                json.dumps(
                    {
                        "agentType": "general-purpose",
                        "teamName": "fixture-team",
                        "name": "implementer",
                    }
                ),
                encoding="utf-8",
            )
            (child / "agent-child.jsonl").write_text("{}\n", encoding="utf-8")
            (child / "agent-stale.meta.json").write_text(
                json.dumps(
                    {
                        "agentType": "general-purpose",
                        "teamName": "retired-team",
                        "name": "implementer",
                    }
                ),
                encoding="utf-8",
            )
            (child / "agent-ordinary.meta.json").write_text(
                json.dumps({"agentType": "Explore"}),
                encoding="utf-8",
            )

            report = analyze(root)

            self.assertEqual(report["teamConfigs"]["files"], 1)
            self.assertEqual(report["teamConfigs"]["valid"], 1)
            self.assertEqual(report["teamConfigs"]["members"], 2)
            self.assertEqual(report["teamConfigs"]["exactlyOneLeadMember"], 1)
            self.assertEqual(report["subagentMetadata"]["files"], 3)
            self.assertEqual(report["subagentMetadata"]["withTeamAndName"], 2)
            self.assertEqual(report["subagentMetadata"]["uniqueCurrentConfigMatch"], 1)
            self.assertEqual(report["subagentMetadata"]["noCurrentConfigMatch"], 1)
            self.assertEqual(report["subagentMetadata"]["uniqueMatchWithTranscript"], 1)
            self.assertTrue(report["conclusion"]["rootLeadRuleSupported"])
            self.assertTrue(report["conclusion"]["childMetadataRuleSupported"])

    def test_duplicate_member_names_are_ambiguous_not_guessed(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            team = root / "teams" / "fixture-team"
            child = root / "projects" / "fixture-project" / "session-1" / "subagents"
            team.mkdir(parents=True)
            child.mkdir(parents=True)
            (team / "config.json").write_text(
                json.dumps(
                    {
                        "name": "fixture-team",
                        "leadAgentId": "lead-id",
                        "leadSessionId": "session-1",
                        "members": [
                            {"agentId": "lead-id", "name": "duplicate"},
                            {"agentId": "child-id", "name": "duplicate"},
                        ],
                    }
                ),
                encoding="utf-8",
            )
            (child / "agent-child.meta.json").write_text(
                json.dumps(
                    {
                        "agentType": "general-purpose",
                        "teamName": "fixture-team",
                        "name": "duplicate",
                    }
                ),
                encoding="utf-8",
            )

            report = analyze(root)

            self.assertEqual(report["teamConfigs"]["uniqueMemberNames"], 0)
            self.assertEqual(report["subagentMetadata"]["uniqueCurrentConfigMatch"], 0)
            self.assertEqual(report["subagentMetadata"]["ambiguousCurrentConfigMatch"], 1)
            self.assertFalse(report["conclusion"]["childMetadataRuleSupported"])


if __name__ == "__main__":
    unittest.main()

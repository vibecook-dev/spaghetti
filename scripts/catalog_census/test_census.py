#!/usr/bin/env python3
"""Focused tests for the bounded catalog census experiment."""

from __future__ import annotations

import json
import tempfile
import unittest
from pathlib import Path

from census import encode_project_key, identity_digest, scan_claude, scan_codex, scan_grok


class CatalogCensusTest(unittest.TestCase):
    def test_codex_candidate_oracle_matches_frozen_rfc012b_fixture(self) -> None:
        repository = Path(__file__).resolve().parents[2]
        fixture = json.loads(
            (
                repository
                / "crates/spaghetti-napi/fixtures/contracts"
                / "rfc012b-codex-candidate-conformance-v1.json"
            ).read_text(encoding="utf-8")
        )
        self.assertEqual(fixture["fixture_contract_version"], 1)
        self.assertEqual(fixture["adapter_id"], "codex")
        self.assertEqual(fixture["support_release_status"], "candidate")
        self.assertFalse(fixture["catalog_execution_authorized"])
        self.assertEqual(fixture["head_bound_status"], "candidate_fixture_evidence")

        result = scan_codex(
            repository / "crates/spaghetti-napi/fixtures/small-codex/.codex",
            head_bytes=fixture["bounds"]["max_head_prefix_bytes"],
        )
        oracle = fixture["independent_oracle"]
        projects = result.catalog.project_identities()
        sessions = result.catalog.session_identities()
        self.assertEqual(len(projects), oracle["project_count"])
        self.assertEqual(len(sessions), oracle["session_count"])
        self.assertEqual(identity_digest(projects), oracle["project_identity_digest"])
        self.assertEqual(identity_digest(sessions), oracle["session_identity_digest"])

    def test_claude_unions_index_and_transcript_without_reading_full_transcript(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary) / ".claude"
            project = root / "projects" / "-tmp-project"
            project.mkdir(parents=True)
            indexed = "11111111-1111-1111-1111-111111111111"
            transcript = "22222222-2222-2222-2222-222222222222"
            (project / "sessions-index.json").write_text(
                json.dumps(
                    {
                        "version": 1,
                        "originalPath": "/tmp/project",
                        "entries": [
                            {
                                "sessionId": indexed,
                                "projectPath": "/tmp/project",
                                "firstPrompt": "indexed prompt",
                                "created": "2026-01-01T00:00:00Z",
                                "modified": "2026-01-01T00:01:00Z",
                                "messageCount": 2,
                            }
                        ],
                    }
                ),
                encoding="utf-8",
            )
            payload = (
                json.dumps(
                    {
                        "type": "user",
                        "sessionId": transcript,
                        "cwd": "/tmp/project",
                        "timestamp": "2026-01-02T00:00:00Z",
                        "message": {"content": "transcript prompt"},
                    }
                )
                + "\n"
                + ("x" * 100_000)
            )
            (project / f"{transcript}.jsonl").write_text(payload, encoding="utf-8")

            result = scan_claude(root, head_bytes=4096, document_bytes=1024 * 1024)

            self.assertEqual(len(result.catalog.projects), 1)
            self.assertEqual(len(result.catalog.sessions), 2)
            self.assertEqual(
                result.catalog.sessions[("-tmp-project", transcript)].first_prompt,
                "transcript prompt",
            )
            self.assertLess(result.metrics.bytes_read, len(payload))

    def test_codex_skips_internal_rollouts_and_extracts_user_prompt(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary) / ".codex"
            sessions = root / "sessions" / "2026" / "01" / "01"
            sessions.mkdir(parents=True)
            external = sessions / "rollout-external.jsonl"
            external.write_text(
                "\n".join(
                    [
                        json.dumps(
                            {
                                "type": "session_meta",
                                "timestamp": "2026-01-01T00:00:00Z",
                                "payload": {"id": "external", "cwd": "/tmp/project"},
                            }
                        ),
                        json.dumps(
                            {
                                "type": "response_item",
                                "payload": {
                                    "type": "message",
                                    "role": "user",
                                    "content": [{"type": "input_text", "text": "Fix it"}],
                                },
                            }
                        ),
                    ]
                )
                + "\n",
                encoding="utf-8",
            )
            internal = sessions / "rollout-internal.jsonl"
            internal.write_text(
                json.dumps(
                    {
                        "type": "session_meta",
                        "payload": {
                            "id": "child",
                            "cwd": "/tmp/project",
                            "thread_source": "subagent",
                        },
                    }
                )
                + "\n",
                encoding="utf-8",
            )

            result = scan_codex(root, head_bytes=4096)

            self.assertEqual(len(result.catalog.sessions), 1)
            record = result.catalog.sessions[("-tmp-project", "external")]
            self.assertEqual(record.first_prompt, "Fix it")
            self.assertEqual(result.metrics.evidence["internal-session-skipped"], 1)

    def test_grok_uses_membership_and_summary_without_chat_read(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary) / ".grok"
            session = root / "sessions" / "%2Ftmp%2Fproject" / "native-id"
            session.mkdir(parents=True)
            (session / "summary.json").write_text(
                json.dumps(
                    {
                        "info": {"id": "summary-id", "cwd": "/tmp/project"},
                        "generated_title": "A useful title",
                        "created_at": "2026-01-01T00:00:00Z",
                        "updated_at": "2026-01-01T00:01:00Z",
                        "num_chat_messages": 4,
                    }
                ),
                encoding="utf-8",
            )
            (session / "chat_history.jsonl").write_text("z" * 100_000, encoding="utf-8")

            result = scan_grok(root, head_bytes=4096, document_bytes=1024 * 1024)

            key = (encode_project_key("/tmp/project"), "summary-id")
            self.assertIn(key, result.catalog.sessions)
            self.assertEqual(result.catalog.sessions[key].title, "A useful title")
            self.assertLess(result.metrics.bytes_read, 4096)


if __name__ == "__main__":
    unittest.main()

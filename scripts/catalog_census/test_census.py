#!/usr/bin/env python3
"""Focused tests for the bounded catalog census experiment."""

from __future__ import annotations

import json
import tempfile
import unittest
from pathlib import Path

from census import encode_project_key, identity_digest, scan_claude, scan_codex, scan_grok


class CatalogCensusTest(unittest.TestCase):
    def test_claude_candidate_oracle_matches_frozen_rfc012b_fixture(self) -> None:
        repository = Path(__file__).resolve().parents[2]
        fixture = json.loads(
            (
                repository
                / "crates/spaghetti-napi/fixtures/contracts"
                / "rfc012b-claude-candidate-conformance-v1.json"
            ).read_text(encoding="utf-8")
        )
        self.assertEqual(fixture["fixture_contract_version"], 1)
        self.assertEqual(fixture["adapter_id"], "claude-code")
        self.assertEqual(fixture["support_release_status"], "candidate")
        self.assertFalse(fixture["catalog_execution_authorized"])
        self.assertEqual(fixture["planned_composition_status"], "planned_unbound")
        bounds = fixture["bounds"]
        self.assertEqual(bounds["transcript_head_max_record_payload_bytes"], 65_536)
        self.assertEqual(bounds["transcript_head_delimiter_bytes"], 1)
        self.assertEqual(bounds["transcript_head_framing_read_ahead_bytes"], 65_536)
        self.assertTrue(
            bounds["transcript_head_delimiter_included_in_framing_read_ahead"]
        )
        self.assertEqual(bounds["transcript_head_checkpoint_anchor_bytes"], 4_096)
        self.assertEqual(
            bounds["transcript_head_physical_read_ceiling_bytes"],
            65_536 + 65_536 + 4_096,
        )

        result = scan_claude(
            repository / "crates/spaghetti-napi/fixtures/small/.claude",
            head_bytes=bounds["transcript_head_max_window_payload_bytes"],
            document_bytes=bounds["index_max_document_bytes"],
        )
        oracle = fixture["independent_oracle"]
        projects = result.catalog.project_identities()
        sessions = result.catalog.session_identities()
        self.assertEqual(len(projects), oracle["project_count"])
        self.assertEqual(len(sessions), oracle["session_count"])
        self.assertEqual(identity_digest(projects), oracle["project_identity_digest"])
        self.assertEqual(identity_digest(sessions), oracle["session_identity_digest"])
        self.assertEqual(result.metrics.evidence["subagent-parent-uuid-shaped"], 2)
        self.assertEqual(result.metrics.evidence["subagent-parent-non-uuid"], 0)
        self.assertEqual(result.metrics.evidence["subagent-parent-nested-only"], 0)
        self.assertEqual(
            result.metrics.evidence["subagent-parent-nested-only-uuid-shaped"], 0
        )
        self.assertEqual(
            result.metrics.evidence["subagent-parent-nested-only-non-uuid"], 0
        )

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

    def test_grok_candidate_oracle_matches_frozen_rfc012b_fixture(self) -> None:
        repository = Path(__file__).resolve().parents[2]
        fixture = json.loads(
            (
                repository
                / "crates/spaghetti-napi/fixtures/contracts"
                / "rfc012b-grok-candidate-conformance-v1.json"
            ).read_text(encoding="utf-8")
        )
        self.assertEqual(fixture["fixture_contract_version"], 1)
        self.assertEqual(fixture["adapter_id"], "grok")
        self.assertEqual(fixture["support_release_status"], "candidate")
        self.assertFalse(fixture["catalog_execution_authorized"])
        self.assertEqual(fixture["planned_composition_status"], "planned_unbound")
        self.assertEqual(
            fixture["admission_policy_status"], "current_candidate_declaration"
        )

        result = scan_grok(
            repository / "crates/spaghetti-napi/fixtures/small-grok/.grok",
            head_bytes=4096,
            document_bytes=fixture["bounds"]["summary_max_document_bytes"],
        )
        oracle = fixture["independent_oracle"]
        projects = result.catalog.project_identities()
        sessions = result.catalog.session_identities()
        self.assertEqual(len(projects), oracle["project_count"])
        self.assertEqual(len(sessions), oracle["session_count"])
        self.assertEqual(identity_digest(projects), oracle["project_identity_digest"])
        self.assertEqual(identity_digest(sessions), oracle["session_identity_digest"])
        self.assertEqual(result.metrics.evidence["session-directory-census-admitted"], 4)
        self.assertEqual(
            result.metrics.evidence["session-directory-current-policy-admitted"], 4
        )
        self.assertEqual(
            result.metrics.evidence["session-directory-current-policy-with-updates"], 4
        )
        self.assertEqual(
            result.metrics.evidence["session-directory-current-policy-without-updates"], 0
        )
        self.assertEqual(result.metrics.evidence["session-directory-updates-only"], 0)

    def test_claude_unions_index_and_transcript_without_reading_full_transcript(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary) / ".claude"
            project = root / "projects" / "-tmp-project"
            project.mkdir(parents=True)
            indexed = "11111111-1111-1111-1111-111111111111"
            transcript = "22222222-2222-2222-2222-222222222222"
            nested = "33333333-3333-3333-3333-333333333333"
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
            nested_subagents = project / nested / "subagents"
            nested_subagents.mkdir(parents=True)
            (nested_subagents / "agent-child.jsonl").write_text(
                json.dumps(
                    {
                        "type": "user",
                        "sessionId": nested,
                        "cwd": "/tmp/project",
                        "message": {"content": "nested prompt"},
                    }
                )
                + "\n",
                encoding="utf-8",
            )

            result = scan_claude(root, head_bytes=4096, document_bytes=1024 * 1024)

            self.assertEqual(len(result.catalog.projects), 1)
            self.assertEqual(len(result.catalog.sessions), 3)
            self.assertEqual(
                result.catalog.sessions[("-tmp-project", transcript)].first_prompt,
                "transcript prompt",
            )
            self.assertEqual(
                result.catalog.sessions[("-tmp-project", nested)].evidence,
                {"subagent-membership"},
            )
            self.assertLess(result.metrics.bytes_read, len(payload))

    def test_claude_reports_nested_parent_shape_and_identity_delta_without_ids(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary) / ".claude"
            project = root / "projects" / "-private-project"
            project.mkdir(parents=True)
            overlap = "11111111-1111-1111-1111-111111111111"
            nested_uuid = "22222222-2222-2222-2222-222222222222"
            nested_opaque = "private-opaque-session"
            (project / f"{overlap}.jsonl").write_text("", encoding="utf-8")
            for session_id in (overlap, nested_uuid, nested_opaque):
                subagents = project / session_id / "subagents"
                subagents.mkdir(parents=True)
                (subagents / "agent-child.jsonl").write_text("{}\n", encoding="utf-8")

            result = scan_claude(root, head_bytes=4096, document_bytes=1024 * 1024)

            self.assertEqual(len(result.catalog.sessions), 3)
            evidence = result.metrics.evidence
            self.assertEqual(evidence["subagent-parent-uuid-shaped"], 2)
            self.assertEqual(evidence["subagent-parent-non-uuid"], 1)
            self.assertEqual(evidence["subagent-parent-nested-only"], 2)
            self.assertEqual(
                evidence["subagent-parent-nested-only-uuid-shaped"], 1
            )
            self.assertEqual(evidence["subagent-parent-nested-only-non-uuid"], 1)
            serialized = json.dumps(dict(evidence), sort_keys=True)
            for private_value in ("-private-project", overlap, nested_uuid, nested_opaque):
                self.assertNotIn(private_value, serialized)

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

    def test_grok_reports_current_policy_updates_delta_without_ids(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary) / ".grok"
            project = root / "sessions" / "%2Fprivate%2Fproject"
            current_only = project / "private-current-only"
            updates_only = project / "private-updates-only"
            overlap = project / "private-overlap"
            for session in (current_only, updates_only, overlap):
                session.mkdir(parents=True)
            (current_only / "chat_history.jsonl").write_text("", encoding="utf-8")
            (updates_only / "updates.jsonl").write_text("{}\n", encoding="utf-8")
            (overlap / "summary.json").write_text("{}", encoding="utf-8")
            (overlap / "updates.jsonl").write_text("{}\n", encoding="utf-8")

            result = scan_grok(root, head_bytes=4096, document_bytes=1024 * 1024)

            # Preserve the independent census semantics while making its
            # exact delta from the current candidate declaration measurable.
            self.assertEqual(len(result.catalog.sessions), 3)
            evidence = result.metrics.evidence
            self.assertEqual(evidence["session-directory-census-admitted"], 3)
            self.assertEqual(
                evidence["session-directory-current-policy-admitted"], 2
            )
            self.assertEqual(
                evidence["session-directory-current-policy-with-updates"], 1
            )
            self.assertEqual(
                evidence["session-directory-current-policy-without-updates"], 1
            )
            self.assertEqual(evidence["session-directory-updates-only"], 1)
            serialized = json.dumps(dict(evidence), sort_keys=True)
            for private_value in (
                "%2Fprivate%2Fproject",
                "private-current-only",
                "private-updates-only",
                "private-overlap",
            ):
                self.assertNotIn(private_value, serialized)


if __name__ == "__main__":
    unittest.main()

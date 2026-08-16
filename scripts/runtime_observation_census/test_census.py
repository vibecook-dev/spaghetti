#!/usr/bin/env python3
"""Focused tests for the runtime-observation census."""

from __future__ import annotations

import json
import tempfile
import unittest
from pathlib import Path

from census import analyze


def append(path: Path, value: dict[str, object], *, newline: bool = True) -> None:
    with path.open("ab") as handle:
        handle.write(json.dumps(value).encode())
        if newline:
            handle.write(b"\n")


def assistant(
    row_id: str,
    response_id: str,
    request_id: str,
    usage: tuple[int, int, int, int],
    content: list[dict[str, object]] | None = None,
) -> dict[str, object]:
    return {
        "type": "assistant",
        "uuid": row_id,
        "requestId": request_id,
        "message": {
            "id": response_id,
            "model": "claude-test",
            "content": content or [],
            "usage": {
                "input_tokens": usage[0],
                "output_tokens": usage[1],
                "cache_creation_input_tokens": usage[2],
                "cache_read_input_tokens": usage[3],
            },
        },
    }


class RuntimeObservationCensusTest(unittest.TestCase):
    def test_groups_usage_by_file_and_response_and_keeps_latest_revision(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            project = root / "project"
            project.mkdir()
            transcript = project / "session.jsonl"
            append(transcript, assistant("row-1", "response-1", "request-1", (10, 1, 2, 3)))
            append(transcript, assistant("row-2", "response-1", "request-1", (10, 5, 2, 3)))
            append(transcript, assistant("row-3", "response-1", "request-1", (9, 4, 2, 3)))
            append(transcript, assistant("row-4", "response-2", "request-2", (7, 2, 0, 1)))

            usage = analyze(root)["usage"]

            self.assertEqual(usage["usageBearingAssistantRows"], 4)
            self.assertEqual(usage["fileScopedResponseGroups"], 2)
            self.assertEqual(usage["usageActorFiles"], 1)
            self.assertEqual(usage["usageSessions"], 1)
            self.assertEqual(usage["rootResponseGroups"], 2)
            self.assertEqual(usage["childResponseGroups"], 0)
            self.assertEqual(usage["repeatedRowsBeyondFirst"], 2)
            self.assertEqual(usage["groupsWithChangedCounters"], 1)
            self.assertEqual(usage["groupsWithDownwardCorrection"], 1)
            self.assertEqual(usage["latestResponseSnapshotTotal"]["input_tokens"], 16)
            self.assertEqual(usage["latestResponseSnapshotTotal"]["output_tokens"], 6)
            self.assertEqual(usage["legacyPerRowDeltaTotal"]["input_tokens"], 36)
            self.assertEqual(usage["groupsWithAllBucketsKnown"], 2)
            self.assertEqual(usage["latestResponseUnknownGroups"]["input_tokens"], 0)

    def test_preserves_latest_missing_bucket_as_unknown_instead_of_zero(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            project = root / "project"
            project.mkdir()
            transcript = project / "session.jsonl"
            first = assistant("row-1", "response-1", "request-1", (10, 1, 2, 3))
            correction = assistant("row-2", "response-1", "request-1", (9, 2, 0, 4))
            del correction["message"]["usage"]["cache_creation_input_tokens"]  # type: ignore[index]
            append(transcript, first)
            append(transcript, correction)

            usage = analyze(root)["usage"]

            self.assertEqual(usage["fileScopedResponseGroups"], 1)
            self.assertEqual(usage["groupsWithAllBucketsKnown"], 0)
            self.assertEqual(
                usage["latestResponseUnknownGroups"]["cache_creation_input_tokens"],
                1,
            )
            self.assertEqual(usage["latestResponseSnapshotTotal"]["cache_creation_input_tokens"], 0)

    def test_scopes_response_identity_to_file_and_treats_request_id_as_metadata(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            project = root / "project"
            project.mkdir()
            first = project / "first.jsonl"
            second = project / "second.jsonl"
            append(first, assistant("row-1", "shared-response", "shared-request", (1, 1, 0, 0)))
            append(first, assistant("row-2", "other-response", "shared-request", (2, 1, 0, 0)))
            append(second, assistant("row-3", "shared-response", "request-3", (3, 1, 0, 0)))
            missing = assistant("row-4", "temporary", "temporary", (4, 1, 0, 0))
            del missing["requestId"]
            del missing["message"]["id"]  # type: ignore[index]
            append(second, missing)

            usage = analyze(root)["usage"]

            self.assertEqual(usage["fileScopedResponseGroups"], 4)
            self.assertEqual(usage["requestIdsMappingToMultipleMessageIds"], 1)
            self.assertEqual(usage["rowsWithoutRequestId"], 1)
            self.assertEqual(usage["rowsWithoutMessageId"], 1)

    def test_detects_actor_paths_interactions_and_holds_partial_lines(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            project = root / "project"
            child_dir = project / "session" / "subagents"
            child_dir.mkdir(parents=True)
            root_transcript = project / "session.jsonl"
            child_transcript = child_dir / "agent-child.jsonl"
            ask = {
                "type": "tool_use",
                "id": "question-1",
                "name": "AskUserQuestion",
                "input": {"questions": [{"question": "redacted", "options": []}]},
            }
            append(root_transcript, assistant("row-1", "response-1", "request-1", (1, 1, 0, 0), [ask]))
            append(
                root_transcript,
                {
                    "type": "user",
                    "message": {
                        "content": [
                            {"type": "tool_result", "tool_use_id": "question-1", "content": "redacted"}
                        ]
                    },
                },
            )
            append(child_transcript, {"type": "progress"})
            append(child_transcript, {"type": "assistant"}, newline=False)
            journal = child_dir / "workflows" / "wf-test" / "journal.jsonl"
            journal.parent.mkdir(parents=True)
            append(journal, assistant("ignored-row", "ignored-response", "ignored-request", (99, 99, 0, 0)))

            report = analyze(root)

            self.assertEqual(report["input"]["files"], 2)
            self.assertEqual(report["input"]["rootTranscriptFiles"], 1)
            self.assertEqual(report["input"]["childTranscriptFiles"], 1)
            self.assertEqual(report["input"]["standardChildTranscriptFiles"], 1)
            self.assertEqual(report["parse"]["partialFinalLinesHeld"], 1)
            interactions = report["typedEvidence"]["userInputRequests"]
            self.assertEqual(interactions["askUserQuestionCalls"], 1)
            self.assertEqual(interactions["matchedSuccessfulResults"], 1)
            self.assertEqual(interactions["pendingAtFileEnd"], 0)


if __name__ == "__main__":
    unittest.main()

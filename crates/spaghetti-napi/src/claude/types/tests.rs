//! Deserialization smoke tests for the ported session types. Each test
//! feeds a raw JSON fixture through serde_json and asserts the shape.

use super::*;

#[test]
fn deserializes_user_message_line() {
    let raw = r#"{
        "type": "user",
        "uuid": "u-1",
        "parentUuid": null,
        "timestamp": "2026-04-01T00:00:00.000Z",
        "sessionId": "s-1",
        "cwd": "/tmp",
        "version": "1.0.0",
        "gitBranch": "main",
        "isSidechain": false,
        "userType": "external",
        "message": {
            "role": "user",
            "content": "hello world"
        }
    }"#;

    let msg: SessionMessage = serde_json::from_str(raw).expect("parse user message");
    match msg {
        SessionMessage::User(u) => {
            assert_eq!(u.base.uuid, "u-1");
            assert_eq!(u.base.session_id, "s-1");
            assert_eq!(u.base.parent_uuid, None);
            match u.message.content {
                UserMessageContent::Text(ref t) => assert_eq!(t, "hello world"),
                _ => panic!("expected text content"),
            }
        }
        _ => panic!("expected user variant"),
    }
}

#[test]
fn deserializes_assistant_message_with_tool_use_block() {
    let raw = r#"{
        "type": "assistant",
        "uuid": "a-1",
        "parentUuid": "u-1",
        "timestamp": "2026-04-01T00:00:01.000Z",
        "sessionId": "s-1",
        "cwd": "/tmp",
        "version": "1.0.0",
        "gitBranch": "main",
        "isSidechain": false,
        "userType": "external",
        "requestId": "req-1",
        "message": {
            "model": "claude-opus-4-7",
            "id": "msg-1",
            "type": "message",
            "role": "assistant",
            "content": [
                {"type": "text", "text": "sure, reading it"},
                {"type": "tool_use", "id": "tu-1", "name": "Read", "input": {"file_path": "/a.txt"}}
            ],
            "stop_reason": "tool_use",
            "stop_sequence": null,
            "usage": {
                "input_tokens": 10,
                "output_tokens": 20,
                "cache_creation_input_tokens": 0,
                "cache_read_input_tokens": 5,
                "service_tier": "default"
            }
        }
    }"#;

    let msg: SessionMessage = serde_json::from_str(raw).expect("parse assistant message");
    let SessionMessage::Assistant(a) = msg else {
        panic!("expected assistant variant");
    };
    assert_eq!(a.request_id, "req-1");
    assert_eq!(a.message.content.len(), 2);
    assert_eq!(a.message.usage.input_tokens, 10);
    assert_eq!(a.message.usage.cache_read_input_tokens, 5);
    match &a.message.content[1] {
        AssistantContentBlock::ToolUse(t) => {
            assert_eq!(t.id, "tu-1");
            assert_eq!(t.name, "Read");
            assert_eq!(t.input["file_path"], "/a.txt");
        }
        _ => panic!("expected tool_use block at index 1"),
    }
}

#[test]
fn deserializes_summary_line() {
    let raw = r#"{"type":"summary","summary":"chat about rust","leafUuid":"leaf-42"}"#;
    let msg: SessionMessage = serde_json::from_str(raw).expect("parse summary");
    let SessionMessage::Summary(s) = msg else {
        panic!("expected summary variant");
    };
    assert_eq!(s.summary, "chat about rust");
    assert_eq!(s.leaf_uuid, "leaf-42");
}

#[test]
fn classified_native_drift_bridge_owner_fields_parse_as_native_metadata() {
    let raw = r#"{
        "type": "bridge-session",
        "sessionId": "fixture-session",
        "bridgeSessionId": "fixture-bridge",
        "lastSequenceNum": 7,
        "ownerAccountUuid": "fixture-account",
        "ownerOrganizationUuid": "fixture-organization"
    }"#;

    let msg: SessionMessage = serde_json::from_str(raw).expect("parse bridge-session message");
    let SessionMessage::BridgeSession(bridge) = msg else {
        panic!("expected bridge-session variant");
    };
    assert_eq!(
        bridge.owner_account_uuid.as_deref(),
        Some("fixture-account")
    );
    assert_eq!(
        bridge.owner_organization_uuid.as_deref(),
        Some("fixture-organization")
    );
}

#[test]
fn deserializes_session_index_entry() {
    let raw = r#"{
        "sessionId": "s-1",
        "fullPath": "/home/me/.claude/projects/-tmp/s-1.jsonl",
        "fileMtime": 1712000000000.0,
        "firstPrompt": "hi",
        "summary": "small chat",
        "messageCount": 4,
        "created": "2026-04-01T00:00:00.000Z",
        "modified": "2026-04-01T00:01:00.000Z",
        "gitBranch": "main",
        "projectPath": "/tmp",
        "isSidechain": false
    }"#;
    let entry: SessionIndexEntry = serde_json::from_str(raw).expect("parse index entry");
    assert_eq!(entry.session_id, "s-1");
    assert_eq!(entry.message_count, 4);
    assert!(!entry.is_sidechain);
}

#[test]
fn deserializes_subagent_transcript_message_line() {
    // A subagent JSONL file contains SessionMessage lines; verify that an
    // assistant-with-agentId line parses cleanly as a SessionMessage.
    let raw = r#"{
        "type": "assistant",
        "uuid": "a-sub-1",
        "parentUuid": null,
        "timestamp": "2026-04-01T00:00:02.000Z",
        "sessionId": "s-1",
        "cwd": "/tmp",
        "version": "1.0.0",
        "gitBranch": "main",
        "isSidechain": true,
        "userType": "external",
        "requestId": "req-sub",
        "agentId": "agent-abc",
        "message": {
            "model": "claude-opus-4-7",
            "id": "msg-sub",
            "type": "message",
            "role": "assistant",
            "content": [{"type": "text", "text": "subagent reply"}],
            "stop_reason": "end_turn",
            "stop_sequence": null,
            "usage": {
                "input_tokens": 1,
                "output_tokens": 2,
                "cache_creation_input_tokens": 0,
                "cache_read_input_tokens": 0,
                "service_tier": null
            }
        }
    }"#;
    let msg: SessionMessage = serde_json::from_str(raw).expect("parse subagent message");
    let SessionMessage::Assistant(a) = msg else {
        panic!("expected assistant variant");
    };
    assert_eq!(a.agent_id.as_deref(), Some("agent-abc"));
    assert!(a.base.is_sidechain);
}

#[test]
fn deserializes_persisted_tool_result_and_todo_file() {
    let tr_raw = r#"{"toolUseId":"tu-123","content":"file contents here"}"#;
    let tr: PersistedToolResult = serde_json::from_str(tr_raw).expect("parse tool result");
    assert_eq!(tr.tool_use_id, "tu-123");

    let todo_raw = r#"{
        "sessionId": "s-1",
        "agentId": "root",
        "items": [
            {"content": "do a thing", "status": "pending"},
            {"content": "doing a thing", "status": "in_progress", "activeForm": "Doing"}
        ]
    }"#;
    let tf: TodoFile = serde_json::from_str(todo_raw).expect("parse todo file");
    assert_eq!(tf.items.len(), 2);
    assert_eq!(tf.items[1].active_form.as_deref(), Some("Doing"));
}

#[test]
fn deserializes_system_compact_boundary_with_unknown_fields() {
    // Extra unknown keys (future_field, extraNoise) must be tolerated.
    let raw = r#"{
        "type": "system",
        "uuid": "sys-1",
        "parentUuid": null,
        "timestamp": "2026-04-01T00:00:03.000Z",
        "sessionId": "s-1",
        "cwd": "/tmp",
        "version": "1.0.0",
        "gitBranch": "main",
        "isSidechain": false,
        "userType": "external",
        "level": "info",
        "subtype": "compact_boundary",
        "content": "compacted",
        "logicalParentUuid": "u-99",
        "compactMetadata": {"trigger": "auto", "preTokens": 12345},
        "future_field": "ignored",
        "extraNoise": [1,2,3]
    }"#;
    let msg: SessionMessage = serde_json::from_str(raw).expect("parse system message");
    let SessionMessage::System(s) = msg else {
        panic!("expected system variant");
    };
    assert_eq!(s.level.as_deref(), Some("info"));
    // Top-level `content` is exposed for FTS regardless of subtype.
    assert_eq!(s.content_str(), Some("compacted"));
    // `subtype` and all subtype-specific / unknown fields round-trip via extra.
    assert_eq!(
        s.extra.get("subtype").and_then(|v| v.as_str()),
        Some("compact_boundary")
    );
    assert_eq!(
        s.extra
            .get("compactMetadata")
            .and_then(|v| v.get("trigger"))
            .and_then(|v| v.as_str()),
        Some("auto")
    );
    assert!(s.extra.contains_key("future_field"));
    assert!(s.extra.contains_key("extraNoise"));
}

#[test]
fn deserializes_system_message_with_missing_subtype() {
    // A system line without any `subtype` field must parse (the old
    // internally-tagged payload rejected it) and still expose `content`.
    let raw = r#"{
        "type": "system",
        "uuid": "sys-2",
        "timestamp": "2026-04-01T00:00:04.000Z",
        "sessionId": "s-1",
        "content": "no subtype here"
    }"#;
    let msg: SessionMessage = serde_json::from_str(raw).expect("missing subtype must parse");
    let SessionMessage::System(s) = msg else {
        panic!("expected system variant");
    };
    assert!(s.extra.get("subtype").is_none());
    assert_eq!(s.content_str(), Some("no subtype here"));
}

#[test]
fn assistant_line_without_usage_or_request_id_still_parses() {
    // API-error assistant lines omit `message.usage` and `requestId`; the
    // typed parse must tolerate both so the line keeps its FTS text and is
    // not dropped from subagent transcripts (message_count includes it).
    let raw = r#"{
        "type": "assistant",
        "uuid": "a-err",
        "timestamp": "2026-04-01T00:00:05.000Z",
        "sessionId": "s-1",
        "isApiErrorMessage": true,
        "message": {
            "model": "claude-opus-4-7",
            "id": "msg-err",
            "type": "message",
            "role": "assistant",
            "content": [{"type": "text", "text": "overloaded"}]
        }
    }"#;
    let msg: SessionMessage = serde_json::from_str(raw).expect("api-error assistant must parse");
    let SessionMessage::Assistant(a) = msg else {
        panic!("expected assistant variant");
    };
    assert_eq!(a.request_id, "");
    assert_eq!(a.message.usage.input_tokens, 0);
    assert_eq!(a.message.content.len(), 1);
}

#[test]
fn summary_line_without_leaf_uuid_still_parses() {
    let raw = r#"{"type":"summary","summary":"just a summary"}"#;
    let msg: SessionMessage =
        serde_json::from_str(raw).expect("summary without leafUuid must parse");
    let SessionMessage::Summary(s) = msg else {
        panic!("expected summary variant");
    };
    assert_eq!(s.summary, "just a summary");
    assert_eq!(s.leaf_uuid, "");
}

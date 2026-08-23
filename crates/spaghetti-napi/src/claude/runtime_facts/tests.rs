//! Behavioral tests for RFC 012C runtime fact emission.
//!
//! Every family test below decodes real JSONL through the real
//! `ClaudeCodeAdapter::decode` spine and then reduces the emitted facts with
//! the real RFC 012C reducers, so what is asserted is the state a consumer
//! ends up with. There are no digest-stability or struct round-trip
//! assertions: a frozen fixture is a source of records here, never a golden
//! byte pattern.

use std::collections::{BTreeMap, BTreeSet};

use super::*;
use crate::adapter::{
    AdapterId, AgentAdapter, ContractCompleteness, DecodeContext, Fact, FactBatch,
    FactSemanticContext, FactSemanticRevision,
};
use crate::claude::adapter::ClaudeCodeAdapter;
use crate::runtime_semantic_reducer::{reduce_runtime_fact_revision, RuntimeFactReduction};
use crate::source::{RecordOrigin, SourceCursor, SourceMediaType};

const SESSION: &str = "01234567-89ab-cdef-0123-456789abcdef";
const PROJECT: &str = "-Users-test-project";

fn transcript_object_context() -> crate::adapter::AdapterObjectContext {
    crate::adapter::AdapterObjectContext::new(
        1,
        serde_json::to_vec(&serde_json::json!({
            "project_slug": PROJECT,
            "session_id": SESSION,
            "agent_id": null,
            "workflow_id": null,
        }))
        .expect("object context"),
    )
    .expect("object context")
}

/// Decode a whole transcript the way the engine does: one record at a time,
/// carrying decoder state forward, through the adapter's public seam.
fn decode_transcript(lines: &[&str]) -> FactBatch {
    let adapter = ClaudeCodeAdapter::new();
    let object_key = format!("{PROJECT}/{SESSION}.jsonl");
    let mut batch = FactBatch::new_with_semantic_context(
        4096,
        64,
        FactSemanticContext::new(
            &AdapterId::new("claude-code").expect("adapter id"),
            1,
            b"fixture-root",
            b"session-transcripts",
            object_key.as_bytes(),
            1,
        )
        .expect("semantic context"),
    )
    .expect("fact batch");

    let object_context = transcript_object_context();
    let decoder = crate::adapter::DecoderId::new("claude-session-record").expect("decoder");

    let mut offset = 0_u64;
    let mut decoder_state: Option<Vec<u8>> = None;
    for line in lines {
        let payload = line.as_bytes().to_vec();
        let length = payload.len() as u64;
        let record = crate::source::SourceRecord::new(
            &RecordOrigin {
                source_instance_id: 7,
                stream_id: 8,
                object_id: 9,
                observed_at: 10,
                source_timestamp_hint: Some(1_776_211_200_000),
                media_type: SourceMediaType::new("application/x-ndjson").expect("media type"),
            },
            1,
            SourceCursor::append_offset(offset),
            SourceCursor::append_offset(offset + length + 1),
            0,
            payload,
        );
        offset += length + 1;
        adapter
            .decode(
                DecodeContext {
                    decoder: &decoder,
                    object_context: &object_context,
                    decoder_state: decoder_state.as_deref(),
                },
                &record,
                &mut batch,
            )
            .expect("the adapter decodes a well-formed Claude record");
        decoder_state = batch.next_decoder_state().map(<[u8]>::to_vec);
    }
    batch
}

/// Reduce every emitted fact of one family the way the observer and the
/// durable projection both do, and return the accepted state per entity.
fn reduce_family(batch: &FactBatch, family: &str) -> BTreeMap<String, Fact> {
    let mut accepted: BTreeMap<String, (FactSemanticRevision, Fact)> = BTreeMap::new();
    for envelope in batch.facts() {
        if envelope.value.kind() != family {
            continue;
        }
        let semantic = envelope
            .semantic_revision
            .expect("a runtime fact always carries a canonical semantic revision");
        let entity = format!("{:?}", semantic.fact_id);
        let current = accepted
            .get(&entity)
            .map(|(semantic, value)| (semantic, value));
        let reduction = reduce_runtime_fact_revision(current, (&semantic, &envelope.value))
            .expect("the RFC 012C reducer accepts what this adapter emits");
        match reduction {
            RuntimeFactReduction::Upsert { semantic, revision } => {
                accepted.insert(entity, (semantic, *revision));
            }
            RuntimeFactReduction::Unchanged => {}
            RuntimeFactReduction::Retract => {
                accepted.remove(&entity);
            }
        }
    }
    accepted
        .into_iter()
        .map(|(entity, (_, value))| (entity, value))
        .collect()
}

/// Decode each record into its own batch, the way the engine's spine does, and
/// hand every batch to `visit`. Used where accumulating a whole transcript in
/// one batch would say more about the test harness than about the decoder.
fn decode_each_record(lines: &[&str], mut visit: impl FnMut(&FactBatch)) {
    let adapter = ClaudeCodeAdapter::new();
    let object_key = format!("{PROJECT}/{SESSION}.jsonl");
    let object_context = transcript_object_context();
    let decoder = crate::adapter::DecoderId::new("claude-session-record").expect("decoder");

    let mut offset = 0_u64;
    let mut decoder_state: Option<Vec<u8>> = None;
    for line in lines {
        let mut batch = FactBatch::new_with_semantic_context(
            512,
            16,
            FactSemanticContext::new(
                &AdapterId::new("claude-code").expect("adapter id"),
                1,
                b"fixture-root",
                b"session-transcripts",
                object_key.as_bytes(),
                1,
            )
            .expect("semantic context"),
        )
        .expect("fact batch");
        let payload = line.as_bytes().to_vec();
        let length = payload.len() as u64;
        let record = crate::source::SourceRecord::new(
            &RecordOrigin {
                source_instance_id: 7,
                stream_id: 8,
                object_id: 9,
                observed_at: 10,
                source_timestamp_hint: Some(1_776_211_200_000),
                media_type: SourceMediaType::new("application/x-ndjson").expect("media type"),
            },
            1,
            SourceCursor::append_offset(offset),
            SourceCursor::append_offset(offset + length + 1),
            0,
            payload,
        );
        offset += length + 1;
        adapter
            .decode(
                DecodeContext {
                    decoder: &decoder,
                    object_context: &object_context,
                    decoder_state: decoder_state.as_deref(),
                },
                &record,
                &mut batch,
            )
            .expect("the adapter decodes a well-formed Claude record");
        decoder_state = batch.next_decoder_state().map(<[u8]>::to_vec);
        visit(&batch);
    }
}

fn assistant_line(uuid: &str, model: &str, blocks: serde_json::Value) -> String {
    serde_json::json!({
        "type": "assistant",
        "uuid": uuid,
        "parentUuid": null,
        "timestamp": "2026-04-01T00:00:00.000Z",
        "sessionId": SESSION,
        "cwd": "/home/u/proj",
        "version": "1.0.0",
        "gitBranch": "main",
        "isSidechain": false,
        "userType": "external",
        "requestId": "req_1",
        "message": {
            "id": format!("msg_{uuid}"),
            "type": "message",
            "role": "assistant",
            "model": model,
            "content": blocks,
            "usage": {"input_tokens": 10, "output_tokens": 5},
        },
    })
    .to_string()
}

fn user_line(uuid: &str, blocks: serde_json::Value) -> String {
    serde_json::json!({
        "type": "user",
        "uuid": uuid,
        "parentUuid": null,
        "timestamp": "2026-04-01T00:00:00.000Z",
        "sessionId": SESSION,
        "cwd": "/home/u/proj",
        "version": "1.0.0",
        "gitBranch": "main",
        "isSidechain": false,
        "userType": "external",
        "message": {"role": "user", "content": blocks},
    })
    .to_string()
}

#[test]
fn an_assistant_turn_becomes_a_message_and_its_content_blocks() {
    let batch = decode_transcript(&[&assistant_line(
        "11111111-1111-1111-1111-111111111111",
        "claude-fable-5",
        serde_json::json!([
            {"type": "text", "text": "Reading the file."},
            {"type": "tool_use", "id": "toolu_1", "name": "Read", "input": {"path": "/tmp/x"}},
        ]),
    )]);

    let messages = reduce_family(&batch, "runtime.message");
    assert_eq!(messages.len(), 1, "one record is one message");
    let Some(Fact::MessageRevision(message)) = messages.values().next() else {
        panic!("the message family reduced to something else");
    };
    assert_eq!(
        message.native_message_id,
        "11111111-1111-1111-1111-111111111111"
    );
    assert_eq!(message.role, MessageRevisionRole::Assistant);
    assert_eq!(message.completeness, ContractCompleteness::Complete);
    assert_eq!(
        message.ordered_content_block_keys,
        vec!["o:0".to_string(), "n:toolu_1".to_string()],
        "the snapshot names every block in order"
    );

    let blocks = reduce_family(&batch, "runtime.content-block");
    assert_eq!(blocks.len(), 2);
    let mut kinds: Vec<String> = blocks
        .values()
        .map(|fact| {
            let Fact::ContentBlockRevision(block) = fact else {
                panic!("not a content block");
            };
            match &block.content {
                ContentBlockRevisionValue::Text { text } => format!("text:{text}"),
                ContentBlockRevisionValue::ToolCall { tool_name, .. } => {
                    format!("tool_call:{tool_name}")
                }
                other => format!("{other:?}"),
            }
        })
        .collect();
    kinds.sort();
    assert_eq!(
        kinds,
        vec![
            "text:Reading the file.".to_string(),
            "tool_call:Read".to_string()
        ]
    );
}

#[test]
fn a_tool_call_and_its_result_are_separate_correlated_entities() {
    let batch = decode_transcript(&[
        &assistant_line(
            "11111111-1111-1111-1111-111111111111",
            "claude-fable-5",
            serde_json::json!([
                {"type": "tool_use", "id": "toolu_1", "name": "Read", "input": {"path": "/tmp/x"}},
            ]),
        ),
        &user_line(
            "22222222-2222-2222-2222-222222222222",
            serde_json::json!([
                {"type": "tool_result", "tool_use_id": "toolu_1", "content": "ok", "is_error": false},
            ]),
        ),
    ]);

    let tools = reduce_family(&batch, "runtime.tool");
    assert_eq!(tools.len(), 2, "a call and a result are two entities");
    let mut by_kind: BTreeMap<String, ToolRevisionFact> = BTreeMap::new();
    for fact in tools.values() {
        let Fact::ToolRevision(tool) = fact else {
            panic!("not a tool fact");
        };
        by_kind.insert(format!("{:?}", tool.kind), tool.clone());
    }

    let call = &by_kind["Call"];
    assert_eq!(call.native_tool_id, "toolu_1");
    assert_eq!(call.tool_name, "Read");
    assert_eq!(
        call.correlated_native_id.as_deref(),
        Some("toolu_1#result"),
        "the result revises the call's correlation without changing its key"
    );

    let result = &by_kind["Result"];
    assert_eq!(result.native_tool_id, "toolu_1#result");
    assert_eq!(
        result.tool_name, "Read",
        "the result is named by the call it answers"
    );
    assert_eq!(result.correlated_native_id.as_deref(), Some("toolu_1"));
}

#[test]
fn a_result_without_its_call_stays_unmatched_rather_than_guessed() {
    let batch = decode_transcript(&[&user_line(
        "22222222-2222-2222-2222-222222222222",
        serde_json::json!([
            {"type": "tool_result", "tool_use_id": "toolu_orphan", "content": "ok"},
        ]),
    )]);

    assert!(
        reduce_family(&batch, "runtime.tool").is_empty(),
        "no tool entity may be claimed for a result whose tool name is unknown"
    );
    let blocks = reduce_family(&batch, "runtime.content-block");
    assert_eq!(
        blocks.len(),
        1,
        "the evidence is retained as a content block"
    );
    let Some(Fact::ContentBlockRevision(block)) = blocks.values().next() else {
        panic!("not a content block");
    };
    assert_eq!(
        block.native_tool_call_or_result_id.as_deref(),
        Some("toolu_orphan"),
        "the unmatched result keeps its native call identity"
    );
}

#[test]
fn a_model_change_is_one_effective_state_revision_per_change() {
    let batch = decode_transcript(&[
        &assistant_line(
            "11111111-1111-1111-1111-111111111111",
            "claude-fable-5",
            serde_json::json!([{"type": "text", "text": "one"}]),
        ),
        &assistant_line(
            "22222222-2222-2222-2222-222222222222",
            "claude-fable-5",
            serde_json::json!([{"type": "text", "text": "two"}]),
        ),
        &assistant_line(
            "33333333-3333-3333-3333-333333333333",
            "claude-opus-5",
            serde_json::json!([{"type": "text", "text": "three"}]),
        ),
    ]);

    let emitted: Vec<&Fact> = batch
        .facts()
        .iter()
        .map(|envelope| &envelope.value)
        .filter(|fact| fact.kind() == "runtime.effective-state")
        .collect();
    assert_eq!(
        emitted.len(),
        2,
        "an unchanged model must not re-emit; only the two distinct values do"
    );

    let state = reduce_family(&batch, "runtime.effective-state");
    assert_eq!(state.len(), 1, "model is one revisioned entity");
    let Some(Fact::EffectiveStateRevision(model)) = state.values().next() else {
        panic!("not an effective-state fact");
    };
    assert_eq!(model.dimension, EffectiveStateDimension::Model);
    assert_eq!(model.value.value.as_deref(), Some("claude-opus-5"));
    assert_eq!(
        model.evidence_kind,
        EffectiveStateEvidenceKind::ResponseObserved,
        "a model on a response proves it was effective for that response"
    );
}

#[test]
fn a_permission_mode_record_is_a_native_transition() {
    let line = serde_json::json!({
        "type": "permission-mode",
        "permissionMode": "acceptEdits",
        "sessionId": SESSION,
    })
    .to_string();
    let batch = decode_transcript(&[&line]);

    let state = reduce_family(&batch, "runtime.effective-state");
    assert_eq!(state.len(), 1);
    let Some(Fact::EffectiveStateRevision(mode)) = state.values().next() else {
        panic!("not an effective-state fact");
    };
    assert_eq!(mode.dimension, EffectiveStateDimension::PermissionMode);
    assert_eq!(mode.value.value.as_deref(), Some("acceptEdits"));
    assert_eq!(
        mode.evidence_kind,
        EffectiveStateEvidenceKind::NativeTransition,
        "a record whose purpose is the mode change is a transition, not an observation"
    );
}

#[test]
fn session_mode_and_permission_mode_stay_separate_dimensions() {
    let line = serde_json::json!({
        "type": "user",
        "uuid": "11111111-1111-1111-1111-111111111111",
        "timestamp": "2026-04-01T00:00:00.000Z",
        "sessionId": SESSION,
        "mode": "plan",
        "permissionMode": "plan",
        "message": {"role": "user", "content": "hi"},
    })
    .to_string();
    let batch = decode_transcript(&[&line]);

    let state = reduce_family(&batch, "runtime.effective-state");
    assert_eq!(
        state.len(),
        2,
        "an equal native value in two dimensions is two entities"
    );
    let dimensions: Vec<EffectiveStateDimension> = state
        .values()
        .map(|fact| {
            let Fact::EffectiveStateRevision(revision) = fact else {
                panic!("not an effective-state fact");
            };
            revision.dimension
        })
        .collect();
    assert!(dimensions.contains(&EffectiveStateDimension::SessionMode));
    assert!(dimensions.contains(&EffectiveStateDimension::PermissionMode));
}

#[test]
fn a_compact_boundary_becomes_a_native_marker() {
    let line = serde_json::json!({
        "type": "system",
        "uuid": "44444444-4444-4444-4444-444444444444",
        "timestamp": "2026-04-01T00:00:00.000Z",
        "sessionId": SESSION,
        "subtype": "compact_boundary",
        "content": "Summary of prior context.",
        "compactMetadata": {"trigger": "manual", "preTokens": 12000},
    })
    .to_string();
    let batch = decode_transcript(&[&line]);

    let markers = reduce_family(&batch, "runtime.native-marker");
    assert_eq!(markers.len(), 1);
    let Some(Fact::NativeRuntimeMarkerRevision(marker)) = markers.values().next() else {
        panic!("not a native marker");
    };
    assert_eq!(
        marker.value,
        NativeRuntimeMarkerValue::Compaction {
            phase: NativeCompactionPhase::Boundary,
            trigger: Some("manual".to_string()),
            pre_tokens: Some(12000),
        }
    );
    assert_eq!(marker.quality, QualifiedValueQuality::NativeClaimed);
    assert_eq!(marker.effective_at, Some(1_776_211_200_000));
}

#[test]
fn a_queue_operation_becomes_a_native_marker() {
    let line = serde_json::json!({
        "type": "queue-operation",
        "operation": "enqueue",
        "timestamp": "2026-04-01T00:00:00.000Z",
        "sessionId": SESSION,
        "content": "queued prompt text",
    })
    .to_string();
    let batch = decode_transcript(&[&line]);

    let markers = reduce_family(&batch, "runtime.native-marker");
    assert_eq!(markers.len(), 1);
    let Some(Fact::NativeRuntimeMarkerRevision(marker)) = markers.values().next() else {
        panic!("not a native marker");
    };
    assert!(matches!(
        marker.value,
        NativeRuntimeMarkerValue::Queue {
            operation: NativeQueueOperation::Enqueue,
            depth: None,
            item_digest: Some(_),
        }
    ));
}

#[test]
fn a_progress_record_proves_activity_and_no_counters() {
    let line = serde_json::json!({
        "type": "progress",
        "uuid": "55555555-5555-5555-5555-555555555555",
        "timestamp": "2026-04-01T00:00:00.000Z",
        "sessionId": SESSION,
        "toolUseID": "toolu_1",
        "data": {"type": "bash_progress", "output": "line 1"},
    })
    .to_string();
    let batch = decode_transcript(&[&line]);

    let markers = reduce_family(&batch, "runtime.native-marker");
    assert_eq!(markers.len(), 1);
    let Some(Fact::NativeRuntimeMarkerRevision(marker)) = markers.values().next() else {
        panic!("not a native marker");
    };
    let NativeRuntimeMarkerValue::Progress {
        state,
        completed,
        total,
        detail_digest,
    } = &marker.value
    else {
        panic!("progress record produced a different marker kind");
    };
    assert_eq!(*state, NativeProgressState::Active);
    assert_eq!(
        (*completed, *total),
        (None, None),
        "absent native counters stay unknown instead of becoming zero"
    );
    assert!(detail_digest.is_some());
    assert_eq!(marker.correlated_native_id.as_deref(), Some("toolu_1"));
}

#[test]
fn ask_user_question_opens_pending_and_its_result_resolves_it() {
    let batch = decode_transcript(&[
        &assistant_line(
            "11111111-1111-1111-1111-111111111111",
            "claude-fable-5",
            serde_json::json!([{
                "type": "tool_use",
                "id": "toolu_q",
                "name": "AskUserQuestion",
                "input": {"questions": [{
                    "header": "Branch",
                    "question": "Which branch should I use?",
                    "multiSelect": false,
                    "options": [{"label": "main"}, {"label": "develop"}],
                }]},
            }]),
        ),
        &user_line(
            "22222222-2222-2222-2222-222222222222",
            serde_json::json!([
                {"type": "tool_result", "tool_use_id": "toolu_q", "content": "main"},
            ]),
        ),
    ]);

    let interactions = reduce_family(&batch, "runtime.user-input-request");
    assert_eq!(interactions.len(), 1, "one interaction, twice revised");
    let Some(Fact::UserInputRequestRevision(interaction)) = interactions.values().next() else {
        panic!("not a user-input request");
    };
    assert_eq!(interaction.native_tool_use_id, "toolu_q");
    assert_eq!(interaction.state, UserInputLifecycleState::Resolved);
    assert_eq!(interaction.kind, UserInputKind::Choice);
    assert_eq!(interaction.questions.len(), 1);
    assert_eq!(interaction.questions[0].header.as_deref(), Some("Branch"));
    assert_eq!(
        interaction.questions[0]
            .options
            .iter()
            .map(|option| option.label.as_str())
            .collect::<Vec<_>>(),
        vec!["main", "develop"],
        "typed options survive to the resolved revision"
    );
}

#[test]
fn an_error_result_fails_the_interaction_rather_than_resolving_it() {
    let batch = decode_transcript(&[
        &assistant_line(
            "11111111-1111-1111-1111-111111111111",
            "claude-fable-5",
            serde_json::json!([{
                "type": "tool_use",
                "id": "toolu_q",
                "name": "AskUserQuestion",
                "input": {"questions": [{"question": "Proceed?", "options": [{"label": "yes"}]}]},
            }]),
        ),
        &user_line(
            "22222222-2222-2222-2222-222222222222",
            serde_json::json!([
                {"type": "tool_result", "tool_use_id": "toolu_q", "content": "no", "is_error": true},
            ]),
        ),
    ]);

    let interactions = reduce_family(&batch, "runtime.user-input-request");
    let Some(Fact::UserInputRequestRevision(interaction)) = interactions.values().next() else {
        panic!("not a user-input request");
    };
    assert_eq!(interaction.state, UserInputLifecycleState::Failed);
}

#[test]
fn an_unanswered_question_stays_pending() {
    let batch = decode_transcript(&[&assistant_line(
        "11111111-1111-1111-1111-111111111111",
        "claude-fable-5",
        serde_json::json!([{
            "type": "tool_use",
            "id": "toolu_q",
            "name": "AskUserQuestion",
            "input": {"questions": [{"question": "Proceed?", "options": [{"label": "yes"}]}]},
        }]),
    )]);

    let interactions = reduce_family(&batch, "runtime.user-input-request");
    let Some(Fact::UserInputRequestRevision(interaction)) = interactions.values().next() else {
        panic!("not a user-input request");
    };
    assert_eq!(
        interaction.state,
        UserInputLifecycleState::Pending,
        "silence never resolves or cancels an interaction"
    );
}

#[test]
fn exit_plan_mode_carries_the_plan_and_its_ordered_steps() {
    let batch = decode_transcript(&[&assistant_line(
        "11111111-1111-1111-1111-111111111111",
        "claude-fable-5",
        serde_json::json!([{
            "type": "tool_use",
            "id": "toolu_plan",
            "name": "ExitPlanMode",
            "input": {"plan": "# Land the adapter\n- Emit the families\n- Prove them\n"},
        }]),
    )]);

    let plans = reduce_family(&batch, "runtime.plan");
    assert_eq!(plans.len(), 1);
    let Some(Fact::PlanRevision(plan)) = plans.values().next() else {
        panic!("not a plan revision");
    };
    assert_eq!(plan.native_plan_id, "toolu_plan");
    assert_eq!(plan.subject, "Land the adapter");
    assert_eq!(
        plan.ordered_step_keys,
        vec![
            "0:Emit the families".to_string(),
            "1:Prove them".to_string()
        ]
    );
    assert_eq!(plan.completeness, ContractCompleteness::Complete);
}

#[test]
fn the_committed_medium_fixture_decodes_into_every_transcript_family() {
    // The one fixture in the repo that contains the full record vocabulary:
    // messages, tool pairs, a permission-mode record, compaction boundaries,
    // progress and queue operations.
    let root =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/medium/.claude/projects");
    let mut families: BTreeMap<&'static str, usize> = BTreeMap::new();
    let mut decoded_any = false;
    for project in std::fs::read_dir(&root)
        .expect("fixture projects")
        .flatten()
    {
        for entry in std::fs::read_dir(project.path())
            .expect("fixture project")
            .flatten()
        {
            let path = entry.path();
            if path
                .extension()
                .is_none_or(|extension| extension != "jsonl")
            {
                continue;
            }
            let content = std::fs::read_to_string(&path).expect("fixture transcript");
            let lines: Vec<&str> = content.lines().filter(|line| !line.is_empty()).collect();
            if lines.is_empty() {
                continue;
            }
            decoded_any = true;
            let batch = decode_transcript(&lines);
            for envelope in batch.facts() {
                if let Some(kind) = [
                    "runtime.message",
                    "runtime.content-block",
                    "runtime.tool",
                    "runtime.effective-state",
                    "runtime.native-marker",
                ]
                .into_iter()
                .find(|kind| *kind == envelope.value.kind())
                {
                    *families.entry(kind).or_default() += 1;
                }
            }
        }
    }

    assert!(decoded_any, "the medium fixture should contain transcripts");
    for family in [
        "runtime.message",
        "runtime.content-block",
        "runtime.tool",
        "runtime.effective-state",
        "runtime.native-marker",
    ] {
        assert!(
            families.get(family).copied().unwrap_or_default() > 0,
            "{family} was never emitted from the committed corpus fixture"
        );
    }
}

#[test]
fn decoder_state_survives_a_round_trip_with_open_work() {
    let mut state = TranscriptRuntimeState::default();
    state.observe_effective_state(EffectiveStateDimension::Model, "claude-fable-5");
    state.observe_effective_state(EffectiveStateDimension::PermissionMode, "acceptEdits");
    state.open_tool("toolu_1", "Read");
    state.open_tool("toolu_2", "Bash");
    state.open_question(
        "toolu_3",
        &[UserInputQuestion {
            header: Some("Pick".to_string()),
            prompt: "Which branch?".to_string(),
            options: vec![UserInputOption {
                label: "main".to_string(),
                description: None,
                preview: None,
            }],
            multi_select: false,
        }],
    );

    let decoded =
        TranscriptRuntimeState::decode(&state.encode()).expect("state decodes what it encodes");
    assert_eq!(decoded, state);
}

#[test]
fn the_open_tool_set_stays_bounded_and_small() {
    let mut state = TranscriptRuntimeState::default();
    for index in 0..(MAX_OPEN_TOOLS + 8) {
        state.open_tool(&format!("toolu_{index}"), "Read");
    }
    assert_eq!(state.open_tools.len(), MAX_OPEN_TOOLS);
    assert_eq!(
        state.close_tool("toolu_0"),
        None,
        "the oldest unresolved calls are the ones dropped"
    );
    assert!(state
        .close_tool(&format!("toolu_{}", MAX_OPEN_TOOLS + 7))
        .is_some());
    assert!(
        state.encode().len() < 4 * 1024,
        "decoder state is written per record, so it must stay small"
    );
}

#[test]
fn an_oversized_tool_name_is_not_retained() {
    let mut state = TranscriptRuntimeState::default();
    state.open_tool("toolu_1", &"n".repeat(MAX_OPEN_TOOL_NAME_BYTES + 1));
    assert_eq!(
        state.close_tool("toolu_1"),
        None,
        "an unretainable name leaves the result unmatched instead of guessed"
    );
}

#[test]
fn a_truncated_or_extended_state_tail_is_a_stream_error() {
    let mut state = TranscriptRuntimeState::default();
    state.open_tool("toolu_1", "Read");
    let encoded = state.encode();
    assert!(TranscriptRuntimeState::decode(&encoded[..encoded.len() - 2]).is_err());

    let mut extended = encoded.clone();
    extended.push(0);
    assert!(TranscriptRuntimeState::decode(&extended).is_err());
}

#[test]
fn content_text_beyond_the_common_bound_is_partial_not_silently_cut() {
    let (text, completeness) = bounded_text(&"a".repeat(MAX_CONTENT_TEXT_BYTES + 10));
    assert_eq!(text.len(), MAX_CONTENT_TEXT_BYTES);
    assert_eq!(completeness, ContractCompleteness::Partial);

    let (text, completeness) = bounded_text("short");
    assert_eq!(text, "short");
    assert_eq!(completeness, ContractCompleteness::Complete);
}

#[test]
fn truncation_lands_on_a_character_boundary() {
    let text = format!("{}é", "a".repeat(MAX_CONTENT_TEXT_BYTES - 1));
    let (bounded, completeness) = bounded_text(&text);
    assert_eq!(completeness, ContractCompleteness::Partial);
    assert_eq!(bounded.len(), MAX_CONTENT_TEXT_BYTES - 1);
    assert!(bounded.chars().all(|character| character == 'a'));
}

#[test]
fn a_native_extension_kind_is_normalized_to_a_machine_identifier() {
    assert_eq!(
        normalized_native_kind("redacted_thinking"),
        "redacted_thinking"
    );
    assert_eq!(normalized_native_kind("Server Tool Use"), "server_tool_use");
    assert_eq!(normalized_native_kind("9lives"), "x_9lives");
    assert!(normalized_native_kind(&"k".repeat(200)).len() <= 66);
}

#[test]
fn a_question_set_that_does_not_normalize_yields_no_interaction() {
    assert!(user_input_questions(&serde_json::json!({})).is_none());
    assert!(user_input_questions(&serde_json::json!({"questions": []})).is_none());
    assert!(
        user_input_questions(&serde_json::json!({"questions": [{"options": []}]})).is_none(),
        "a question with no prompt is not renderable and must not be claimed"
    );
}

#[test]
fn free_text_and_mixed_question_sets_are_classified_apart() {
    let free = vec![UserInputQuestion {
        header: None,
        prompt: "Anything to add?".to_string(),
        options: Vec::new(),
        multi_select: false,
    }];
    assert_eq!(user_input_kind(&free), UserInputKind::FreeText);

    let mut mixed = free.clone();
    mixed.push(UserInputQuestion {
        header: None,
        prompt: "Pick one".to_string(),
        options: vec![UserInputOption {
            label: "a".to_string(),
            description: None,
            preview: None,
        }],
        multi_select: true,
    });
    assert_eq!(user_input_kind(&mixed), UserInputKind::Mixed);
}

#[test]
fn a_summary_record_is_not_a_message_turn() {
    assert_eq!(message_revision_role(&MessageRole::Summary), None);
    assert_eq!(
        message_revision_role(&MessageRole::Other("progress".to_string())),
        None
    );
    assert_eq!(
        message_revision_role(&MessageRole::Assistant),
        Some(MessageRevisionRole::Assistant)
    );
}

#[test]
fn queue_operations_map_only_to_declared_values() {
    assert_eq!(
        queue_operation("enqueue"),
        Some(NativeQueueOperation::Enqueue)
    );
    assert_eq!(queue_operation("drain"), Some(NativeQueueOperation::Drain));
    assert_eq!(
        queue_operation("teleport"),
        None,
        "an unrecognized native operation is not forced into a known one"
    );
}

#[test]
fn plan_steps_come_from_the_document_and_stay_bounded() {
    assert_eq!(
        plan_step_keys("# Title\n- one\n* two\n3. three\nnot a step\n"),
        vec![
            "0:one".to_string(),
            "1:two".to_string(),
            "2:three".to_string()
        ]
    );
    let long: String = (0..64).map(|index| format!("- step {index}\n")).collect();
    assert_eq!(plan_step_keys(&long).len(), 32);
}

#[test]
fn a_todo_status_never_becomes_a_guessed_terminal_state() {
    use crate::adapter::TaskStatus;
    assert_eq!(
        task_lifecycle_state(&TaskStatus::Pending),
        TaskLifecycleState::Created
    );
    assert_eq!(
        task_lifecycle_state(&TaskStatus::Completed),
        TaskLifecycleState::Completed
    );
    assert_eq!(
        task_lifecycle_state(&TaskStatus::Other("blocked".to_string())),
        TaskLifecycleState::Updated,
        "an unrecognized status proves the task was touched, nothing more"
    );
}

/// Decoder throughput and fact-identity digest on one large transcript.
///
/// Ignored because it is a measurement, not an assertion. Run it with
/// `cargo test -p spaghetti-napi decode_throughput -- --ignored --nocapture`,
/// optionally pointing `SPAG_BENCH_TRANSCRIPT` at a real transcript; without
/// it the committed medium fixture is repeated to a comparable size.
///
/// The digest is the point of the second line: an optimization that changes
/// any fact or revision identity changes it, so a before/after pair proves a
/// speedup preserved semantics.
#[test]
#[ignore]
fn decode_throughput_and_fact_identity_digest() {
    let lines: Vec<String> = match std::env::var("SPAG_BENCH_TRANSCRIPT") {
        Ok(path) => std::fs::read_to_string(&path)
            .expect("bench transcript")
            .lines()
            .filter(|line| !line.is_empty())
            .map(str::to_owned)
            .collect(),
        Err(_) => {
            let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("fixtures/medium/.claude/projects");
            let mut source: Vec<String> = Vec::new();
            for project in std::fs::read_dir(&root).expect("fixtures").flatten() {
                for entry in std::fs::read_dir(project.path())
                    .expect("fixture project")
                    .flatten()
                {
                    if entry.path().extension().is_none_or(|ext| ext != "jsonl") {
                        continue;
                    }
                    source.extend(
                        std::fs::read_to_string(entry.path())
                            .expect("fixture")
                            .lines()
                            .filter(|line| !line.is_empty())
                            .map(str::to_owned),
                    );
                }
            }
            let mut lines = Vec::new();
            while lines.len() < 40_000 {
                lines.extend(source.iter().cloned());
            }
            lines
        }
    };

    let bytes: usize = lines.iter().map(|line| line.len() + 1).sum();
    let borrowed: Vec<&str> = lines.iter().map(String::as_str).collect();

    // One batch per record, exactly as the engine's decode spine does it.
    // Reported as the minimum of several rounds: this machine runs other
    // lanes, and the minimum is the round least disturbed by them. A mean
    // would mostly measure the neighbours.
    let rounds: usize = std::env::var("SPAG_BENCH_ROUNDS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(5);
    let mut hasher = blake3::Hasher::new();
    let mut facts = 0_usize;
    let mut elapsed = std::time::Duration::MAX;
    for round in 0..rounds {
        let mut round_facts = 0_usize;
        let mut round_hasher = blake3::Hasher::new();
        let started = std::time::Instant::now();
        decode_each_record(&borrowed, |batch| {
            for envelope in batch.facts() {
                if let Some(semantic) = envelope.semantic_revision {
                    round_facts += 1;
                    round_hasher.update(envelope.value.kind().as_bytes());
                    round_hasher.update(semantic.fact_id.as_bytes());
                    round_hasher.update(semantic.fact_revision_id.as_bytes());
                }
            }
        });
        elapsed = elapsed.min(started.elapsed());
        if round == 0 {
            facts = round_facts;
            hasher = round_hasher;
        }
    }

    let megabytes = bytes as f64 / (1024.0 * 1024.0);
    println!(
        "decode: {:.1} MB / {} records / {} canonical facts in {:.1} ms = {:.2} ms/MB (best of {rounds})",
        megabytes,
        lines.len(),
        facts,
        elapsed.as_secs_f64() * 1000.0,
        elapsed.as_secs_f64() * 1000.0 / megabytes,
    );
    println!("fact identity digest: {}", hasher.finalize().to_hex());
}

// ---------------------------------------------------------------------------
// Revision identity: what makes two facts for one entity two revisions.
//
// Each case below is a shape real transcripts contain that used to derive one
// `FactRevisionId` twice. Emitting it twice is what the durable writer rejects
// with `UNIQUE constraint failed: fact_records.semantic_fact_revision_id`, so
// these decode through the whole batch the way the engine does: a repeated
// identity fails the decode itself, before any assertion runs.
// ---------------------------------------------------------------------------

/// Every canonical revision one transcript emits, in emission order.
fn revisions(batch: &FactBatch, family: &str) -> Vec<FactSemanticRevision> {
    batch
        .facts()
        .iter()
        .filter(|envelope| envelope.value.kind() == family)
        .map(|envelope| {
            envelope
                .semantic_revision
                .expect("a runtime fact always carries a canonical semantic revision")
        })
        .collect()
}

fn assert_one_entity_with_distinct_revisions(
    revisions: &[FactSemanticRevision],
    expected: usize,
    family: &str,
) {
    assert_eq!(revisions.len(), expected, "{family} revision count");
    let entities: BTreeSet<_> = revisions.iter().map(|semantic| semantic.fact_id).collect();
    assert_eq!(
        entities.len(),
        1,
        "{family} must keep one entity across the repeat"
    );
    let identities: BTreeSet<_> = revisions
        .iter()
        .map(|semantic| semantic.fact_revision_id)
        .collect();
    assert_eq!(
        identities.len(),
        expected,
        "{family} gave two revisions of one entity a single identity"
    );
    for semantic in revisions {
        assert_eq!(
            semantic.semantic_revision_ref.fact_revision_id, semantic.fact_revision_id,
            "the public join reference must name the revision it belongs to"
        );
    }
}

/// A session that moves off a model and back is three revisions, not two.
///
/// The third proves the model was effective again; deriving its identity from
/// the value alone made it the first revision's identity, because a returning
/// value normalizes identically.
#[test]
fn a_dimension_returning_to_an_earlier_value_is_a_distinct_revision() {
    let blocks = serde_json::json!([{"type": "text", "text": "ok"}]);
    let batch = decode_transcript(&[
        &assistant_line(
            "11111111-1111-1111-1111-111111111111",
            "model-a",
            blocks.clone(),
        ),
        &assistant_line(
            "22222222-2222-2222-2222-222222222222",
            "model-b",
            blocks.clone(),
        ),
        &assistant_line("33333333-3333-3333-3333-333333333333", "model-a", blocks),
    ]);

    let model = revisions(&batch, "runtime.effective-state");
    assert_one_entity_with_distinct_revisions(&model, 3, "runtime.effective-state");

    let states = reduce_family(&batch, "runtime.effective-state");
    assert_eq!(states.len(), 1, "one actor and dimension is one entity");
    let Some(Fact::EffectiveStateRevision(state)) = states.values().next() else {
        panic!("the effective-state family reduced to something else");
    };
    assert_eq!(state.dimension, EffectiveStateDimension::Model);
    assert_eq!(
        state.value.value.as_deref(),
        Some("model-a"),
        "the last revision wins, and the last evidence is the model it returned to"
    );
}

/// A record repeated verbatim is a second observation of one fact.
///
/// Real transcripts do repeat a line — same `uuid`, byte-identical message —
/// and both occurrences are evidence. They name one message entity and one
/// content-block entity, at two positions in the object.
#[test]
fn a_verbatim_repeated_record_is_a_distinct_revision_of_one_fact() {
    let line = assistant_line(
        "44444444-4444-4444-4444-444444444444",
        "model-a",
        serde_json::json!([{"type": "text", "text": "the same thing twice"}]),
    );
    let batch = decode_transcript(&[&line, &line]);

    assert_one_entity_with_distinct_revisions(
        &revisions(&batch, "runtime.message"),
        2,
        "runtime.message",
    );
    assert_one_entity_with_distinct_revisions(
        &revisions(&batch, "runtime.content-block"),
        2,
        "runtime.content-block",
    );
    assert_eq!(
        reduce_family(&batch, "runtime.message").len(),
        1,
        "a repeated record does not create a second message"
    );
}

/// A `tool_use` id repeated with identical input keeps one call entity.
#[test]
fn a_repeated_tool_call_is_a_distinct_revision_of_one_call() {
    let line = assistant_line(
        "55555555-5555-5555-5555-555555555555",
        "model-a",
        serde_json::json!([
            {"type": "tool_use", "id": "toolu_repeat", "name": "Read", "input": {"path": "p"}},
        ]),
    );
    let second = assistant_line(
        "66666666-6666-6666-6666-666666666666",
        "model-a",
        serde_json::json!([
            {"type": "tool_use", "id": "toolu_repeat", "name": "Read", "input": {"path": "p"}},
        ]),
    );
    let batch = decode_transcript(&[&line, &second]);

    assert_one_entity_with_distinct_revisions(
        &revisions(&batch, "runtime.tool"),
        2,
        "runtime.tool",
    );
    let tools = reduce_family(&batch, "runtime.tool");
    assert_eq!(tools.len(), 1, "one native call id is one tool entity");
}

/// A permission mode that toggles and returns behaves like the model does.
#[test]
fn a_permission_mode_returning_to_an_earlier_value_is_a_distinct_revision() {
    fn moded(uuid: &str, mode: &str) -> String {
        let mut record: serde_json::Value = serde_json::from_str(&assistant_line(
            uuid,
            "model-a",
            serde_json::json!([{"type": "text", "text": "ok"}]),
        ))
        .expect("assistant record");
        record["permissionMode"] = serde_json::Value::String(mode.to_string());
        record.to_string()
    }
    let batch = decode_transcript(&[
        &moded("77777777-7777-7777-7777-777777777777", "default"),
        &moded("88888888-8888-8888-8888-888888888888", "acceptEdits"),
        &moded("99999999-9999-9999-9999-999999999999", "default"),
    ]);

    let all = revisions(&batch, "runtime.effective-state");
    // One model revision (the model never changes) plus three permission-mode
    // revisions; the identities must all be distinct.
    let identities: BTreeSet<_> = all
        .iter()
        .map(|semantic| semantic.fact_revision_id)
        .collect();
    assert_eq!(
        identities.len(),
        all.len(),
        "two revisions shared one identity"
    );
    let permission: Vec<_> = all
        .iter()
        .copied()
        .filter(|semantic| {
            batch.facts().iter().any(|envelope| {
                envelope.semantic_revision == Some(*semantic)
                    && matches!(
                        &envelope.value,
                        Fact::EffectiveStateRevision(fact)
                            if fact.dimension == EffectiveStateDimension::PermissionMode
                    )
            })
        })
        .collect();
    assert_one_entity_with_distinct_revisions(&permission, 3, "permission mode");
}

/// The general invariant, over a transcript built to repeat itself.
///
/// Recurring shapes are the point: the same tool called with the same input,
/// a model that oscillates, and records repeated verbatim. Every fact must
/// still carry its own revision identity — for the families that bind their
/// record — and the repeated shapes must really land on shared entities,
/// otherwise the test would pass by emitting unrelated facts.
///
/// `runtime.usage-v2` is the deliberate counterexample and is asserted as
/// such: re-asserted usage *is* one revision, and its commit path is written
/// to ignore the repeat rather than to distinguish it.
#[test]
fn a_repetitive_transcript_gives_every_fact_its_own_revision_identity() {
    let mut lines: Vec<String> = Vec::new();
    for turn in 0..300_u32 {
        let uuid = format!("{turn:08x}-0000-0000-0000-000000000000");
        // Two models and two tool inputs, so both recur every other turn.
        let model = if turn % 2 == 0 { "model-a" } else { "model-b" };
        let path = if turn % 4 < 2 { "alpha" } else { "beta" };
        let call = format!("toolu_{}", turn % 8);
        lines.push(assistant_line(
            &uuid,
            model,
            serde_json::json!([
                {"type": "text", "text": "same text every turn"},
                {"type": "tool_use", "id": call, "name": "Read", "input": {"path": path}},
            ]),
        ));
        lines.push(user_line(
            &format!("{turn:08x}-1111-0000-0000-000000000000"),
            serde_json::json!([
                {"type": "tool_result", "tool_use_id": call, "content": "same result"},
            ]),
        ));
        if turn % 10 == 0 {
            // A verbatim repeat of the turn just written.
            lines.push(lines[lines.len() - 2].clone());
        }
    }
    let borrowed: Vec<&str> = lines.iter().map(String::as_str).collect();

    const RECORD_BOUND: [&str; 8] = [
        "runtime.message",
        "runtime.content-block",
        "runtime.tool",
        "runtime.user-input-request",
        "runtime.effective-state",
        "runtime.native-marker",
        "runtime.task",
        "runtime.plan",
    ];

    let mut identities: BTreeSet<Vec<u8>> = BTreeSet::new();
    let mut entities: BTreeMap<Vec<u8>, usize> = BTreeMap::new();
    let mut usage_identities: BTreeSet<Vec<u8>> = BTreeSet::new();
    let mut facts = 0_usize;
    let mut repeated_usage = 0_usize;
    decode_each_record(&borrowed, |batch| {
        for envelope in batch.facts() {
            let Some(semantic) = envelope.semantic_revision else {
                continue;
            };
            let identity = semantic.fact_revision_id.as_bytes().to_vec();
            if envelope.value.kind() == "runtime.usage-v2" {
                if !usage_identities.insert(identity) {
                    repeated_usage += 1;
                }
                continue;
            }
            if !RECORD_BOUND.contains(&envelope.value.kind()) {
                continue;
            }
            facts += 1;
            assert!(
                identities.insert(identity),
                "two facts claimed one revision identity in {}",
                envelope.value.kind()
            );
            *entities
                .entry(semantic.fact_id.as_bytes().to_vec())
                .or_default() += 1;
        }
    });

    assert!(
        facts > 2_000,
        "the generated transcript should be substantial"
    );
    assert!(
        entities.values().any(|revisions| *revisions > 1),
        "the generated transcript must actually revisit entities"
    );
    assert!(
        repeated_usage > 0,
        "identical usage must stay one revision; this transcript repeats it"
    );
}

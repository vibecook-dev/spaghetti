//! `RuntimeSemanticValue` must serialize exactly like the `Fact` it mirrors.
//!
//! The observer puts `serde_json::to_value(&Fact)` on the wire and the
//! generated TypeScript describes `RuntimeSemanticValue`. If those two shapes
//! ever diverge the SDK's types become a confident lie, so every family is
//! checked here rather than trusted to review.

use serde_json::{json, Value};

use super::*;
use crate::adapter::{CanonicalEntityKey, CanonicalSourceInstanceKey};

fn key(kind: &str, seed: &[u8]) -> String {
    let instance = CanonicalSourceInstanceKey::derive(1, b"fixture").expect("instance key");
    let key = CanonicalEntityKey::derive("claude-code", &instance, kind, seed).expect("entity key");
    serde_json::to_value(key)
        .expect("canonical keys serialize as opaque strings")
        .as_str()
        .expect("opaque reference is a string")
        .to_string()
}

/// One minimal, valid fact per runtime family, built from the wire JSON so the
/// fixture cannot drift from the shape `Fact` actually accepts.
fn runtime_facts() -> Vec<(&'static str, Fact)> {
    let session = key("session", b"session");
    let actor = key("run", b"actor");
    let message = key("message", b"message");
    let cases: Vec<(&'static str, Value)> = vec![
        (
            "runtime.actor-run",
            json!({"ActorRunRevision": {
                "actor_run": actor, "session": session, "role": "root",
                "parent_actor_run": null, "native_session_id": "s-1",
                "native_actor_id": null, "native_actor_type": null,
            }}),
        ),
        (
            "runtime.message",
            json!({"MessageRevision": {
                "session": session, "actor_run": actor, "native_message_id": "uuid-1",
                "role": "assistant", "ordered_content_block_keys": ["o:0"],
                "completeness": "complete", "operation": "upsert",
            }}),
        ),
        (
            "runtime.content-block",
            json!({"ContentBlockRevision": {
                "session": session, "actor_run": actor, "message": message,
                "native_content_block_id": null, "ordinal": 0,
                "content": {"kind": "text", "text": "hello"},
                "native_tool_call_or_result_id": null,
                "completeness": "complete", "operation": "upsert",
            }}),
        ),
        (
            "runtime.tool",
            json!({"ToolRevision": {
                "session": session, "actor_run": actor, "native_tool_id": "toolu_1",
                "kind": "call", "tool_name": "Read", "correlated_native_id": null,
                "completeness": "complete", "operation": "upsert",
            }}),
        ),
        (
            "runtime.task",
            json!({"TaskRevision": {
                "session": session, "actor_run": actor, "native_task_id": "task-1",
                "subject": "Prove it", "state": "created",
                "completeness": "complete", "operation": "upsert", "owned_set": null,
            }}),
        ),
        (
            "runtime.plan",
            json!({"PlanRevision": {
                "session": session, "actor_run": actor, "native_plan_id": "plan-1",
                "subject": "Land it", "ordered_step_keys": ["0:one"],
                "completeness": "complete", "operation": "upsert", "owned_set": null,
            }}),
        ),
        (
            "runtime.native-marker",
            json!({"NativeRuntimeMarkerRevision": {
                "session": session, "actor_run": actor, "native_marker_id": "marker-1",
                "correlated_native_id": null,
                "value": {"kind": "compaction", "phase": "boundary", "trigger": "manual", "pre_tokens": 12000},
                "quality": "native_claimed", "effective_at": 1_776_211_200_000_i64,
                "provenance": {"native_field": "system.compact_boundary", "normalization_contract_version": 1},
                "completeness": "complete", "operation": "upsert",
            }}),
        ),
        (
            "runtime.user-input-request",
            json!({"UserInputRequestRevision": {
                "session": session, "actor_run": actor, "native_tool_use_id": "toolu_q",
                "kind": "choice",
                "questions": [{"header": null, "prompt": "Which?", "options": [], "multi_select": false}],
                "state": "pending", "operation": "upsert", "completeness": "complete",
                "result_reference": null,
            }}),
        ),
        (
            "runtime.effective-state",
            json!({"EffectiveStateRevision": {
                "session": session, "actor_run": actor, "dimension": "model",
                "value": {
                    "value": "claude-fable-5", "quality": "native_claimed",
                    "authority": "native_response", "completeness": "complete",
                    "provenance": {"native_field": "message.model", "normalization_contract_version": 1},
                },
                "evidence_kind": "response_observed",
                "completeness": "complete", "operation": "upsert",
            }}),
        ),
    ];

    cases
        .into_iter()
        .map(|(kind, value)| {
            let fact: Fact = serde_json::from_value(value)
                .unwrap_or_else(|error| panic!("{kind} fixture is not a valid Fact: {error}"));
            (kind, fact)
        })
        .collect()
}

#[test]
fn every_runtime_family_serializes_identically_through_both_types() {
    for (kind, fact) in runtime_facts() {
        assert_eq!(fact.kind(), kind, "fixture is the family it claims");
        let value = RuntimeSemanticValue::from_fact(&fact)
            .unwrap_or_else(|| panic!("{kind} has no runtime value mapping"));
        assert_eq!(
            serde_json::to_value(&fact).expect("fact serializes"),
            serde_json::to_value(&value).expect("runtime value serializes"),
            "{kind} would reach TypeScript in a shape the generated type does not describe",
        );
    }
}

#[test]
fn a_durable_only_fact_has_no_runtime_value() {
    let fact = Fact::UnknownRecord {
        native_kind: Some("attachment".to_string()),
        raw_payload: b"{}".to_vec(),
        reason: "unmapped native family".to_string(),
    };
    assert!(
        RuntimeSemanticValue::from_fact(&fact).is_none(),
        "a fact no observer carries must not acquire a runtime wire type"
    );
}

#[test]
fn the_mapping_covers_every_family_the_observer_can_deliver() {
    // The observer's family list and this mapping have to agree; a family
    // added to one and not the other is exactly the gap this catches.
    let covered: Vec<&'static str> = runtime_facts()
        .iter()
        .map(|(kind, _)| *kind)
        .chain(["runtime.actor-affiliation", "runtime.usage-v2"])
        .collect();
    for family in crate::observer::ObserverFamily::ALL {
        let name = family.as_str();
        assert!(
            covered.contains(&name),
            "{name} is on the observer wire with no RuntimeSemanticValue variant"
        );
    }
}

//! RFC 012C v1 portable value-contract fixture.
//!
//! This module is a test-only cross-language oracle for already-landed actor,
//! affiliation, and usage-v2 values. It does not define observer envelopes,
//! epochs, replacement snapshots, or durable query pages.

use serde::{Deserialize, Serialize};

use crate::adapter::{
    ActorAffiliationDimension, ActorAffiliationRevisionFact, ActorAffiliationState,
    ActorRunRevisionFact, ActorRunRole, AdapterId, CanonicalEntityKey, CanonicalFactId,
    CanonicalSourceInstanceKey, ContractCompleteness, ExternalEntityRef, Fact, FactBatch,
    FactSemanticContext, FactSemanticRevision, QualifiedTimestamp, QualifiedUnknownReason,
    QualifiedValue, QualifiedValueQuality, SemanticRevisionRef, SourceRecordId, TimestampQuality,
    UsageBucketsV2, UsageQualifiedValue, UsageResponseIdentity, UsageRevisionV2Fact,
    UsageValueAuthority, UsageValueProvenance,
};
use crate::source::{RecordOrigin, SourceCursor, SourceMediaType, SourceRecord};

const FIXTURE_CONTRACT_VERSION: u32 = 1;
const RUNTIME_SEMANTIC_CONTRACT_VERSION: u32 = 1;
const ACTOR_RUN_FAMILY: &str = "runtime.actor-run";
const ACTOR_AFFILIATION_FAMILY: &str = "runtime.actor-affiliation";
const USAGE_V2_FAMILY: &str = "runtime.usage-v2";
const FAMILY_VERSION: u32 = 1;
const ADAPTER_ID: &str = "fixture-adapter";
const SOURCE_DISCRIMINATOR: &[u8] = b"fixture-source-instance";
const STREAM_KEY: &[u8] = b"transcript";
const OBJECT_KEY: &[u8] = b"session.jsonl";
const SESSION_NATIVE_ID: &str = "fixture-session-1";
const ROOT_ACTOR_NATIVE_ID: &str = "fixture-root-actor";
const CHILD_ACTOR_NATIVE_ID: &str = "fixture-child-actor";
const TEAM_NATIVE_ID: &str = "fixture-team-1";
const TEAM_MEMBER_NATIVE_ID: &str = "fixture-team-member-1";
const WORKFLOW_NATIVE_ID: &str = "fixture-workflow-1";
const NATIVE_MESSAGE_ID: &str = "msg_fixture_native_1";
const ABA_MESSAGE_ID: &str = "msg_fixture_aba";
const FALLBACK_RESPONSE_KEY: &[u8] = b"fixture-source-record-fallback-1";
const REQUEST_ID: &str = "req_fixture_1";
const COMMITTED_FIXTURE: &str = include_str!("../../fixtures/contracts/rfc012c-runtime-v1.json");

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct FamilyVersionWire {
    family: String,
    version: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct SessionIdentityWire {
    entity_key: CanonicalEntityKey,
    external_ref: ExternalEntityRef,
    native_session_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct SourceWire {
    adapter_id: String,
    source_instance_key: CanonicalSourceInstanceKey,
    session: SessionIdentityWire,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct ActorExampleWire {
    family: String,
    family_version: u32,
    revision: ActorRunRevisionFact,
    fact_id: CanonicalFactId,
    source_record_id: SourceRecordId,
    semantic_revision_ref: SemanticRevisionRef,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct ActorsWire {
    root: ActorExampleWire,
    child: ActorExampleWire,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct AffiliationExampleWire {
    family: String,
    family_version: u32,
    revision: ActorAffiliationRevisionFact,
    fact_id: CanonicalFactId,
    source_record_id: SourceRecordId,
    semantic_revision_ref: SemanticRevisionRef,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct AffiliationsWire {
    child_team_present: AffiliationExampleWire,
    child_workflow_present: AffiliationExampleWire,
    child_workflow_removed: AffiliationExampleWire,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct UsageExampleWire {
    family: String,
    family_version: u32,
    revision: UsageRevisionV2Fact,
    semantic_revision_key_hex: String,
    fact_id: CanonicalFactId,
    source_record_id: SourceRecordId,
    semantic_revision_ref: SemanticRevisionRef,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct UsageAbaWire {
    native_message_id: String,
    a: UsageExampleWire,
    b: UsageExampleWire,
    a_repeat: UsageExampleWire,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct UsageWire {
    native_message: UsageExampleWire,
    source_record_fallback: UsageExampleWire,
    response_revisions: UsageAbaWire,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct RuntimeContractFixtureWire {
    fixture_contract_version: u32,
    runtime_semantic_contract_version: u32,
    families: Vec<FamilyVersionWire>,
    source: SourceWire,
    actors: ActorsWire,
    affiliations: AffiliationsWire,
    usage: UsageWire,
}

fn hex_digest(bytes: &[u8; 32]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn fixture_record() -> SourceRecord {
    SourceRecord::new(
        &RecordOrigin {
            source_instance_id: 1,
            stream_id: 2,
            object_id: 3,
            observed_at: 4,
            source_timestamp_hint: None,
            media_type: SourceMediaType::new("application/json").unwrap(),
        },
        1,
        SourceCursor::append_offset(0),
        SourceCursor::append_offset(3),
        0,
        b"{}".to_vec(),
    )
}

fn fixture_context() -> FactSemanticContext {
    FactSemanticContext::new(
        &AdapterId::new(ADAPTER_ID).unwrap(),
        1,
        SOURCE_DISCRIMINATOR,
        STREAM_KEY,
        OBJECT_KEY,
        1,
    )
    .unwrap()
}

fn fixture_batch(context: FactSemanticContext) -> FactBatch {
    FactBatch::new_with_semantic_context(8, 2, context).unwrap()
}

fn exact_tokens(value: u64, native_field: &str) -> UsageQualifiedValue<u64> {
    QualifiedValue::from_parts(
        Some(value),
        QualifiedValueQuality::Exact,
        UsageValueAuthority::NativeResponse,
        ContractCompleteness::Complete,
        None,
        None,
        UsageValueProvenance {
            native_field: native_field.to_string(),
            normalization_contract_version: 1,
        },
    )
    .unwrap()
}

fn missing_tokens(native_field: &str) -> UsageQualifiedValue<u64> {
    QualifiedValue::from_parts(
        None,
        QualifiedValueQuality::Unknown,
        UsageValueAuthority::NativeResponse,
        ContractCompleteness::Unknown,
        Some(QualifiedUnknownReason::Missing),
        None,
        UsageValueProvenance {
            native_field: native_field.to_string(),
            normalization_contract_version: 1,
        },
    )
    .unwrap()
}

fn exact_text(value: &str, native_field: &str) -> UsageQualifiedValue<String> {
    QualifiedValue::from_parts(
        Some(value.to_string()),
        QualifiedValueQuality::Exact,
        UsageValueAuthority::NativeResponse,
        ContractCompleteness::Complete,
        None,
        None,
        UsageValueProvenance {
            native_field: native_field.to_string(),
            normalization_contract_version: 1,
        },
    )
    .unwrap()
}

fn claimed_effort(value: &str) -> UsageQualifiedValue<String> {
    QualifiedValue::from_parts(
        Some(value.to_string()),
        QualifiedValueQuality::NativeClaimed,
        UsageValueAuthority::AdapterDerived,
        ContractCompleteness::Partial,
        None,
        None,
        UsageValueProvenance {
            native_field: "message.effort".to_string(),
            normalization_contract_version: 1,
        },
    )
    .unwrap()
}

fn usage_buckets(input: u64, output: u64, cache_read_unknown: bool) -> UsageBucketsV2 {
    UsageBucketsV2 {
        input_tokens: exact_tokens(input, "message.usage.input_tokens"),
        output_tokens: exact_tokens(output, "message.usage.output_tokens"),
        cache_creation_input_tokens: exact_tokens(0, "message.usage.cache_creation_input_tokens"),
        cache_read_input_tokens: if cache_read_unknown {
            missing_tokens("message.usage.cache_read_input_tokens")
        } else {
            exact_tokens(0, "message.usage.cache_read_input_tokens")
        },
    }
}

fn usage_revision(
    session: CanonicalEntityKey,
    actor_run: CanonicalEntityKey,
    native_message_id: Option<&str>,
    request_id: Option<&str>,
    buckets: UsageBucketsV2,
    model: Option<UsageQualifiedValue<String>>,
    effort: Option<UsageQualifiedValue<String>>,
) -> UsageRevisionV2Fact {
    let (response_key, response_identity, native_message_id) = match native_message_id {
        Some(native_message_id) => (
            native_message_id.as_bytes().to_vec(),
            UsageResponseIdentity::NativeMessageId,
            Some(native_message_id.to_string()),
        ),
        None => (
            FALLBACK_RESPONSE_KEY.to_vec(),
            UsageResponseIdentity::SourceRecordFallback,
            None,
        ),
    };
    UsageRevisionV2Fact {
        session,
        actor_run,
        response_key,
        response_identity,
        native_message_id,
        request_id: request_id.map(str::to_string),
        buckets,
        model,
        effort,
        source_time: None,
    }
}

fn emit_native(
    context: &FactSemanticContext,
    record: &SourceRecord,
    stable_key: &[u8],
    value: Fact,
) -> FactSemanticRevision {
    let mut batch = fixture_batch(context.clone());
    batch.push_native(record, stable_key, value).unwrap();
    batch.facts()[0].semantic_revision.unwrap()
}

fn emit_native_revision(
    context: &FactSemanticContext,
    record: &SourceRecord,
    stable_key: &[u8],
    revision_key: &[u8],
    value: Fact,
) -> FactSemanticRevision {
    let mut batch = fixture_batch(context.clone());
    batch
        .push_native_with_revision(record, stable_key, revision_key, value)
        .unwrap();
    batch.facts()[0].semantic_revision.unwrap()
}

fn emit_usage(
    context: &FactSemanticContext,
    record: &SourceRecord,
    revision: UsageRevisionV2Fact,
) -> UsageExampleWire {
    let semantic_revision_key = revision.semantic_revision_key().unwrap();
    let mut batch = fixture_batch(context.clone());
    batch
        .push_native_object_scoped_with_revision(
            record,
            &revision.response_key,
            &semantic_revision_key,
            Fact::UsageRevisionV2(revision.clone()),
        )
        .unwrap();
    let semantic = batch.facts()[0].semantic_revision.unwrap();
    UsageExampleWire {
        family: USAGE_V2_FAMILY.to_string(),
        family_version: FAMILY_VERSION,
        revision,
        semantic_revision_key_hex: hex_digest(&semantic_revision_key),
        fact_id: semantic.fact_id,
        source_record_id: semantic.source_record_id,
        semantic_revision_ref: semantic.semantic_revision_ref,
    }
}

fn actor_example(
    context: &FactSemanticContext,
    record: &SourceRecord,
    revision: ActorRunRevisionFact,
    stable_key: &[u8],
) -> ActorExampleWire {
    revision.validate().unwrap();
    let semantic = emit_native(
        context,
        record,
        stable_key,
        Fact::ActorRunRevision(revision.clone()),
    );
    ActorExampleWire {
        family: ACTOR_RUN_FAMILY.to_string(),
        family_version: FAMILY_VERSION,
        revision,
        fact_id: semantic.fact_id,
        source_record_id: semantic.source_record_id,
        semantic_revision_ref: semantic.semantic_revision_ref,
    }
}

fn affiliation_example(
    context: &FactSemanticContext,
    record: &SourceRecord,
    revision: ActorAffiliationRevisionFact,
    stable_key: &[u8],
    revision_key: &[u8],
) -> AffiliationExampleWire {
    revision.validate().unwrap();
    let semantic = emit_native_revision(
        context,
        record,
        stable_key,
        revision_key,
        Fact::ActorAffiliationRevision(revision.clone()),
    );
    AffiliationExampleWire {
        family: ACTOR_AFFILIATION_FAMILY.to_string(),
        family_version: FAMILY_VERSION,
        revision,
        fact_id: semantic.fact_id,
        source_record_id: semantic.source_record_id,
        semantic_revision_ref: semantic.semantic_revision_ref,
    }
}

fn expected_fixture() -> RuntimeContractFixtureWire {
    let context = fixture_context();
    let record = fixture_record();
    let keys = fixture_batch(context.clone());
    let session = keys
        .canonical_entity_key("session", SESSION_NATIVE_ID.as_bytes())
        .unwrap();
    let root_actor = keys
        .canonical_entity_key("run", ROOT_ACTOR_NATIVE_ID.as_bytes())
        .unwrap();
    let child_actor = keys
        .canonical_entity_key("run", CHILD_ACTOR_NATIVE_ID.as_bytes())
        .unwrap();
    let team = keys
        .canonical_entity_key("team", TEAM_NATIVE_ID.as_bytes())
        .unwrap();
    let team_member = keys
        .canonical_entity_key("team_member", TEAM_MEMBER_NATIVE_ID.as_bytes())
        .unwrap();
    let workflow = keys
        .canonical_entity_key("workflow", WORKFLOW_NATIVE_ID.as_bytes())
        .unwrap();
    let team_affiliation = keys
        .canonical_entity_key(
            "actor_affiliation",
            b"fixture-child-actor/team/fixture-team-1",
        )
        .unwrap();
    let workflow_affiliation = keys
        .canonical_entity_key(
            "actor_affiliation",
            b"fixture-child-actor/workflow/fixture-workflow-1",
        )
        .unwrap();

    let root = actor_example(
        &context,
        &record,
        ActorRunRevisionFact {
            actor_run: root_actor,
            session,
            role: ActorRunRole::Root,
            parent_actor_run: None,
            native_session_id: Some(SESSION_NATIVE_ID.to_string()),
            native_actor_id: Some(ROOT_ACTOR_NATIVE_ID.to_string()),
            native_actor_type: None,
        },
        ROOT_ACTOR_NATIVE_ID.as_bytes(),
    );
    let child = actor_example(
        &context,
        &record,
        ActorRunRevisionFact {
            actor_run: child_actor,
            session,
            role: ActorRunRole::Child,
            parent_actor_run: Some(root_actor),
            native_session_id: Some(SESSION_NATIVE_ID.to_string()),
            native_actor_id: Some(CHILD_ACTOR_NATIVE_ID.to_string()),
            native_actor_type: None,
        },
        CHILD_ACTOR_NATIVE_ID.as_bytes(),
    );

    let child_team_present = affiliation_example(
        &context,
        &record,
        ActorAffiliationRevisionFact {
            affiliation: team_affiliation,
            actor_run: child_actor,
            session,
            dimension: ActorAffiliationDimension::Team,
            target: team,
            member: Some(team_member),
            native_target_id: Some(TEAM_NATIVE_ID.to_string()),
            native_member_id: Some(TEAM_MEMBER_NATIVE_ID.to_string()),
            state: ActorAffiliationState::Present,
            effective_at: None,
        },
        b"fixture-child-actor/team/fixture-team-1",
        b"affiliation-state-present",
    );
    let workflow_revision = ActorAffiliationRevisionFact {
        affiliation: workflow_affiliation,
        actor_run: child_actor,
        session,
        dimension: ActorAffiliationDimension::Workflow,
        target: workflow,
        member: None,
        native_target_id: Some(WORKFLOW_NATIVE_ID.to_string()),
        native_member_id: None,
        state: ActorAffiliationState::Present,
        effective_at: None,
    };
    let child_workflow_present = affiliation_example(
        &context,
        &record,
        workflow_revision.clone(),
        b"fixture-child-actor/workflow/fixture-workflow-1",
        b"affiliation-state-present",
    );
    let child_workflow_removed = affiliation_example(
        &context,
        &record,
        ActorAffiliationRevisionFact {
            state: ActorAffiliationState::Removed,
            ..workflow_revision
        },
        b"fixture-child-actor/workflow/fixture-workflow-1",
        b"affiliation-state-removed",
    );

    let native_message = emit_usage(
        &context,
        &record,
        usage_revision(
            session,
            child_actor,
            Some(NATIVE_MESSAGE_ID),
            Some(REQUEST_ID),
            usage_buckets(0, 42, true),
            Some(exact_text("fixture-model-1", "message.model")),
            Some(claimed_effort("high")),
        ),
    );
    let source_record_fallback = emit_usage(
        &context,
        &record,
        usage_revision(
            session,
            root_actor,
            None,
            None,
            usage_buckets(7, 3, false),
            None,
            None,
        ),
    );
    let aba_a = emit_usage(
        &context,
        &record,
        usage_revision(
            session,
            child_actor,
            Some(ABA_MESSAGE_ID),
            Some(REQUEST_ID),
            usage_buckets(10, 2, false),
            Some(exact_text("fixture-model-1", "message.model")),
            None,
        ),
    );
    let aba_b = emit_usage(
        &context,
        &record,
        usage_revision(
            session,
            child_actor,
            Some(ABA_MESSAGE_ID),
            Some(REQUEST_ID),
            usage_buckets(20, 2, false),
            Some(exact_text("fixture-model-1", "message.model")),
            None,
        ),
    );

    RuntimeContractFixtureWire {
        fixture_contract_version: FIXTURE_CONTRACT_VERSION,
        runtime_semantic_contract_version: RUNTIME_SEMANTIC_CONTRACT_VERSION,
        families: vec![
            FamilyVersionWire {
                family: ACTOR_RUN_FAMILY.to_string(),
                version: FAMILY_VERSION,
            },
            FamilyVersionWire {
                family: ACTOR_AFFILIATION_FAMILY.to_string(),
                version: FAMILY_VERSION,
            },
            FamilyVersionWire {
                family: USAGE_V2_FAMILY.to_string(),
                version: FAMILY_VERSION,
            },
        ],
        source: SourceWire {
            adapter_id: ADAPTER_ID.to_string(),
            source_instance_key: context.source_instance_key(),
            session: SessionIdentityWire {
                entity_key: session,
                external_ref: ExternalEntityRef::new(session),
                native_session_id: SESSION_NATIVE_ID.to_string(),
            },
        },
        actors: ActorsWire { root, child },
        affiliations: AffiliationsWire {
            child_team_present,
            child_workflow_present,
            child_workflow_removed,
        },
        usage: UsageWire {
            native_message,
            source_record_fallback,
            response_revisions: UsageAbaWire {
                native_message_id: ABA_MESSAGE_ID.to_string(),
                a: aba_a.clone(),
                b: aba_b,
                a_repeat: aba_a,
            },
        },
    }
}

fn committed_fixture() -> RuntimeContractFixtureWire {
    serde_json::from_str(COMMITTED_FIXTURE).expect("RFC 012C fixture must deserialize")
}

fn assert_usage_revision_identity(example: &UsageExampleWire) {
    let recomputed = example.revision.semantic_revision_key().unwrap();
    assert_eq!(hex_digest(&recomputed), example.semantic_revision_key_hex);
    let expected_ref = SemanticRevisionRef::new(
        crate::adapter::FactRevisionId::derive(&example.fact_id, 1, &recomputed).unwrap(),
    );
    assert_eq!(expected_ref, example.semantic_revision_ref);
}

#[test]
fn portable_runtime_fixture_matches_rust_constructed_values() {
    let expected = expected_fixture();
    let committed = committed_fixture();
    assert_eq!(committed, expected);

    let reserialized = serde_json::to_string(&committed).unwrap();
    let round_trip: RuntimeContractFixtureWire = serde_json::from_str(&reserialized).unwrap();
    assert_eq!(round_trip, committed);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(COMMITTED_FIXTURE).unwrap(),
        serde_json::to_value(&expected).unwrap()
    );
}

#[test]
fn portable_runtime_fixture_covers_required_value_cases() {
    let fixture = expected_fixture();
    assert_eq!(fixture.fixture_contract_version, FIXTURE_CONTRACT_VERSION);
    assert_eq!(
        fixture.runtime_semantic_contract_version,
        RUNTIME_SEMANTIC_CONTRACT_VERSION
    );
    assert_eq!(
        fixture
            .families
            .iter()
            .map(|family| (family.family.as_str(), family.version))
            .collect::<Vec<_>>(),
        vec![
            (ACTOR_RUN_FAMILY, FAMILY_VERSION),
            (ACTOR_AFFILIATION_FAMILY, FAMILY_VERSION),
            (USAGE_V2_FAMILY, FAMILY_VERSION),
        ]
    );

    let root = &fixture.actors.root.revision;
    let child = &fixture.actors.child.revision;
    assert_eq!(root.role, ActorRunRole::Root);
    assert_eq!(root.parent_actor_run, None);
    assert_eq!(child.role, ActorRunRole::Child);
    assert_eq!(child.parent_actor_run, Some(root.actor_run));
    assert_ne!(child.actor_run, root.actor_run);
    assert_eq!(child.session, root.session);
    assert_eq!(child.session, fixture.source.session.entity_key);
    assert_eq!(
        fixture.source.session.external_ref.entity_key,
        fixture.source.session.entity_key
    );

    let team = &fixture.affiliations.child_team_present.revision;
    let workflow = &fixture.affiliations.child_workflow_present.revision;
    let removed = &fixture.affiliations.child_workflow_removed.revision;
    assert_eq!(team.actor_run, child.actor_run);
    assert_eq!(workflow.actor_run, child.actor_run);
    assert_eq!(team.dimension, ActorAffiliationDimension::Team);
    assert_eq!(workflow.dimension, ActorAffiliationDimension::Workflow);
    assert_eq!(team.state, ActorAffiliationState::Present);
    assert_eq!(workflow.state, ActorAffiliationState::Present);
    assert_eq!(removed.state, ActorAffiliationState::Removed);
    assert_eq!(removed.affiliation, workflow.affiliation);
    assert_eq!(
        fixture.affiliations.child_workflow_present.fact_id,
        fixture.affiliations.child_workflow_removed.fact_id
    );
    assert_ne!(
        fixture
            .affiliations
            .child_workflow_present
            .semantic_revision_ref,
        fixture
            .affiliations
            .child_workflow_removed
            .semantic_revision_ref
    );

    let native = &fixture.usage.native_message.revision;
    assert_eq!(
        native.response_identity,
        UsageResponseIdentity::NativeMessageId
    );
    assert_eq!(native.native_message_id.as_deref(), Some(NATIVE_MESSAGE_ID));
    assert_eq!(native.response_key, NATIVE_MESSAGE_ID.as_bytes());
    assert_eq!(native.buckets.input_tokens.value, Some(0));
    assert_eq!(native.buckets.output_tokens.value, Some(42));
    assert_eq!(native.buckets.cache_read_input_tokens.value, None);
    assert_eq!(
        native.buckets.cache_read_input_tokens.unknown_reason,
        Some(QualifiedUnknownReason::Missing)
    );
    assert_eq!(
        native
            .model
            .as_ref()
            .and_then(|value| value.value.as_deref()),
        Some("fixture-model-1")
    );
    assert_eq!(
        native.effort.as_ref().map(|value| value.authority),
        Some(UsageValueAuthority::AdapterDerived)
    );

    let fallback = &fixture.usage.source_record_fallback.revision;
    assert_eq!(
        fallback.response_identity,
        UsageResponseIdentity::SourceRecordFallback
    );
    assert_eq!(fallback.native_message_id, None);
    assert_eq!(fallback.response_key, FALLBACK_RESPONSE_KEY);

    let aba = &fixture.usage.response_revisions;
    assert_eq!(aba.a.revision.response_key, ABA_MESSAGE_ID.as_bytes());
    assert_eq!(aba.a.fact_id, aba.b.fact_id);
    assert_eq!(aba.a.fact_id, aba.a_repeat.fact_id);
    assert_eq!(
        aba.a.semantic_revision_ref,
        aba.a_repeat.semantic_revision_ref
    );
    assert_eq!(
        aba.a.semantic_revision_key_hex,
        aba.a_repeat.semantic_revision_key_hex
    );
    assert_ne!(aba.a.semantic_revision_ref, aba.b.semantic_revision_ref);
    assert_ne!(
        aba.a.revision.buckets.input_tokens,
        aba.b.revision.buckets.input_tokens
    );
}

#[test]
fn usage_semantic_revision_keys_are_recomputed_from_constructors() {
    let fixture = expected_fixture();
    for example in [
        &fixture.usage.native_message,
        &fixture.usage.source_record_fallback,
        &fixture.usage.response_revisions.a,
        &fixture.usage.response_revisions.b,
        &fixture.usage.response_revisions.a_repeat,
    ] {
        assert_usage_revision_identity(example);
    }
}

#[test]
fn usage_aba_sequence_is_idempotent_at_the_fact_batch_boundary() {
    let fixture = expected_fixture();
    let context = fixture_context();
    let record = fixture_record();
    let mut batch = fixture_batch(context);
    let a = fixture.usage.response_revisions.a.revision.clone();
    let b = fixture.usage.response_revisions.b.revision.clone();
    let a_key = a.semantic_revision_key().unwrap();
    let b_key = b.semantic_revision_key().unwrap();
    let a_response_key = a.response_key.clone();
    let b_response_key = b.response_key.clone();
    let first = batch
        .push_native_object_scoped_with_revision(
            &record,
            &a_response_key,
            &a_key,
            Fact::UsageRevisionV2(a.clone()),
        )
        .unwrap();
    batch
        .push_native_object_scoped_with_revision(
            &record,
            &b_response_key,
            &b_key,
            Fact::UsageRevisionV2(b),
        )
        .unwrap();
    let repeated = batch
        .push_native_object_scoped_with_revision(
            &record,
            &a_response_key,
            &a_key,
            Fact::UsageRevisionV2(a),
        )
        .unwrap();
    assert_eq!(first, repeated);
    assert_eq!(batch.facts().len(), 2);
    assert_eq!(
        batch.facts()[0]
            .semantic_revision
            .unwrap()
            .semantic_revision_ref,
        fixture.usage.response_revisions.a.semantic_revision_ref
    );
    assert_eq!(
        batch.facts()[1]
            .semantic_revision
            .unwrap()
            .semantic_revision_ref,
        fixture.usage.response_revisions.b.semantic_revision_ref
    );
}

#[test]
fn invalid_runtime_payloads_fail_the_existing_fact_contract() {
    let fixture = expected_fixture();
    let mut root = fixture.actors.root.revision.clone();
    root.parent_actor_run = Some(root.actor_run);
    assert!(root.validate().is_err());

    let mut child = fixture.actors.child.revision.clone();
    child.parent_actor_run = None;
    assert!(child.validate().is_err());

    let mut usage = fixture.usage.native_message.revision.clone();
    usage.response_key = b"not-the-native-id".to_vec();
    assert!(usage.semantic_revision_key().is_err());

    usage = fixture.usage.source_record_fallback.revision.clone();
    usage.native_message_id = Some(NATIVE_MESSAGE_ID.to_string());
    assert!(usage.semantic_revision_key().is_err());

    usage = fixture.usage.native_message.revision.clone();
    usage.source_time = Some(QualifiedTimestamp {
        value: String::new(),
        quality: TimestampQuality::NativeExact,
    });
    assert!(usage.semantic_revision_key().is_err());
}

//! RFC 011 Codex adapter.
//!
//! The common append driver owns rollout framing, checkpoints, generations,
//! retry, and live scheduling. This module owns only Codex path declarations
//! and the meaning of native rollout records.

use std::path::{Component, Path};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::adapter::{
    AdapterDiagnostic, AdapterError, AdapterErrorClass, AdapterId, AdapterManifest,
    AdapterObjectContext, AdapterSupportBinding, AgentAdapter, Availability, CapabilityDeclaration,
    CapabilityGranularity, CapabilityId, CapabilitySupport, ConsistencyPolicy, ContentBlock,
    DecodeContext, DecodeDisposition, DecoderId, DeletionPolicy, DiscoveryContext, DriverSpec,
    EntityKey, EntityScope, EvidenceKind, EvidenceStrength, Fact, FactBatch, MessageFact,
    MessageRole, ObjectSelector, QualifiedTimestamp, RawRetentionPolicy, RunEvidenceFact, RunFact,
    SessionFact, SourceInstance, SourceInstanceKey, SourceInstanceSpec, SourceObjectDescriptor,
    SourceRoot, StreamAuthority, StreamId, StreamSpec, SupportLevel, TimestampQuality, TokenUsage,
    UsageAccounting, UsageFact, UsageScope, ValueQuality,
};
use crate::source::{
    platform_path_key, AppendDelimitedConfig, IngestPriority, SourceRecord, SourceRecordState,
};

const ADAPTER_ID: &str = "codex";
const ROLLOUT_STREAM: &str = "rollout-sessions";
const ROLLOUT_DECODER: &str = "codex-rollout-record";
const OBJECT_CONTEXT_VERSION: u32 = 1;
const DECODER_STATE_VERSION: u32 = 1;
const SEARCH_TEXT_MAX_UTF16: usize = 2_000;

const HISTORY_SESSIONS: &str = "history.sessions";
const HISTORY_MESSAGES: &str = "history.messages";
const HISTORY_CONTENT_BLOCKS: &str = "history.content_blocks";
const HISTORY_TIMESTAMPS: &str = "history.timestamps";
const HISTORY_MODEL_IDENTITY: &str = "history.model_identity";
const RUNTIME_SESSION_ACTIVITY: &str = "runtime.session_activity";
const USAGE_INPUT_TOKENS: &str = "usage.input_tokens";
const USAGE_OUTPUT_TOKENS: &str = "usage.output_tokens";
const USAGE_CACHE_TOKENS: &str = "usage.cache_tokens";
const SOURCE_LIVE: &str = "source.live";
const SOURCE_RECONCILE: &str = "source.reconcile";
const SOURCE_RESUME_CURSOR: &str = "source.resume_cursor";

pub struct CodexAdapter {
    manifest: AdapterManifest,
}

impl CodexAdapter {
    pub fn new() -> Self {
        Self {
            manifest: AdapterManifest {
                id: AdapterId::new(ADAPTER_ID).expect("static Codex adapter id is valid"),
                display_name: "Codex".to_string(),
                adapter_version: env!("CARGO_PKG_VERSION").to_string(),
                contract_version: 1,
                support_binding: Some(
                    AdapterSupportBinding::new(
                        env!("CARGO_PKG_VERSION"),
                        1,
                        "sha256:0256d195021bb939f4af366b631eaf04c9121a880380852fde9913331673961e",
                        "sha256:c4d6d49516dc525fb7b3d514924c1024b6998a1831c44c9c6b7936f96163c25b",
                        "sha256:7990862cac4b59164dd8d25218077cce33e12fc15b66086e2763fcf6057a9fa5",
                    )
                    .expect("static Codex support binding is valid"),
                ),
                source_schema_versions: vec!["codex-rollout-jsonl-v1".to_string()],
                capabilities: codex_capabilities(),
            },
        }
    }

    fn adapter_id(&self) -> &AdapterId {
        &self.manifest.id
    }
}

impl Default for CodexAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl AgentAdapter for CodexAdapter {
    fn manifest(&self) -> &AdapterManifest {
        &self.manifest
    }

    fn discover(
        &self,
        context: &DiscoveryContext,
    ) -> Result<Vec<SourceInstanceSpec>, AdapterError> {
        context
            .configured_roots
            .iter()
            .map(|configured_root| {
                let canonical = std::fs::canonicalize(configured_root).map_err(|error| {
                    AdapterError::new(
                        AdapterErrorClass::Transient,
                        "codex_root_unavailable",
                        format!("{}: {error}", configured_root.to_string_lossy()),
                    )
                })?;
                if !canonical.is_dir() {
                    return Err(AdapterError::new(
                        AdapterErrorClass::AdapterFatal,
                        "codex_root_not_directory",
                        canonical.to_string_lossy(),
                    ));
                }
                Ok(SourceInstanceSpec {
                    stable_key: SourceInstanceKey::new(platform_path_key(&canonical))?,
                    display_name: format!("Codex ({})", canonical.to_string_lossy()),
                    roots: vec![
                        SourceRoot {
                            name: "home".to_string(),
                            path: canonical.clone(),
                        },
                        SourceRoot {
                            name: "sessions".to_string(),
                            path: canonical.join("sessions"),
                        },
                    ],
                    discovery_reason: "configured Codex data root".to_string(),
                })
            })
            .collect()
    }

    fn streams(&self, instance: &SourceInstance) -> Result<Vec<StreamSpec>, AdapterError> {
        let stream = StreamSpec {
            id: StreamId::new(ROLLOUT_STREAM)?,
            driver: DriverSpec::AppendDelimited(AppendDelimitedConfig::json_lines()),
            selector: ObjectSelector {
                root_name: "sessions".to_string(),
                include: vec!["**/rollout-*.jsonl".to_string()],
                exclude: Vec::new(),
            },
            decoder: DecoderId::new(ROLLOUT_DECODER)?,
            authority: StreamAuthority::Canonical,
            entity_scope: EntityScope::Session,
            priority: IngestPriority::Interactive,
            consistency: ConsistencyPolicy::IncrementalCursor,
            deletion: DeletionPolicy::MirrorSource,
            retention: RawRetentionPolicy::HashOnly,
            capabilities: rollout_capabilities(),
        };
        stream.validate(instance)?;
        Ok(vec![stream])
    }

    fn bootstrap_object(
        &self,
        _instance: &SourceInstance,
        object: &SourceObjectDescriptor,
    ) -> Result<AdapterObjectContext, AdapterError> {
        if object.stream_id.as_str() != ROLLOUT_STREAM {
            return Err(AdapterError::new(
                AdapterErrorClass::StreamFatal,
                "codex_unknown_stream",
                format!("unknown Codex stream {}", object.stream_id),
            ));
        }
        validate_rollout_path(&object.relative_path)?;
        let context = CodexObjectContext {
            relative_path: object.relative_path.to_string_lossy().into_owned(),
        };
        let payload = serde_json::to_vec(&context).map_err(|error| {
            AdapterError::new(
                AdapterErrorClass::InvalidContract,
                "codex_object_context_encode",
                error.to_string(),
            )
        })?;
        AdapterObjectContext::new(OBJECT_CONTEXT_VERSION, payload)
    }

    fn decode(
        &self,
        context: DecodeContext<'_>,
        record: &SourceRecord,
        output: &mut FactBatch,
    ) -> Result<DecodeDisposition, AdapterError> {
        if context.decoder.as_str() != ROLLOUT_DECODER {
            return Err(AdapterError::unknown_decoder(context.decoder));
        }
        let object_context = CodexObjectContext::decode(context.object_context)?;
        decode_rollout_record(
            self.adapter_id(),
            &object_context,
            context.decoder_state,
            record,
            output,
        )
    }
}

fn codex_capabilities() -> Vec<CapabilityDeclaration> {
    let native = |id: &'static str,
                  granularity: CapabilityGranularity,
                  notes: Option<&'static str>| CapabilityDeclaration {
        id: CapabilityId::new(id).expect("static Codex capability id is valid"),
        support: CapabilitySupport {
            level: SupportLevel::Native,
            granularity,
            availability: Availability::Live,
            notes: notes.map(str::to_owned),
        },
    };
    vec![
        native(HISTORY_SESSIONS, CapabilityGranularity::Session, None),
        native(HISTORY_MESSAGES, CapabilityGranularity::Message, None),
        native(HISTORY_CONTENT_BLOCKS, CapabilityGranularity::Message, None),
        native(HISTORY_TIMESTAMPS, CapabilityGranularity::Message, None),
        native(
            HISTORY_MODEL_IDENTITY,
            CapabilityGranularity::Message,
            Some("Codex turn_context model identity applies to following rollout records"),
        ),
        native(
            RUNTIME_SESSION_ACTIVITY,
            CapabilityGranularity::Run,
            Some("native record activity is durable; silence does not imply completion"),
        ),
        native(
            USAGE_INPUT_TOKENS,
            CapabilityGranularity::Turn,
            Some("last_token_usage is a replaceable native turn snapshot; total-only records remain session-scoped cumulative counters"),
        ),
        native(USAGE_OUTPUT_TOKENS, CapabilityGranularity::Turn, None),
        native(USAGE_CACHE_TOKENS, CapabilityGranularity::Turn, None),
        native(SOURCE_LIVE, CapabilityGranularity::Instance, None),
        native(SOURCE_RECONCILE, CapabilityGranularity::Instance, None),
        native(SOURCE_RESUME_CURSOR, CapabilityGranularity::Record, None),
    ]
}

fn rollout_capabilities() -> Vec<CapabilityId> {
    [
        HISTORY_SESSIONS,
        HISTORY_MESSAGES,
        HISTORY_CONTENT_BLOCKS,
        HISTORY_TIMESTAMPS,
        HISTORY_MODEL_IDENTITY,
        RUNTIME_SESSION_ACTIVITY,
        USAGE_INPUT_TOKENS,
        USAGE_OUTPUT_TOKENS,
        USAGE_CACHE_TOKENS,
        SOURCE_LIVE,
        SOURCE_RECONCILE,
        SOURCE_RESUME_CURSOR,
    ]
    .into_iter()
    .map(|id| CapabilityId::new(id).expect("static Codex stream capability id is valid"))
    .collect()
}

#[derive(Debug, Serialize, Deserialize)]
struct CodexObjectContext {
    relative_path: String,
}

impl CodexObjectContext {
    fn decode(context: &AdapterObjectContext) -> Result<Self, AdapterError> {
        if context.version() != OBJECT_CONTEXT_VERSION {
            return Err(AdapterError::new(
                AdapterErrorClass::StreamFatal,
                "codex_object_context_version",
                format!(
                    "unsupported Codex object context version {}",
                    context.version()
                ),
            ));
        }
        serde_json::from_slice(context.payload()).map_err(|error| {
            AdapterError::new(
                AdapterErrorClass::StreamFatal,
                "codex_object_context_decode",
                error.to_string(),
            )
        })
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct CodexDecoderState {
    version: u32,
    initialized: bool,
    internal: bool,
    session_id: String,
    cwd: String,
    native_project_key: String,
    session_time: Option<String>,
    first_prompt: Option<String>,
    model: Option<String>,
    last_assistant: Option<EntityKey>,
}

impl CodexDecoderState {
    fn decode(value: Option<&[u8]>) -> Result<Self, AdapterError> {
        let Some(value) = value else {
            return Ok(Self {
                version: DECODER_STATE_VERSION,
                ..Self::default()
            });
        };
        let state: Self = serde_json::from_slice(value).map_err(|error| {
            AdapterError::new(
                AdapterErrorClass::StreamFatal,
                "codex_decoder_state_decode",
                error.to_string(),
            )
        })?;
        if state.version != DECODER_STATE_VERSION {
            return Err(AdapterError::new(
                AdapterErrorClass::StreamFatal,
                "codex_decoder_state_version",
                format!("unsupported Codex decoder state version {}", state.version),
            ));
        }
        Ok(state)
    }

    fn store(&self, output: &mut FactBatch) -> Result<(), AdapterError> {
        let encoded = serde_json::to_vec(self).map_err(|error| {
            AdapterError::new(
                AdapterErrorClass::InvalidContract,
                "codex_decoder_state_encode",
                error.to_string(),
            )
        })?;
        output.set_next_decoder_state(encoded)
    }
}

fn validate_rollout_path(path: &Path) -> Result<(), AdapterError> {
    if path.as_os_str().is_empty()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(AdapterError::new(
            AdapterErrorClass::StreamFatal,
            "codex_invalid_rollout_path",
            path.to_string_lossy(),
        ));
    }
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("");
    if !name.starts_with("rollout-") || !name.ends_with(".jsonl") {
        return Err(AdapterError::new(
            AdapterErrorClass::StreamFatal,
            "codex_invalid_rollout_name",
            name,
        ));
    }
    Ok(())
}

fn decode_rollout_record(
    adapter_id: &AdapterId,
    object_context: &CodexObjectContext,
    decoder_state: Option<&[u8]>,
    record: &SourceRecord,
    output: &mut FactBatch,
) -> Result<DecodeDisposition, AdapterError> {
    if record.state == SourceRecordState::Absent {
        return Ok(DecodeDisposition::IgnoredKnown);
    }
    let mut state = CodexDecoderState::decode(decoder_state)?;
    let raw: Value = match serde_json::from_slice(&record.payload) {
        Ok(value) => value,
        Err(error) => {
            preserve_unknown(
                record,
                output,
                None,
                format!("malformed Codex JSON record: {error}"),
            )?;
            state.store(output)?;
            return Ok(DecodeDisposition::PreservedUnknown);
        }
    };
    let Some(root) = raw.as_object() else {
        preserve_unknown(
            record,
            output,
            None,
            "Codex rollout record is not an object".to_string(),
        )?;
        state.store(output)?;
        return Ok(DecodeDisposition::PreservedUnknown);
    };
    let native_kind = root
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("unknown");

    if native_kind == "session_meta" {
        return decode_session_meta(adapter_id, root, record, &mut state, output);
    }

    if !state.initialized {
        preserve_unknown(
            record,
            output,
            Some(native_kind.to_string()),
            format!(
                "Codex record preceded valid session_meta in {}",
                object_context.relative_path
            ),
        )?;
        state.store(output)?;
        return Ok(DecodeDisposition::PreservedUnknown);
    }
    if state.internal {
        state.store(output)?;
        return Ok(DecodeDisposition::IgnoredKnown);
    }

    let source_time = qualified_timestamp(root.get("timestamp"));
    match native_kind {
        "turn_context" => {
            if let Some(model) = root
                .get("payload")
                .and_then(Value::as_object)
                .and_then(|payload| nonempty_string(payload.get("model")))
            {
                state.model = Some(model);
            }
            state.store(output)?;
            Ok(DecodeDisposition::IgnoredKnown)
        }
        "response_item" => decode_response_item(
            adapter_id,
            root,
            &raw,
            source_time,
            record,
            &mut state,
            output,
        ),
        "event_msg" => {
            decode_event_message(adapter_id, root, source_time, record, &mut state, output)
        }
        _ => {
            preserve_unknown(
                record,
                output,
                Some(native_kind.to_string()),
                format!("unknown Codex rollout record kind {native_kind}"),
            )?;
            state.store(output)?;
            Ok(DecodeDisposition::PreservedUnknown)
        }
    }
}

fn decode_session_meta(
    adapter_id: &AdapterId,
    root: &Map<String, Value>,
    record: &SourceRecord,
    state: &mut CodexDecoderState,
    output: &mut FactBatch,
) -> Result<DecodeDisposition, AdapterError> {
    let Some(payload) = root.get("payload").and_then(Value::as_object) else {
        preserve_unknown(
            record,
            output,
            Some("session_meta".to_string()),
            "Codex session_meta payload is not an object".to_string(),
        )?;
        state.store(output)?;
        return Ok(DecodeDisposition::PreservedUnknown);
    };
    let session_id = nonempty_string(payload.get("id"));
    let cwd = nonempty_string(payload.get("cwd"));
    let (Some(session_id), Some(cwd)) = (session_id, cwd) else {
        preserve_unknown(
            record,
            output,
            Some("session_meta".to_string()),
            "Codex session_meta requires non-empty id and cwd".to_string(),
        )?;
        state.store(output)?;
        return Ok(DecodeDisposition::PreservedUnknown);
    };
    state.version = DECODER_STATE_VERSION;
    state.initialized = true;
    state.internal = is_internal_session(payload);
    state.session_id = session_id;
    state.cwd = cwd;
    state.native_project_key = encode_project_key(&state.cwd);
    state.session_time = root
        .get("timestamp")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or_else(|| nonempty_string(payload.get("timestamp")));
    state.model = nonempty_string(payload.get("model"));
    state.last_assistant = None;
    state.store(output)?;
    if state.internal {
        return Ok(DecodeDisposition::IgnoredKnown);
    }

    let (session, project, run) = entity_keys(adapter_id, record.source_instance_id, state)?;
    output.push(
        record,
        Fact::Session(session_fact(state, session.clone(), project)),
    )?;
    output.push(
        record,
        Fact::Run(RunFact {
            run: run.clone(),
            session,
            native_run_id: state.session_id.clone(),
            parent_run: None,
        }),
    )?;
    output.push(
        record,
        Fact::RunEvidence(RunEvidenceFact {
            run: run.clone(),
            kind: EvidenceKind::RunDeclared,
            strength: EvidenceStrength::NativeExplicit,
            native_state: Some("session_meta".to_string()),
            source_time: state.session_time.as_deref().map(native_timestamp),
        }),
    )?;
    output.push(
        record,
        Fact::RunEvidence(RunEvidenceFact {
            run,
            kind: EvidenceKind::RunStarted,
            strength: EvidenceStrength::NativeActivity,
            native_state: Some("session_meta".to_string()),
            source_time: state.session_time.as_deref().map(native_timestamp),
        }),
    )?;
    Ok(DecodeDisposition::Applied)
}

#[allow(clippy::too_many_arguments)]
fn decode_response_item(
    adapter_id: &AdapterId,
    root: &Map<String, Value>,
    raw: &Value,
    source_time: Option<QualifiedTimestamp>,
    record: &SourceRecord,
    state: &mut CodexDecoderState,
    output: &mut FactBatch,
) -> Result<DecodeDisposition, AdapterError> {
    let Some(payload) = root.get("payload").and_then(Value::as_object) else {
        preserve_unknown(
            record,
            output,
            Some("response_item".to_string()),
            "Codex response_item payload is not an object".to_string(),
        )?;
        state.store(output)?;
        return Ok(DecodeDisposition::PreservedUnknown);
    };
    let payload_kind = payload
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let Some((role, content, search_text)) = decode_content(payload_kind, payload) else {
        preserve_unknown(
            record,
            output,
            Some(format!("response_item/{payload_kind}")),
            format!("unknown Codex response_item kind {payload_kind}"),
        )?;
        state.store(output)?;
        return Ok(DecodeDisposition::PreservedUnknown);
    };
    let (session, project, run) = entity_keys(adapter_id, record.source_instance_id, state)?;
    let native_message_id =
        nonempty_string(payload.get("id")).or_else(|| nonempty_string(payload.get("call_id")));
    let native_key = message_native_key(
        &state.session_id,
        payload_kind,
        native_message_id.as_deref(),
        record,
    );
    let message = EntityKey::native(
        adapter_id,
        record.source_instance_id,
        "message",
        &native_key,
    )?;

    if role == MessageRole::User && state.first_prompt.is_none() {
        if let Some(text) = search_text
            .as_deref()
            .filter(|text| !is_injected_user_text(text))
        {
            state.first_prompt = Some(
                crate::core::text::truncate_utf16(text, 200)
                    .trim()
                    .to_string(),
            );
            output.push(
                record,
                Fact::Session(session_fact(state, session.clone(), project)),
            )?;
        }
    }
    output.push(
        record,
        Fact::Message(MessageFact {
            message: message.clone(),
            session: session.clone(),
            run: run.clone(),
            native_message_id,
            native_kind: payload_kind.to_string(),
            role: role.clone(),
            content,
            source_time: source_time.clone(),
            parent_native_message_id: nonempty_string(payload.get("parent_id")),
            model: state.model.clone(),
            search_text,
            raw_json: serde_json::to_vec(raw).map_err(|error| {
                AdapterError::new(
                    AdapterErrorClass::RecordPermanent,
                    "codex_raw_json_encode",
                    error.to_string(),
                )
            })?,
        }),
    )?;
    output.push(
        record,
        Fact::RunEvidence(RunEvidenceFact {
            run,
            kind: EvidenceKind::ActivityObserved,
            strength: EvidenceStrength::NativeActivity,
            native_state: Some(format!("response_item/{payload_kind}")),
            source_time,
        }),
    )?;
    if role == MessageRole::Assistant && payload_kind == "message" {
        state.last_assistant = Some(message);
    }
    state.store(output)?;
    Ok(DecodeDisposition::Applied)
}

fn decode_event_message(
    adapter_id: &AdapterId,
    root: &Map<String, Value>,
    source_time: Option<QualifiedTimestamp>,
    record: &SourceRecord,
    state: &mut CodexDecoderState,
    output: &mut FactBatch,
) -> Result<DecodeDisposition, AdapterError> {
    let Some(payload) = root.get("payload").and_then(Value::as_object) else {
        preserve_unknown(
            record,
            output,
            Some("event_msg".to_string()),
            "Codex event_msg payload is not an object".to_string(),
        )?;
        state.store(output)?;
        return Ok(DecodeDisposition::PreservedUnknown);
    };
    let event_kind = payload
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    if event_kind == "token_count" {
        return decode_token_count(adapter_id, payload, source_time, record, state, output);
    }

    if event_kind == "user_message" && state.first_prompt.is_none() {
        if let Some(prompt) =
            nonempty_string(payload.get("message")).filter(|value| !is_injected_user_text(value))
        {
            state.first_prompt = Some(
                crate::core::text::truncate_utf16(&prompt, 200)
                    .trim()
                    .to_string(),
            );
            let (session, project, _) = entity_keys(adapter_id, record.source_instance_id, state)?;
            output.push(record, Fact::Session(session_fact(state, session, project)))?;
            state.store(output)?;
            return Ok(DecodeDisposition::Applied);
        }
    }

    // These are UI/lifecycle projections or telemetry whose canonical source
    // is another response_item. Preserve genuinely unknown event shapes, but
    // do not duplicate ordinary user/agent messages in history.
    if matches!(
        event_kind,
        "user_message"
            | "agent_message"
            | "agent_reasoning"
            | "turn_aborted"
            | "task_started"
            | "task_complete"
            | "context_compacted"
            | "rate_limit"
    ) {
        state.store(output)?;
        return Ok(DecodeDisposition::IgnoredKnown);
    }

    preserve_unknown(
        record,
        output,
        Some(format!("event_msg/{event_kind}")),
        format!("unknown Codex event_msg kind {event_kind}"),
    )?;
    state.store(output)?;
    Ok(DecodeDisposition::PreservedUnknown)
}

fn decode_token_count(
    adapter_id: &AdapterId,
    payload: &Map<String, Value>,
    source_time: Option<QualifiedTimestamp>,
    record: &SourceRecord,
    state: &mut CodexDecoderState,
    output: &mut FactBatch,
) -> Result<DecodeDisposition, AdapterError> {
    let Some(info) = payload.get("info").and_then(Value::as_object) else {
        state.store(output)?;
        return Ok(DecodeDisposition::IgnoredKnown);
    };
    let last = info.get("last_token_usage").and_then(parse_usage);
    let total = info.get("total_token_usage").and_then(parse_usage);
    let (session, _, run) = entity_keys(adapter_id, record.source_instance_id, state)?;
    let usage = if let (Some(values), Some(subject)) = (last, state.last_assistant.clone()) {
        Some(UsageFact {
            subject,
            session: session.clone(),
            scope: UsageScope::Turn,
            // Native "last" values are a current snapshot of the preceding
            // turn. Multiple token_count records for one turn replace rather
            // than sum, matching the source's correction semantics.
            accounting: UsageAccounting::Snapshot,
            quality: ValueQuality::NativeExact,
            values,
            model: state.model.clone(),
            source_time: source_time.clone(),
        })
    } else {
        total.map(|values| UsageFact {
            subject: session.clone(),
            session: session.clone(),
            scope: UsageScope::Session,
            accounting: UsageAccounting::Cumulative,
            quality: ValueQuality::NativeExact,
            values,
            model: state.model.clone(),
            source_time: source_time.clone(),
        })
    };
    let Some(usage) = usage else {
        state.store(output)?;
        return Ok(DecodeDisposition::IgnoredKnown);
    };
    output.push(record, Fact::Usage(usage))?;
    output.push(
        record,
        Fact::RunEvidence(RunEvidenceFact {
            run,
            kind: EvidenceKind::ActivityObserved,
            strength: EvidenceStrength::NativeActivity,
            native_state: Some("event_msg/token_count".to_string()),
            source_time,
        }),
    )?;
    state.store(output)?;
    Ok(DecodeDisposition::Applied)
}

fn session_fact(state: &CodexDecoderState, session: EntityKey, project: EntityKey) -> SessionFact {
    SessionFact {
        session,
        project,
        native_session_id: state.session_id.clone(),
        native_project_key: state.native_project_key.clone(),
        cwd: Some(state.cwd.clone()),
        git_branch: None,
        first_prompt: state.first_prompt.clone(),
        ai_title: None,
        custom_title: None,
        source_time: state.session_time.as_deref().map(native_timestamp),
    }
}

fn entity_keys(
    adapter_id: &AdapterId,
    source_instance_id: u64,
    state: &CodexDecoderState,
) -> Result<(EntityKey, EntityKey, EntityKey), AdapterError> {
    let session = EntityKey::native(
        adapter_id,
        source_instance_id,
        "session",
        state.session_id.as_bytes(),
    )?;
    let project = EntityKey::native(
        adapter_id,
        source_instance_id,
        "project",
        state.native_project_key.as_bytes(),
    )?;
    let run = EntityKey::native(
        adapter_id,
        source_instance_id,
        "run",
        state.session_id.as_bytes(),
    )?;
    Ok((session, project, run))
}

fn decode_content(
    kind: &str,
    payload: &Map<String, Value>,
) -> Option<(MessageRole, Vec<ContentBlock>, Option<String>)> {
    if kind == "message" {
        let role = match payload.get("role").and_then(Value::as_str) {
            Some("user") => MessageRole::User,
            Some("assistant") => MessageRole::Assistant,
            Some("system") => MessageRole::System,
            Some(value) => MessageRole::Other(value.to_string()),
            None => MessageRole::Other("unknown".to_string()),
        };
        let blocks = message_blocks(payload.get("content"));
        let search_text = readable_search_text(&blocks);
        return Some((role, blocks, search_text));
    }
    if kind == "reasoning" {
        let text = readable_value(payload.get("summary"));
        let blocks = vec![ContentBlock::Thinking {
            text: text.clone(),
            redacted: text.is_empty() && payload.contains_key("encrypted_content"),
        }];
        return Some((MessageRole::Assistant, blocks, truncate_search_text(text)));
    }
    if is_tool_result(kind) {
        let native_call_id = nonempty_string(payload.get("call_id"))
            .or_else(|| nonempty_string(payload.get("id")))
            .unwrap_or_else(|| "unknown".to_string());
        let value = payload
            .get("output")
            .or_else(|| payload.get("tools"))
            .cloned()
            .unwrap_or(Value::Null);
        let is_error = payload.get("is_error").and_then(Value::as_bool) == Some(true)
            || payload.get("success").and_then(Value::as_bool) == Some(false)
            || payload
                .get("status")
                .and_then(Value::as_str)
                .is_some_and(|status| {
                    matches!(status.to_ascii_lowercase().as_str(), "error" | "failed")
                });
        return Some((
            MessageRole::Other("tool_result".to_string()),
            vec![ContentBlock::ToolResult {
                native_call_id,
                content: value,
                is_error,
            }],
            None,
        ));
    }
    if is_tool_call(kind) {
        let native_id = nonempty_string(payload.get("call_id"))
            .or_else(|| nonempty_string(payload.get("id")))
            .unwrap_or_else(|| "unknown".to_string());
        let name = nonempty_string(payload.get("name"))
            .or_else(|| kind.strip_suffix("_call").map(str::to_owned))
            .unwrap_or_else(|| "Unknown Tool".to_string());
        let input = tool_input(payload);
        return Some((
            MessageRole::Assistant,
            vec![ContentBlock::ToolCall {
                native_id,
                name: name.clone(),
                input,
            }],
            truncate_search_text(name),
        ));
    }
    None
}

fn message_blocks(value: Option<&Value>) -> Vec<ContentBlock> {
    let Some(value) = value else {
        return Vec::new();
    };
    if let Some(text) = value.as_str() {
        return vec![ContentBlock::Text {
            text: text.to_string(),
        }];
    }
    let Some(values) = value.as_array() else {
        return vec![ContentBlock::Native {
            native_kind: "content".to_string(),
            value: value.clone(),
        }];
    };
    values
        .iter()
        .filter_map(|value| {
            let object = value.as_object()?;
            let kind = object
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            if matches!(kind, "input_text" | "output_text" | "text" | "summary_text") {
                return Some(ContentBlock::Text {
                    text: object
                        .get("text")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                });
            }
            if kind == "input_image" {
                let source = object
                    .get("image_url")
                    .or_else(|| object.get("url"))
                    .or_else(|| object.get("data"))
                    .map(|value| value.to_string())
                    .unwrap_or_default();
                return Some(ContentBlock::Image {
                    media_type: object
                        .get("media_type")
                        .and_then(Value::as_str)
                        .unwrap_or("application/octet-stream")
                        .to_string(),
                    data_hash: *blake3::hash(source.as_bytes()).as_bytes(),
                });
            }
            Some(ContentBlock::Native {
                native_kind: kind.to_string(),
                value: value.clone(),
            })
        })
        .collect()
}

fn readable_search_text(blocks: &[ContentBlock]) -> Option<String> {
    let text = blocks
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text } => Some(text.as_str()),
            ContentBlock::Thinking { text, .. } => Some(text.as_str()),
            ContentBlock::ToolCall { name, .. } => Some(name.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    truncate_search_text(text)
}

fn truncate_search_text(text: String) -> Option<String> {
    (!text.is_empty())
        .then(|| crate::core::text::truncate_utf16(&text, SEARCH_TEXT_MAX_UTF16).to_string())
}

fn readable_value(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(value)) => value.clone(),
        Some(Value::Array(values)) => {
            let parts = values
                .iter()
                .filter_map(|value| value.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>();
            if parts.is_empty() {
                serde_json::to_string(values).unwrap_or_default()
            } else {
                parts.join("")
            }
        }
        Some(value) if !value.is_null() => serde_json::to_string(value).unwrap_or_default(),
        _ => String::new(),
    }
}

fn tool_input(payload: &Map<String, Value>) -> Value {
    let value = payload
        .get("arguments")
        .or_else(|| payload.get("input"))
        .or_else(|| payload.get("action"))
        .cloned()
        .unwrap_or_else(|| Value::Object(Map::new()));
    if let Some(text) = value.as_str() {
        serde_json::from_str(text).unwrap_or_else(|_| {
            Value::Object(Map::from_iter([(
                "input".to_string(),
                Value::String(text.to_string()),
            )]))
        })
    } else if value.is_object() {
        value
    } else if let Value::Array(values) = value {
        Value::Object(Map::from_iter([(
            "items".to_string(),
            Value::Array(values),
        )]))
    } else {
        Value::Object(Map::from_iter([("input".to_string(), value)]))
    }
}

fn parse_usage(value: &Value) -> Option<TokenUsage> {
    let object = value.as_object()?;
    let number = |key: &str| object.get(key).and_then(Value::as_u64).unwrap_or(0);
    Some(TokenUsage {
        input_tokens: number("input_tokens"),
        output_tokens: number("output_tokens").saturating_add(number("reasoning_output_tokens")),
        cache_creation_tokens: 0,
        cache_read_tokens: number("cached_input_tokens"),
    })
}

fn is_tool_result(kind: &str) -> bool {
    kind.ends_with("_call_output") || kind == "tool_search_output"
}

fn is_tool_call(kind: &str) -> bool {
    kind.ends_with("_call") && !is_tool_result(kind)
}

fn is_internal_session(payload: &Map<String, Value>) -> bool {
    if payload.get("thread_source").and_then(Value::as_str) == Some("subagent") {
        return true;
    }
    if payload
        .get("source")
        .and_then(Value::as_object)
        .is_some_and(|source| source.contains_key("subagent"))
    {
        return true;
    }
    let id = payload.get("id").and_then(Value::as_str).unwrap_or("");
    let logical = payload
        .get("session_id")
        .and_then(Value::as_str)
        .unwrap_or("");
    let parent = payload
        .get("parent_thread_id")
        .and_then(Value::as_str)
        .unwrap_or("");
    !parent.is_empty() && !id.is_empty() && !logical.is_empty() && id != logical
}

fn is_injected_user_text(text: &str) -> bool {
    let text = text.trim_start();
    text.is_empty()
        || text.starts_with("<environment_context>")
        || text.starts_with("<recommended_plugins>")
        || text.starts_with("<permissions instructions>")
        || text.starts_with("<collaboration_mode>")
        || text.starts_with("<skills_instructions>")
        || text.starts_with("<apps_instructions>")
        || text.starts_with("<plugins_instructions>")
        || text.starts_with("<multi_agent_mode>")
        || text.starts_with("<INSTRUCTIONS>")
        || text.starts_with("# AGENTS.md instructions")
        || text.starts_with(
            "The following is the Codex agent history whose request action you are assessing.",
        )
        || text.starts_with(
            "The following is the Codex agent history added since your last approval assessment.",
        )
        || (text.starts_with('<')
            && text.contains("<cwd>")
            && (text.contains("</cwd>") || text.contains("<shell>")))
}

fn encode_project_key(cwd: &str) -> String {
    cwd.replace(['/', '\\'], "-")
}

fn nonempty_string(value: Option<&Value>) -> Option<String> {
    value
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn qualified_timestamp(value: Option<&Value>) -> Option<QualifiedTimestamp> {
    nonempty_string(value).map(|value| native_timestamp(&value))
}

fn native_timestamp(value: &str) -> QualifiedTimestamp {
    QualifiedTimestamp {
        value: value.to_string(),
        quality: TimestampQuality::NativeExact,
    }
}

fn message_native_key(
    session_id: &str,
    native_kind: &str,
    native_message_id: Option<&str>,
    record: &SourceRecord,
) -> Vec<u8> {
    let mut key = Vec::new();
    push_key_component(&mut key, session_id.as_bytes());
    push_key_component(&mut key, native_kind.as_bytes());
    if let Some(native_message_id) = native_message_id {
        push_key_component(&mut key, native_message_id.as_bytes());
    } else {
        push_key_component(&mut key, record.cursor_start.as_bytes());
        push_key_component(&mut key, record.cursor_end.as_bytes());
    }
    key
}

fn push_key_component(output: &mut Vec<u8>, value: &[u8]) {
    output.extend_from_slice(&(value.len() as u64).to_be_bytes());
    output.extend_from_slice(value);
}

fn preserve_unknown(
    record: &SourceRecord,
    output: &mut FactBatch,
    native_kind: Option<String>,
    reason: String,
) -> Result<(), AdapterError> {
    output.push(
        record,
        Fact::UnknownRecord {
            native_kind,
            raw_payload: record.payload.clone(),
            reason: reason.clone(),
        },
    )?;
    output.push_diagnostic(AdapterDiagnostic {
        class: AdapterErrorClass::RecordPermanent,
        code: "codex_preserved_unknown".to_string(),
        message: reason,
    })
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use tempfile::TempDir;

    use crate::adapter::{AdapterRegistry, DiscoveryContext, SourceInstance};
    use crate::engine::{EngineOptions, ReconcileRequest, SpaghettiEngineCore};
    use crate::source::{AppendDelimitedFile, AppendItem, RecordOrigin, SourceMediaType};

    use super::*;

    fn fixture_adapter() -> (CodexAdapter, TempDir, SourceInstance, StreamSpec) {
        let root = TempDir::new().unwrap();
        fs::create_dir_all(root.path().join("sessions/2026/01/01")).unwrap();
        let adapter = CodexAdapter::new();
        let spec = adapter
            .discover(&DiscoveryContext {
                configured_roots: vec![root.path().to_path_buf()],
                observed_at: 1,
            })
            .unwrap()
            .remove(0);
        let instance = SourceInstance { id: 7, spec };
        let stream = adapter.streams(&instance).unwrap().remove(0);
        (adapter, root, instance, stream)
    }

    fn records(lines: &[&str]) -> Vec<SourceRecord> {
        let root = TempDir::new().unwrap();
        let path = root.path().join("rollout-test.jsonl");
        fs::write(&path, format!("{}\n", lines.join("\n"))).unwrap();
        let driver = AppendDelimitedFile::new(AppendDelimitedConfig::json_lines()).unwrap();
        let origin = RecordOrigin {
            source_instance_id: 7,
            stream_id: 8,
            object_id: 9,
            observed_at: 10,
            source_timestamp_hint: None,
            media_type: SourceMediaType::new("application/x-ndjson").unwrap(),
        };
        match driver.read(&path, None, &origin, false).unwrap() {
            crate::source::AppendRead::Batch { items, .. } => items
                .into_iter()
                .filter_map(|item| match item {
                    AppendItem::Record(record) => Some(record),
                    AppendItem::Quarantined(_) => None,
                })
                .collect(),
            other => panic!("unexpected append read {other:?}"),
        }
    }

    fn context() -> AdapterObjectContext {
        AdapterObjectContext::new(
            OBJECT_CONTEXT_VERSION,
            serde_json::to_vec(&CodexObjectContext {
                relative_path: "2026/01/01/rollout-test.jsonl".to_string(),
            })
            .unwrap(),
        )
        .unwrap()
    }

    fn decode_sequence(lines: &[&str]) -> Vec<Fact> {
        let adapter = CodexAdapter::new();
        let context = context();
        let decoder = DecoderId::new(ROLLOUT_DECODER).unwrap();
        let mut state = None;
        let mut facts = Vec::new();
        for record in records(lines) {
            let mut batch = FactBatch::new(16, 4).unwrap();
            adapter
                .decode(
                    DecodeContext {
                        decoder: &decoder,
                        object_context: &context,
                        decoder_state: state.as_deref(),
                    },
                    &record,
                    &mut batch,
                )
                .unwrap();
            state = batch.next_decoder_state().map(ToOwned::to_owned);
            facts.extend(batch.facts().iter().map(|fact| fact.value.clone()));
        }
        facts
    }

    #[test]
    fn declares_one_common_append_stream() {
        let (adapter, _root, instance, stream) = fixture_adapter();
        assert_eq!(adapter.manifest().id.as_str(), ADAPTER_ID);
        assert_eq!(stream.id.as_str(), ROLLOUT_STREAM);
        assert!(matches!(stream.driver, DriverSpec::AppendDelimited(_)));
        assert_eq!(stream.selector.include, ["**/rollout-*.jsonl"]);
        assert_eq!(
            instance.root("sessions").unwrap(),
            instance.root("home").unwrap().join("sessions")
        );
    }

    #[test]
    fn metadata_messages_tools_and_usage_share_durable_decoder_state() {
        let facts = decode_sequence(&[
            r#"{"timestamp":"2026-01-01T00:00:00Z","type":"session_meta","payload":{"id":"s1","cwd":"/tmp/project"}}"#,
            r#"{"timestamp":"2026-01-01T00:00:01Z","type":"turn_context","payload":{"model":"gpt-test"}}"#,
            r#"{"timestamp":"2026-01-01T00:00:02Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"Fix it"}]}}"#,
            r#"{"timestamp":"2026-01-01T00:00:03Z","type":"response_item","payload":{"type":"function_call","call_id":"c1","name":"shell","arguments":"{\"cmd\":\"pwd\"}"}}"#,
            r#"{"timestamp":"2026-01-01T00:00:04Z","type":"response_item","payload":{"type":"function_call_output","call_id":"c1","output":"/tmp/project"}}"#,
            r#"{"timestamp":"2026-01-01T00:00:05Z","type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"Done"}]}}"#,
            r#"{"timestamp":"2026-01-01T00:00:06Z","type":"event_msg","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":10,"cached_input_tokens":2,"output_tokens":3,"reasoning_output_tokens":4}}}}"#,
        ]);
        assert_eq!(
            facts
                .iter()
                .filter(|fact| matches!(fact, Fact::Session(_)))
                .count(),
            2
        );
        assert_eq!(
            facts
                .iter()
                .filter(|fact| matches!(fact, Fact::Message(_)))
                .count(),
            4
        );
        let usage = facts
            .iter()
            .find_map(|fact| match fact {
                Fact::Usage(usage) => Some(usage),
                _ => None,
            })
            .unwrap();
        assert_eq!(usage.accounting, UsageAccounting::Snapshot);
        assert_eq!(usage.scope, UsageScope::Turn);
        assert_eq!(usage.values.input_tokens, 10);
        assert_eq!(usage.values.output_tokens, 7);
        assert_eq!(usage.values.cache_read_tokens, 2);
    }

    #[test]
    fn total_only_usage_remains_session_scoped_cumulative() {
        let facts = decode_sequence(&[
            r#"{"type":"session_meta","payload":{"id":"s1","cwd":"/tmp/project"}}"#,
            r#"{"type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":100,"output_tokens":25}}}}"#,
        ]);
        let usage = facts
            .iter()
            .find_map(|fact| match fact {
                Fact::Usage(usage) => Some(usage),
                _ => None,
            })
            .unwrap();
        assert_eq!(usage.accounting, UsageAccounting::Cumulative);
        assert_eq!(usage.scope, UsageScope::Session);
        assert_eq!(usage.subject, usage.session);
    }

    #[test]
    fn internal_rollouts_do_not_emit_canonical_facts() {
        let facts = decode_sequence(&[
            r#"{"type":"session_meta","payload":{"id":"child","session_id":"root","parent_thread_id":"root","thread_source":"subagent","cwd":"/tmp/project"}}"#,
            r#"{"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"hidden"}]}}"#,
        ]);
        assert!(facts.is_empty());
    }

    #[test]
    fn fallback_message_identity_ignores_catalog_order_and_generation() {
        let lines = [
            r#"{"type":"session_meta","payload":{"id":"s1","cwd":"/tmp/project"}}"#,
            r#"{"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"stable"}]}}"#,
        ];
        let mut source_records = records(&lines);
        let record = source_records.pop().unwrap();
        let mut replay = record.clone();
        replay.object_id = 999;
        replay.generation = 4;
        assert_eq!(
            message_native_key("s1", "message", None, &record),
            message_native_key("s1", "message", None, &replay)
        );
    }

    #[test]
    fn bootstrap_rejects_non_rollout_objects() {
        let (adapter, _root, instance, stream) = fixture_adapter();
        let error = adapter
            .bootstrap_object(
                &instance,
                &SourceObjectDescriptor {
                    stream_id: stream.id,
                    object_key: b"bad".to_vec(),
                    relative_path: PathBuf::from("2026/01/not-a-rollout.txt"),
                },
            )
            .unwrap_err();
        assert_eq!(error.class, AdapterErrorClass::StreamFatal);
    }

    #[test]
    fn fixture_reconciles_through_common_engine_with_truthful_usage() {
        let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/small-codex/.codex");
        let temp = TempDir::new().unwrap();
        let database_path = temp.path().join("codex-observation.sqlite");
        let registry = AdapterRegistry::builder()
            .register(CodexAdapter::new())
            .build()
            .unwrap();
        let engine = SpaghettiEngineCore::open_with_registry(
            EngineOptions {
                database_path: database_path.clone(),
                query_workers: Some(1),
                owner_label: Some("codex-adapter-test".to_string()),
                defer_query_structures: false,
            },
            registry,
        )
        .unwrap();
        let first = engine
            .reconcile_adapter(ADAPTER_ID, ReconcileRequest::manual(vec![fixture.clone()]))
            .unwrap();
        assert_eq!(first.records_quarantined, 0);
        assert_eq!(first.retries_required, 0);
        assert!(first.records_decoded > 0);
        let second = engine
            .reconcile_adapter(ADAPTER_ID, ReconcileRequest::manual(vec![fixture]))
            .unwrap();
        assert_eq!(second.commits, 0, "unchanged reconcile must not write");
        engine.shutdown().unwrap();

        let connection = rusqlite::Connection::open(database_path).unwrap();
        let count = |table: &str| {
            connection
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                    row.get::<_, i64>(0)
                })
                .unwrap()
        };
        assert_eq!(count("canonical_sessions"), 10);
        assert_eq!(count("canonical_messages"), 30);
        let totals: (i64, i64, i64, i64) = connection
            .query_row(
                r#"
                SELECT SUM(exact_input_tokens), SUM(exact_output_tokens),
                       SUM(exact_cache_creation_tokens), SUM(exact_cache_read_tokens)
                FROM usage_totals
                "#,
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(totals, (1_205, 480, 0, 0));
        let repeated_turn_rows: i64 = connection
            .query_row(
                r#"
                SELECT COUNT(*) FROM usage_contributions
                WHERE accounting = 'snapshot' AND input_tokens = 45
                  AND output_tokens = 15
                "#,
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(repeated_turn_rows, 1, "last turn snapshot must replace");
    }

    #[test]
    fn unknown_record_diagnostics_flush_instead_of_exceeding_the_commit_bound() {
        let root = TempDir::new().unwrap();
        let day = root.path().join("sessions/2026/01/01");
        fs::create_dir_all(&day).unwrap();
        let mut lines = Vec::with_capacity(300);
        for index in 0..300 {
            lines.push(format!(
                r#"{{"type":"not_a_codex_kind","timestamp":"2026-01-01T00:00:00.{index:03}Z"}}"#
            ));
        }
        fs::write(
            day.join("rollout-diagnostics.jsonl"),
            lines.join("\n") + "\n",
        )
        .unwrap();

        let temp = TempDir::new().unwrap();
        let database_path = temp.path().join("codex-diagnostics.sqlite");
        let registry = AdapterRegistry::builder()
            .register(CodexAdapter::new())
            .build()
            .unwrap();
        let engine = SpaghettiEngineCore::open_with_registry(
            EngineOptions {
                database_path,
                query_workers: Some(1),
                owner_label: Some("codex-diagnostic-overflow-test".to_string()),
                defer_query_structures: false,
            },
            registry,
        )
        .unwrap();
        let result = engine
            .reconcile_adapter(
                ADAPTER_ID,
                ReconcileRequest::manual(vec![root.path().to_path_buf()]),
            )
            .expect("diagnostic overflow must split commits instead of failing reconcile");
        assert_eq!(result.retries_required, 0);
        assert_eq!(result.records_decoded, 300);
        assert!(
            result.commits >= 2,
            "300 unknown Codex records must not fit in one 256-diagnostic commit, got {}",
            result.commits
        );
        engine.shutdown().unwrap();
    }
}
